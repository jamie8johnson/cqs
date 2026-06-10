//! `BatchContext`: the daemon/stdin session's shared Store, lazily-loaded
//! Embedder, vector index, and per-name reference cache, plus staleness
//! invalidation and context construction.
//!
//! Split out of the former monolithic `cli/batch/mod.rs` (issue #1691).
//! Shared wire types, JSON helpers, and `BatchView` live in sibling modules
//! and are reached via `use super::*`.

use super::*;

// ─── Data-version probe ──────────────────────────────────────────────────────

/// Long-lived `PRAGMA data_version` probe connection.
///
/// `data_version` is a per-connection counter that SQLite bumps when *another*
/// connection (including one in the same process — e.g. the watch loop's
/// read-write Store) commits a change to the database. It moves on WAL commits
/// that never touch the main `index.db` file, which is exactly the blind spot
/// of the `DbFileIdentity` (inode/size/mtime) check: under WAL, incremental
/// reindex writes land in `index.db-wal` and the main file's identity is
/// unchanged until checkpoint.
///
/// The classic pitfall: the counter is only meaningful when queried repeatedly
/// on the SAME connection — a fresh connection per check re-baselines every
/// time and never observes a change. So the connection here must live as long
/// as the `BatchContext` (and be re-opened when `index.db` is replaced via
/// rename-over, since the old fd then points at the orphaned inode and its
/// data_version never moves again).
pub(super) struct DataVersionProbe {
    conn: sqlx::SqliteConnection,
    /// Last observed `PRAGMA data_version` value on `conn`.
    last: i64,
}

// ─── BatchContext ────────────────────────────────────────────────────────────

/// Shared resources for a batch session.
///
/// Store is opened once. Embedder and vector index are lazily initialized on
/// first use and cached for the session. References are cached per-name.
///
/// The CAGRA/HNSW index is held for the full session lifetime; this is
/// intentional. Rebuilding between commands would add seconds of latency.
/// VRAM cost: ~3-4 KB per vector (768-1024 dim × 4 bytes, depending on model), so 100k chunks ≈ 300 MB.
///
/// # Cache invalidation
///
/// **Stable caches** (embedder, reranker, config, audit_state) use `OnceLock`
/// and live for the session. ONNX sessions are cleared after idle timeout.
///
/// **Mutable caches** (hnsw, call_graph, test_chunks, file_set, notes_cache)
/// use `RefCell<Option<T>>` and are auto-invalidated when index.db changes —
/// detected via file identity (inode/size/mtime) OR `PRAGMA data_version` on
/// a long-lived probe connection (the latter catches WAL-mode incremental
/// writes that never touch the main file; see [`Self::check_index_staleness`]).
/// This detects concurrent `cqs index` runs and watch-loop reindexes during
/// long daemon / `cqs chat` sessions. On invalidation, the Store is also
/// re-opened since it has its own internal `OnceLock` caches
/// (call_graph_cache, test_chunks_cache).
///
/// Manual invalidation is available via the `refresh` batch command.
pub(crate) struct BatchContext {
    // The store is wrapped in `Mutex<Arc<...>>` so `checkout_view` can clone
    // the Arc under a brief critical section and hand it to handlers without
    // holding the outer BatchContext mutex across dispatch. Mutex (not RwLock)
    // is correct: the store is *swapped* on `check_index_staleness` re-open,
    // never read concurrently with the swap — a Mutex is the cheapest
    // correctness shape.
    //
    // Typestate: BatchContext is the daemon's shared store, which only ever
    // dispatches read-only queries (daemon handlers never mutate). The compiler
    // refuses to call a write method on a `Store<ReadOnly>`, so write-on-
    // read-store runtime errors are structurally impossible on this path.
    pub(super) store: Mutex<Arc<Store<ReadOnly>>>,
    /// The tokio runtime driving `store`. Held here so `invalidate()` and
    /// `check_index_staleness()` re-open the store on the same runtime —
    /// otherwise they would rebuild a fresh current-thread runtime on every
    /// index swap and drift apart from the daemon's shared pool.
    pub(super) runtime: Arc<tokio::runtime::Runtime>,
    // Stable caches — OnceLock (not index-derived)
    //
    // `OnceLock<Arc<Embedder>>` so the watch outer scope can hand the same
    // Embedder instance down into the daemon thread — without sharing, the
    // BatchContext and the watch loop would each hold a ~500 MB ONNX session.
    // `BatchContext::adopt_embedder` installs a pre-built Arc after
    // `BatchContext::new`; the CLI path creates a fresh one lazily via `warm`.
    //
    // Wrapped in `Arc<...>` so `BatchView` can carry a clone of the same
    // `OnceLock`; init through the view propagates to BatchContext and any
    // other view sharing the Arc.
    pub(super) embedder: Arc<OnceLock<Arc<Embedder>>>,
    /// `RefCell<Option<CachedReload<Config>>>` so a `.cqs/config.toml` edit
    /// shows up after `CONFIG_RELOAD_INTERVAL` (default 5 min) without a daemon
    /// restart. The reload is a sub-ms file read; cost is negligible per query.
    pub(super) config: RefCell<Option<CachedReload<cqs::config::Config>>>,
    /// `Arc<OnceLock<...>>` so views share one slot with the BatchContext. The
    /// inner type is `Arc<dyn cqs::Reranker>` so the trait object can be
    /// swapped at construction time (NoopReranker for ablation, future
    /// LlmReranker for batch eval, etc.) without touching the cache surface.
    pub(super) reranker: Arc<OnceLock<Arc<dyn cqs::Reranker>>>,
    /// `RefCell<Option<CachedReload<AuditMode>>>` so the 30-min audit
    /// auto-expire fires while the daemon is up. Reloads from
    /// `.cqs/audit-mode.json` every `AUDIT_STATE_RELOAD_INTERVAL` (default
    /// 30 s); the file carries its own embedded `expires_at` so the load
    /// itself respects expiration.
    pub(super) audit_state: RefCell<Option<CachedReload<cqs::audit::AuditMode>>>,
    // Mutable caches — RefCell<Option<T>> for invalidation on index change
    pub(super) hnsw: RefCell<Option<Arc<dyn VectorIndex>>>,
    pub(super) base_hnsw: RefCell<Option<Arc<dyn VectorIndex>>>,
    pub(super) call_graph: RefCell<Option<Arc<cqs::store::CallGraph>>>,
    pub(super) test_chunks: RefCell<Option<Arc<Vec<cqs::store::ChunkSummary>>>>,
    /// Cache returns `Arc<HashSet<PathBuf>>` so callers don't clone the full
    /// set on every invocation. Mirrors `call_graph` / `test_chunks`.
    pub(super) file_set: RefCell<Option<Arc<HashSet<PathBuf>>>>,
    /// Cached notes returned as `Arc<Vec<Note>>` so callers don't clone the
    /// full Vec on every dispatch. Mirrors `call_graph` / `test_chunks` /
    /// `file_set`.
    pub(super) notes_cache: RefCell<Option<Arc<Vec<cqs::note::Note>>>>,
    // LRU caps at 2 — each ReferenceIndex holds Store + HNSW (50-200MB).
    // Values are `Arc` so `get_all_refs` can fan out refs to parallel
    // `--include-refs` searches without cloning the index bytes.
    //
    // `Arc<Mutex<LruCache<...>>>` so BatchView can carry a clone of the same
    // Arc and `get_all_refs` / `get_ref` work on the snapshot path without
    // re-acquiring the outer BatchContext mutex.
    pub(super) refs: Arc<Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>>,
    /// `Arc<OnceLock<...>>` mirrors the embedder pattern — see field doc above.
    pub(super) splade_encoder: Arc<OnceLock<Option<cqs::splade::SpladeEncoder>>>,
    /// `Arc<Mutex<Option<Arc<SpladeIndex>>>>` so BatchView can carry an Arc
    /// clone of the cell and `ensure_splade_index` can populate it from either
    /// the BatchContext path or the view path. The SPLADE rebuild path replaces
    /// the inner `Arc<SpladeIndex>`; existing readers that already cloned the
    /// previous Arc keep their snapshot until the next dispatch.
    pub(super) splade_index: Arc<Mutex<Option<Arc<cqs::splade::index::SpladeIndex>>>>,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    pub model_config: cqs::embedder::ModelConfig,
    /// Last-seen identity (inode + size + mtime on unix; size + mtime
    /// elsewhere) of index.db, used to detect concurrent index updates.
    ///
    /// WSL NTFS has 1-s mtime resolution, so a fast `cqs index --force` plus a
    /// daemon query burst could share the same mtime bucket and keep serving
    /// results from the orphaned inode. `DbFileIdentity` mixes in inode + size
    /// so sub-second replacements still register.
    pub(super) index_id: Cell<Option<DbFileIdentity>>,
    /// Second staleness discriminator: a long-lived `PRAGMA data_version`
    /// probe connection (see [`DataVersionProbe`]). Catches WAL-mode
    /// incremental writes (watch loop → `index.db-wal`) that leave the main
    /// file's identity untouched until checkpoint — the false-negative class
    /// `index_id` alone cannot see (DS-V1.40-1 / #1714).
    ///
    /// `None` when the probe couldn't be opened (warned, identity-only
    /// fallback); re-opened lazily on the next staleness check.
    pub(super) data_version_probe: RefCell<Option<DataVersionProbe>>,
    /// When the staleness check last ran. Used to rate-limit `fs::metadata`
    /// on `index.db` — see [`STALENESS_CHECK_INTERVAL`].
    pub(super) last_staleness_check: Cell<Option<Instant>>,
    /// `Arc<AtomicU64>` so `BatchView` carries a cheap clone of the counter
    /// handle and handlers can read/bump without re-locking the outer
    /// BatchContext mutex. The atomicity is the load-bearing invariant; the
    /// Arc just lets the view participate.
    pub(crate) error_count: Arc<AtomicU64>,
    /// Tracks when the last command was processed.
    /// Used to clear ONNX sessions (embedder, reranker) after idle timeout.
    pub(super) last_command_time: Cell<Instant>,
    /// Wall-clock instant when this `BatchContext` was constructed.
    ///
    /// Surfaces `uptime_secs` for `cqs ping`. Held as `Instant` rather than
    /// `SystemTime` so it's monotonic — daylight-savings or `ntpd` slewing
    /// won't cause a sudden negative uptime.
    pub(super) started_at: Instant,
    /// Cumulative number of socket / stdin queries this `BatchContext` has
    /// dispatched. Bumped so both the daemon socket path and the `cqs batch`
    /// stdin path increment the same counter. Read by the `ping` handler.
    /// `Arc<AtomicU64>` for the same reason as `error_count`.
    pub(crate) query_count: Arc<AtomicU64>,
    /// Shared snapshot of watch-loop freshness state. Default is the `unknown`
    /// snapshot — a `cqs status --watch-fresh` against a `cqs batch` (no watch
    /// loop) gets `state: unknown` and an empty counter set. Inside `cqs watch
    /// --serve`, the watch loop clones this Arc and writes a fresh snapshot
    /// every cycle; the daemon's `dispatch_status` handler reads through it.
    /// The `RwLock` cost is trivial — one writer at 100 ms cadence, readers on
    /// the daemon thread that snapshot-and-drop in microseconds.
    pub(crate) watch_snapshot: cqs::watch_status::SharedWatchSnapshot,
    /// Shared one-shot signal. The daemon's `dispatch_reconcile` handler flips
    /// this `true` when a `cqs hook fire` client posts a `reconcile` socket
    /// message; the watch loop observes the flip on its next 100 ms cycle and
    /// runs an immediate reconcile pass (bypassing the periodic-tick idle
    /// gating). Default is a fresh `Arc<AtomicBool>` with no listener — outside
    /// `cqs watch --serve`, dispatching `reconcile` is a no-op rather than an
    /// error.
    pub(crate) reconcile_signal: cqs::watch_status::SharedReconcileSignal,
    /// Event-driven freshness wake-up. The watch loop's
    /// `publish_watch_snapshot` calls `set_fresh` every cycle; the daemon's
    /// `wait_fresh` handler parks on `wait_until_fresh` until the state flips.
    /// Default outside `cqs watch --serve` is a fresh notifier whose `is_fresh`
    /// flag stays `false` forever — a stray `wait_fresh` request without an
    /// active watch loop hits the caller's deadline naturally.
    pub(crate) fresh_notifier: cqs::watch_status::SharedFreshNotifier,
}

/// A number of `BatchContext` accessors are unreachable from non-test
/// production code because all dispatch goes through `BatchView`. They back
/// `BatchContext::build_view`, the test fixtures, and the stdin-batch
/// `BatchContext::invalidate` shortcut. The compiler's unused-method warning
/// fires on each one regardless; suppress at the impl level rather than per
/// method to avoid noise.
#[allow(dead_code)]
impl BatchContext {
    /// Construct a `BatchContext` around an already-opened read-only store.
    ///
    /// The single construction path for both the production factory
    /// (`create_context_with_runtime`) and the test fixture
    /// (`create_test_context`), so the cache/counter invariants live in one
    /// place: all lazy caches start empty, counters start at zero, and the
    /// watch-loop handles (`watch_snapshot` / `reconcile_signal` /
    /// `fresh_notifier`) default to the unwired no-op shapes — the daemon
    /// thread swaps real ones in via the `adopt_*` methods before serving.
    ///
    /// The runtime is captured from the store so the re-opens in
    /// [`Self::check_index_staleness`] and [`Self::invalidate`] stay on the
    /// same worker pool instead of spinning up a transient current-thread
    /// runtime per index swap.
    pub(super) fn new(
        store: Store<ReadOnly>,
        root: PathBuf,
        cqs_dir: PathBuf,
        model_config: cqs::embedder::ModelConfig,
        index_id: Option<DbFileIdentity>,
    ) -> Self {
        let runtime = Arc::clone(store.runtime());
        let ctx = Self {
            // Mutex<Arc<Store>> so `checkout_view` can clone the Arc out
            // cheaply.
            store: Mutex::new(Arc::new(store)),
            runtime,
            embedder: Arc::new(OnceLock::new()),
            config: RefCell::new(None),
            reranker: Arc::new(OnceLock::new()),
            audit_state: RefCell::new(None),
            hnsw: RefCell::new(None),
            base_hnsw: RefCell::new(None),
            call_graph: RefCell::new(None),
            test_chunks: RefCell::new(None),
            file_set: RefCell::new(None),
            notes_cache: RefCell::new(None),
            splade_encoder: Arc::new(OnceLock::new()),
            splade_index: Arc::new(Mutex::new(None)),
            refs: Arc::new(Mutex::new(lru::LruCache::new(refs_lru_size()))),
            root,
            cqs_dir,
            model_config,
            index_id: Cell::new(index_id),
            data_version_probe: RefCell::new(None),
            // None means the first staleness check runs unconditionally; the
            // rate-limit kicks in only after the first successful stat.
            last_staleness_check: Cell::new(None),
            error_count: Arc::new(AtomicU64::new(0)),
            last_command_time: Cell::new(Instant::now()),
            // `started_at` is captured here so `uptime_secs` in the ping
            // response measures from BatchContext creation — the meaningful
            // event for the daemon (the embedder load may be later).
            started_at: Instant::now(),
            query_count: Arc::new(AtomicU64::new(0)),
            watch_snapshot: cqs::watch_status::shared_unknown(),
            reconcile_signal: cqs::watch_status::shared_reconcile_signal(),
            fresh_notifier: cqs::watch_status::shared_fresh_notifier(),
        };
        // Baseline the data_version probe at construction (not lazily on the
        // first staleness check) so WAL commits landing between construction
        // and the first check are observed as a change rather than silently
        // absorbed into a late baseline. Failure is non-fatal — the open
        // helper warns and the check falls back to identity-only.
        ctx.rebaseline_data_version_probe(&cqs::resolve_index_db(&ctx.cqs_dir));
        ctx
    }

    /// Check idle timeout and clear ONNX sessions if enough time has passed.
    ///
    /// Call this at the start of each command. Clears embedder and reranker
    /// sessions after `CQS_BATCH_IDLE_MINUTES` (default 5) of no commands,
    /// freeing ~500MB+. Set to 0 to disable eviction entirely. Sessions
    /// re-initialize lazily on next use.
    pub(crate) fn check_idle_timeout(&self) {
        self.sweep_idle_sessions();
        self.last_command_time.set(Instant::now());
    }

    /// Clear ONNX sessions if idle too long without resetting the clock.
    ///
    /// Called both from `check_idle_timeout` (on command arrival) and from a
    /// periodic accept-loop tick (watch.rs), so a truly idle daemon still
    /// releases ~500MB+ after the idle timeout. Unlike `check_idle_timeout` it
    /// does NOT update `last_command_time`; the tick is a passive observer.
    ///
    /// ONNX timeout is configurable via `CQS_BATCH_IDLE_MINUTES` (default 5).
    /// Set to 0 to disable ONNX eviction entirely.
    ///
    /// Also clears the data caches (`hnsw`, `splade_index`, `call_graph`,
    /// `test_chunks`, `notes_cache`, `file_set`) after a longer idle window,
    /// configurable via `CQS_BATCH_DATA_IDLE_MINUTES` (default 30 min).
    /// Without this, a daemon idle for hours holds 600 MB+ of HNSW +
    /// SPLADE-index + call-graph caches that no agent is using. The split
    /// timeout (5 min ONNX, 30 min data) preserves first-query responsiveness
    /// — the next user query pays a sub-second ONNX init rather than a
    /// multi-second HNSW rebuild.
    pub(crate) fn sweep_idle_sessions(&self) {
        let timeout_minutes = idle_timeout_minutes();
        if timeout_minutes > 0 {
            let elapsed = self.last_command_time.get().elapsed();
            // saturating_mul so an operator passing
            // `CQS_BATCH_IDLE_MINUTES=u64::MAX` can't overflow into zero and
            // instant-evict sessions they meant to pin forever.
            let timeout = std::time::Duration::from_secs(timeout_minutes.saturating_mul(60));
            if elapsed >= timeout {
                if let Some(emb) = self.embedder.get() {
                    emb.clear_session();
                    tracing::info!(
                        idle_minutes = elapsed.as_secs() / 60,
                        "Cleared embedder session after idle timeout"
                    );
                }
                if let Some(rr) = self.reranker.get() {
                    rr.clear_session();
                    tracing::info!(
                        idle_minutes = elapsed.as_secs() / 60,
                        "Cleared reranker session after idle timeout"
                    );
                }
                // Also clear SPLADE encoder session
                if let Some(splade) = self.splade_encoder.get().and_then(|opt| opt.as_ref()) {
                    splade.clear_session();
                    tracing::info!(
                        idle_minutes = elapsed.as_secs() / 60,
                        "Cleared SPLADE session after idle timeout"
                    );
                }
            }
        }

        // Separate (longer) idle window for the heavyweight data caches.
        // Independent of the ONNX-session check above so an operator can
        // disable one without disabling the other.
        let data_timeout_minutes = data_cache_idle_timeout_minutes();
        if data_timeout_minutes == 0 {
            return;
        }
        let elapsed = self.last_command_time.get().elapsed();
        // Same overflow guard as the ONNX-session path above.
        let data_timeout = std::time::Duration::from_secs(data_timeout_minutes.saturating_mul(60));
        if elapsed >= data_timeout {
            tracing::info!(
                idle_minutes = elapsed.as_secs() / 60,
                "Clearing data caches (hnsw / splade_index / call_graph / test_chunks / notes / file_set) after idle timeout"
            );
            // Reuses the same try_borrow_mut-tolerant code path as the
            // index-change invalidation, so a handler holding a Ref<...>
            // mid-query simply defers the eviction to the next sweep tick.
            self.invalidate_mutable_caches();
        }
    }

    /// Check if index.db changed since last access. If so, clear all mutable
    /// caches and re-open the Store (which resets its internal OnceLock
    /// caches like call_graph_cache, test_chunks_cache).
    ///
    /// Two discriminators, either of which fires the invalidation:
    ///
    /// 1. **File identity** — `(inode, size, mtime)` on unix, `(size, mtime)`
    ///    elsewhere. Catches replacement (`cqs index --force` rename-over,
    ///    new inode even when two events share a 1-s WSL NTFS mtime bucket)
    ///    and in-place rewrites of the main file.
    /// 2. **`PRAGMA data_version`** on a long-lived probe connection. Catches
    ///    WAL-mode incremental writes: the watch loop's commits land in
    ///    `index.db-wal`, leaving the main file's identity unchanged until
    ///    checkpoint — identity alone would serve stale caches through any
    ///    number of incremental reindexes (DS-V1.40-1 / #1714).
    ///
    /// False positives cost one cache reload; false negatives are the bug,
    /// so the probe falls back to identity-only (with a warn) rather than
    /// blocking the check when it can't be opened or queried.
    ///
    /// Rate-limited to at most once per [`STALENESS_CHECK_INTERVAL`]. Every
    /// `ctx.store()` and every `vector_index` / `file_set` / etc. accessor
    /// calls this.
    pub(crate) fn check_index_staleness(&self) {
        let now = Instant::now();
        if let Some(prev) = self.last_staleness_check.get() {
            if now.duration_since(prev) < STALENESS_CHECK_INTERVAL {
                return;
            }
        }
        self.last_staleness_check.set(Some(now));

        // Slot-aware index resolution.
        let index_path = cqs::resolve_index_db(&self.cqs_dir);
        let current_id = match DbFileIdentity::from_path(&index_path) {
            Some(id) => id,
            None => {
                // If the DB becomes temporarily unstattable (permissions,
                // concurrent rebuild, NFS glitch), warn rather than silently
                // returning — a silent return would keep every subsequent
                // command in the session using stale caches forever.
                tracing::warn!(
                    path = %index_path.display(),
                    "Cannot stat index.db for staleness check — caches may remain stale"
                );
                return;
            }
        };

        let last = self.index_id.get();
        let identity_changed = last.is_some() && last != Some(current_id);
        // Only consult the probe when identity is unchanged: an identity
        // change already invalidates, and the old probe fd points at the
        // orphaned inode anyway (its data_version would never move again).
        let data_version_changed = if identity_changed {
            false
        } else {
            self.data_version_changed()
        };

        if identity_changed || data_version_changed {
            let _span = tracing::info_span!("batch_index_invalidation").entered();
            tracing::info!(
                identity_changed,
                data_version_changed,
                "index.db changed, invalidating mutable caches"
            );
            self.invalidate_mutable_caches();

            if identity_changed {
                // The probe connection still points at the replaced (deleted)
                // inode — re-open it against the new file. Re-baseline BEFORE
                // the Store re-open below: a write landing between the two is
                // then caught on the next check (false positive at worst);
                // baselining after the re-open would silently absorb it.
                self.rebaseline_data_version_probe(&index_path);
            }

            // Re-open the Store to reset its internal OnceLock caches.
            // Reuse the shared runtime so this re-open doesn't spin up a
            // transient current_thread runtime on every index swap.
            match Store::open_readonly_pooled_with_runtime(&index_path, Arc::clone(&self.runtime)) {
                Ok(new_store) => {
                    // Check if index dimension changed — OnceLock model_config
                    // can't be cleared, so warn the user to restart the batch session.
                    let new_dim = new_store.dim();
                    if new_dim != self.model_config.dim {
                        tracing::warn!(
                            old_dim = self.model_config.dim,
                            new_dim = new_dim,
                            "Index dimension changed — queries may return wrong results until batch restart"
                        );
                    }
                    // Swap the Arc inside the Mutex. Existing readers
                    // (BatchView snapshots already handed out) keep their
                    // old Arc and remain valid; the next `checkout_view`
                    // returns a snapshot pointing at the new store.
                    *self.store.lock().unwrap_or_else(|p| p.into_inner()) = Arc::new(new_store);
                    tracing::info!("Store re-opened after index change");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to re-open Store after index change");
                }
            }
        }
        self.index_id.set(Some(current_id));
    }

    /// Open a fresh probe connection against `index_path` and read its
    /// baseline `PRAGMA data_version`. Returns `None` (with a `warn!`) when
    /// the open or the query fails — staleness detection then falls back to
    /// identity-only rather than panicking or silently skipping the check.
    fn open_data_version_probe(&self, index_path: &std::path::Path) -> Option<DataVersionProbe> {
        use sqlx::ConnectOptions;
        let result = self.runtime.block_on(async {
            // Mirror the Store's read-only open shape (filename + read_only +
            // WAL) so the probe sees the same journal-mode view of the DB.
            let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(index_path)
                .read_only(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .connect()
                .await?;
            let last: i64 = sqlx::query_scalar("PRAGMA data_version")
                .fetch_one(&mut conn)
                .await?;
            Ok::<_, sqlx::Error>(DataVersionProbe { conn, last })
        });
        match result {
            Ok(probe) => Some(probe),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %index_path.display(),
                    "Failed to open data_version probe — falling back to identity-only staleness detection"
                );
                None
            }
        }
    }

    /// Query `PRAGMA data_version` on the long-lived probe connection and
    /// compare against the last observed value. Returns `true` when another
    /// connection (e.g. the watch loop's read-write Store) has committed
    /// since the previous observation — WAL or not.
    ///
    /// A missing probe (failed earlier open) is re-opened here, which
    /// establishes a fresh baseline and returns `false` for this check. A
    /// query failure warns, drops the probe so the next check re-opens it,
    /// and returns `false` (identity-only fallback — never panics).
    fn data_version_changed(&self) -> bool {
        let mut slot = self.data_version_probe.borrow_mut();
        match slot.as_mut() {
            None => {
                // Earlier open failed (or the probe was dropped after a query
                // error) — retry. Freshly baselined, so nothing to compare.
                *slot = self.open_data_version_probe(&cqs::resolve_index_db(&self.cqs_dir));
                false
            }
            Some(probe) => {
                let result = self.runtime.block_on(
                    sqlx::query_scalar::<_, i64>("PRAGMA data_version").fetch_one(&mut probe.conn),
                );
                match result {
                    Ok(v) => {
                        let changed = v != probe.last;
                        probe.last = v;
                        changed
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "data_version probe query failed — dropping probe; will re-open on next staleness check"
                        );
                        *slot = None;
                        false
                    }
                }
            }
        }
    }

    /// Drop the current probe connection (if any) and open a fresh one
    /// against `index_path`, re-baselining `data_version`. Called when
    /// `index.db` is replaced (rename-over): the old fd points at the
    /// orphaned inode, so its counter would never move again. Also called at
    /// construction and on manual `refresh`.
    fn rebaseline_data_version_probe(&self, index_path: &std::path::Path) {
        use sqlx::Connection;
        let mut slot = self.data_version_probe.borrow_mut();
        if let Some(old) = slot.take() {
            // Explicit close so sqlite finalizes the old handle now instead
            // of whenever Drop gets around to it. Best-effort — the fd is
            // dead-weight either way.
            let _ = self.runtime.block_on(old.conn.close());
        }
        *slot = self.open_data_version_probe(index_path);
    }

    /// Clear all mutable caches. Called on index identity change or manual refresh.
    ///
    /// `splade_index` must be cleared here too — otherwise a long-lived batch
    /// session that had loaded the SPLADE posting map once would serve results
    /// from the pre-reindex generation forever after a concurrent `cqs index`.
    /// Clearing the RefCell lets `ensure_splade_index` see `None` on the next
    /// call and rebuild from
    /// the freshly persisted `splade.index.bin` (or SQLite fallback).
    fn invalidate_mutable_caches(&self) {
        // Use try_borrow_mut: a search handler may still hold a Ref<...>
        // to splade_index or hnsw across an accessor call that triggers
        // staleness re-check (for example handlers/search.rs does
        // `let splade_index_ref = ctx.borrow_splade_index()` then later
        // calls `ctx.store().search_hybrid(...)`). Panicking on
        // borrow_mut() would crash the whole batch session for what is
        // just a deferral case. Slots that are busy stay populated; we
        // reset the rate-limit so the next accessor retries the
        // invalidation as soon as the in-flight handler releases its Ref.
        let mut all_clear = true;
        macro_rules! try_clear_to_none {
            ($field:expr, $name:literal) => {
                match $field.try_borrow_mut() {
                    Ok(mut g) => *g = None,
                    Err(_) => {
                        all_clear = false;
                        tracing::debug!(slot = $name, "borrow held; deferring invalidation");
                    }
                }
            };
        }
        try_clear_to_none!(self.hnsw, "hnsw");
        try_clear_to_none!(self.base_hnsw, "base_hnsw");
        try_clear_to_none!(self.call_graph, "call_graph");
        try_clear_to_none!(self.test_chunks, "test_chunks");
        try_clear_to_none!(self.file_set, "file_set");
        try_clear_to_none!(self.notes_cache, "notes_cache");
        // splade_index is `Arc<Mutex<...>>`. Use try_lock to mirror the
        // borrow-deferral semantics of the RefCell branches.
        match self.splade_index.try_lock() {
            Ok(mut g) => *g = None,
            Err(_) => {
                all_clear = false;
                tracing::debug!(slot = "splade_index", "lock held; deferring invalidation");
            }
        }
        // refs LRU is `Arc<Mutex<...>>` (shared with BatchView).
        // `try_lock` is the read-only equivalent of `try_borrow_mut`: if a
        // handler thread holds the mutex (e.g. iterating refs in a parallel
        // search), the eviction is deferred to the next sweep.
        match self.refs.try_lock() {
            Ok(mut g) => g.clear(),
            Err(_) => {
                all_clear = false;
                tracing::debug!(slot = "refs", "lock held; deferring invalidation");
            }
        }

        if !all_clear {
            // Reset rate-limit so the next accessor reattempts immediately
            // (rather than waiting STALENESS_CHECK_INTERVAL with stale caches).
            self.last_staleness_check.set(None);
            tracing::debug!("partial cache invalidation; will retry on next accessor");
        }
    }

    /// Manually invalidate all mutable caches and re-open the Store.
    /// Used by the `refresh` batch command.
    pub(crate) fn invalidate(&self) -> Result<()> {
        let _span = tracing::info_span!("batch_manual_invalidation").entered();
        self.invalidate_mutable_caches();

        // Slot-aware index resolution.
        let index_path = cqs::resolve_index_db(&self.cqs_dir);
        // Re-baseline the data_version probe before the Store re-open (same
        // ordering rationale as check_index_staleness): a write landing
        // between the two is caught on the next check instead of absorbed.
        self.rebaseline_data_version_probe(&index_path);
        // Pass the shared runtime so manual refreshes keep using the same
        // worker pool as the session they're refreshing.
        let new_store =
            Store::open_readonly_pooled_with_runtime(&index_path, Arc::clone(&self.runtime))
                .map_err(|e| anyhow::anyhow!("Failed to re-open Store: {e}"))?;
        // Swap the Arc; existing BatchView snapshots keep the old.
        *self.store.lock().unwrap_or_else(|p| p.into_inner()) = Arc::new(new_store);

        // Update identity to current so we don't immediately re-invalidate.
        if let Some(id) = DbFileIdentity::from_path(&index_path) {
            self.index_id.set(Some(id));
        }
        // Treat the manual refresh as a fresh staleness check so the next
        // batch command hits the rate-limit fast path.
        self.last_staleness_check.set(Some(Instant::now()));

        tracing::info!("Manual cache invalidation complete");
        Ok(())
    }

    /// Dispatch a single command line (e.g. "search foo -n 5 --json") and
    /// write the JSON result to `out`. Used by the daemon socket handler.
    ///
    /// Every line that reaches the dispatcher bumps `query_count` (so the ping
    /// handler can report total queries served), and any parse / dispatch
    /// failure bumps `error_count` so the `cmd_batch` stdin loop and the daemon
    /// socket handler converge on the same counter.
    ///
    /// The daemon socket path calls [`Self::dispatch_tokens`] directly
    /// (skipping the shell round-trip), and the `cmd_batch` stdin loop does its
    /// own tokenization. `dispatch_line` is retained for tests and any future
    /// stdin-style surface that needs shell parsing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn dispatch_line(&self, line: &str, out: &mut impl std::io::Write) {
        use crate::cli::json_envelope::error_codes;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        let tokens = match shell_words::split(trimmed) {
            Ok(t) => t,
            Err(e) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                let msg = format!("Parse error: {e}");
                tracing::warn!(code = error_codes::PARSE_ERROR, error = %msg, "Batch dispatch_line: tokenization failed");
                let _ = write_envelope_error(out, error_codes::PARSE_ERROR, &msg);
                return;
            }
        };
        if tokens.is_empty() {
            return;
        }
        self.dispatch_parsed_tokens(&tokens, out);
    }

    /// Dispatch pre-tokenized `(command, args)` directly, skipping the
    /// `shell_words::join` / `shell_words::split` round-trip that
    /// `dispatch_line` requires for the stdin surface.
    ///
    /// The daemon socket path already receives `{ "command": "...", "args":
    /// [...] }` as parsed JSON — round-tripping that through a shell-quoted
    /// string is wasted work on every daemon query and a latent correctness
    /// bug on tokens containing shell metacharacters. This entry point takes
    /// the tokens directly and still shares the NUL check, counter bumps,
    /// and dispatch body with `dispatch_line`.
    pub(crate) fn dispatch_tokens(
        &self,
        command: &str,
        args: &[String],
        out: &mut impl std::io::Write,
    ) {
        if command.is_empty() {
            return;
        }
        let tokens: Vec<String> = std::iter::once(command.to_string())
            .chain(args.iter().cloned())
            .collect();
        self.dispatch_parsed_tokens(&tokens, out);
    }

    /// Shared dispatch body for both `dispatch_line` (stdin surface) and
    /// `dispatch_tokens` (daemon socket surface). The only difference between
    /// the two callers is how they reached a non-empty `tokens` vec — NUL
    /// check, counter bumps, clap parse, and handler dispatch are identical.
    ///
    /// Takes a snapshot via `build_view` and delegates to
    /// [`dispatch_via_view`]. Stdin batch holds no shared `Arc<Mutex>` so
    /// the view's `outer_lock` is `None`; refresh inside this path goes
    /// through `BatchContext::invalidate` directly.
    fn dispatch_parsed_tokens(&self, tokens: &[String], out: &mut impl std::io::Write) {
        use crate::cli::json_envelope::error_codes;
        // NUL byte check parity with the daemon socket loop in cmd_batch.
        // Both surfaces share downstream handlers; they must share input
        // validation too.
        if let Err(msg) = reject_null_tokens(tokens) {
            self.error_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                code = error_codes::INVALID_INPUT,
                error = msg,
                "Batch dispatch: NUL byte in tokens"
            );
            let _ = write_envelope_error(out, error_codes::INVALID_INPUT, msg);
            return;
        }
        self.check_idle_timeout();
        // Bump query_count *after* the early returns above (empty input is
        // not a query). Counts both successes and errors — symmetric with
        // how `total_queries` is described in PingResponse.
        self.query_count.fetch_add(1, Ordering::Relaxed);
        let view = self.build_view(None);
        // Refresh special-case: stdin batch reaches Refresh through the same
        // path as the daemon, but invalidate must run on `&self`. Detect
        // upfront and call directly.
        match commands::BatchInput::try_parse_from(tokens) {
            Ok(input) => {
                if matches!(input.cmd, commands::BatchCmd::Refresh) {
                    match self.invalidate() {
                        Ok(()) => {
                            let _ = write_json_line(
                                out,
                                &serde_json::json!({
                                    "status": "ok",
                                    "message": "Caches invalidated, Store re-opened",
                                }),
                            );
                        }
                        Err(e) => {
                            self.error_count.fetch_add(1, Ordering::Relaxed);
                            let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                            let _ = write_envelope_error(out, code.as_str(), &msg);
                        }
                    }
                    return;
                }
                match commands::dispatch(&view, input.cmd) {
                    Ok(value) => {
                        let _ = write_json_line(out, &value);
                    }
                    Err(e) => {
                        self.error_count.fetch_add(1, Ordering::Relaxed);
                        // redact_error walks the source chain and emits a stable
                        // (code, message) pair instead of echoing the raw anyhow
                        // chain (which can carry HTTP bodies, sqlite query text,
                        // filesystem paths). The full unredacted chain is logged
                        // via tracing::warn! inside redact_error so an operator
                        // can correlate by chain-id.
                        let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                        let _ = write_envelope_error(out, code.as_str(), &msg);
                    }
                }
            }
            Err(e) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                // Parse errors come from user-supplied tokens — they're
                // safe to surface verbatim and useful for the agent to
                // correct its query. No redaction needed.
                let msg = format!("{e:#}");
                tracing::warn!(code = error_codes::PARSE_ERROR, error = %msg, "Batch dispatch: clap parse failed");
                let _ = write_envelope_error(out, error_codes::PARSE_ERROR, &msg);
            }
        }
    }

    /// Build a [`cqs::daemon_translate::PingResponse`] snapshot of the
    /// daemon's current state.
    ///
    /// Pure read-side helper — bumps no counters, blocks no
    /// I/O, takes no locks. The `splade_loaded` / `reranker_loaded`
    /// flags peek the `OnceLock`s without forcing a load, so calling
    /// `ping` does not warm any ONNX session that wasn't already
    /// resident. `last_indexed_at` reads `index.db`'s mtime as the
    /// best available signal for "when did the index last change"; a
    /// missing file or unreadable metadata yields `None` rather than
    /// failing the whole ping.
    pub(crate) fn ping_snapshot(&self) -> cqs::daemon_translate::PingResponse {
        // Surface overflow as None (treated same as "missing mtime") instead
        // of silently wrapping past `i64::MAX`. Different shape from
        // `unix_secs_i64()` — reads file mtime, not wall-clock.
        // Slot-aware index resolution.
        let last_indexed_at = std::fs::metadata(cqs::resolve_index_db(&self.cqs_dir))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok());
        // SPLADE encoder slot is `OnceLock<Option<...>>`: only "loaded" if
        // the inner Option is Some. A user with no SPLADE model configured
        // populates the OnceLock with None on first sparse query; that's
        // not a "loaded" model from the operator's POV.
        let splade_loaded = self
            .splade_encoder
            .get()
            .map(|opt| opt.is_some())
            .unwrap_or(false);
        cqs::daemon_translate::PingResponse {
            model: self.model_config.name.clone(),
            // Cast usize→u32. Real models are 384 / 768 / 1024 dim; clamp
            // to u32::MAX rather than wrap if a future custom config goes
            // pathological. The cast is information-preserving in practice.
            dim: u32::try_from(self.model_config.dim).unwrap_or(u32::MAX),
            uptime_secs: self.started_at.elapsed().as_secs(),
            last_indexed_at,
            error_count: self.error_count.load(Ordering::Relaxed),
            total_queries: self.query_count.load(Ordering::Relaxed),
            splade_loaded,
            reranker_loaded: self.reranker.get().is_some(),
        }
    }

    /// Snapshot the Store as a refcounted `Arc`, checking for index staleness
    /// first.
    ///
    /// The store is held in `Mutex<Arc<Store<ReadOnly>>>`; this accessor takes
    /// the mutex briefly, clones the Arc, and drops the lock — handlers hold a
    /// stable snapshot for as long as they need it without keeping any
    /// BatchContext lock acquired.
    pub fn store(&self) -> Arc<Store<ReadOnly>> {
        self.check_index_staleness();
        let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(&guard)
    }

    /// Pre-warm the embedder so the first query doesn't pay the ~500ms ONNX init.
    /// Called once at session start. Errors are logged but non-fatal.
    ///
    /// If the watch outer scope installed a shared Embedder via
    /// `adopt_embedder`, the OnceLock is already populated and this is a no-op
    /// for model loading (cache eviction still runs).
    pub fn warm(&self) {
        if self.embedder.get().is_none() {
            let _span = tracing::info_span!("batch_warm").entered();
            match Embedder::new(self.model_config.clone()) {
                Ok(e) => {
                    let _ = self.embedder.set(std::sync::Arc::new(e));
                    tracing::info!("Embedder pre-warmed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Embedder warm failed — will retry on first query");
                }
            }
        }
        // Evict the project's embeddings cache at daemon startup. Otherwise a
        // long-lived daemon / watch session on a machine that never runs a
        // manual index can grow the cache past the `CQS_CACHE_MAX_SIZE` cap
        // (default 10 GB) without ever trimming — the full `cqs index` pipeline
        // is the only other evictor. A single post-warm eviction lets the
        // daemon self-heal on boot.
        //
        // The cache is project-scoped at `<project>/.cqs/embeddings_cache.db`,
        // so resolve the path against the daemon's project root via
        // `resolve_index_dir(&self.root)`.
        //
        // Reuse the batch context's runtime so this one-shot open doesn't spawn
        // a fresh current_thread runtime.
        let project_cqs_dir = cqs::resolve_index_dir(&self.root);
        let cache_path = cqs::cache::EmbeddingCache::project_default_path(&project_cqs_dir);
        evict_embeddings_cache_with_runtime(
            &cache_path,
            "daemon startup",
            Some(std::sync::Arc::clone(&self.runtime)),
        );
    }

    /// Install a shared Embedder from the outer watch scope.
    ///
    /// Returns `true` if the Arc was installed, `false` if the OnceLock was
    /// already populated (lazy init already happened, or another caller won
    /// the race). The caller can use this result to decide whether to fall
    /// back to its own lazily-initialized embedder.
    pub fn adopt_embedder(&self, shared: std::sync::Arc<Embedder>) -> bool {
        self.embedder.set(shared).is_ok()
    }

    /// Install a shared `Arc<RwLock<WatchSnapshot>>` from the outer
    /// `cmd_watch` scope. Replaces the constructor's default `unknown`
    /// handle with one the watch loop also holds, so subsequent
    /// `watch_snapshot()` reads see the loop's most-recent publish.
    ///
    /// Takes `&mut self` so the field can be replaced cleanly; the
    /// daemon thread binds `let mut ctx = create_context_with_runtime(..)`
    /// for exactly this swap before wrapping the ctx in `Arc<Mutex<...>>`.
    pub fn adopt_watch_snapshot(&mut self, shared: cqs::watch_status::SharedWatchSnapshot) {
        self.watch_snapshot = shared;
    }

    /// Install the shared reconcile-signal handle. Called from the daemon
    /// thread before lock-wrapping the `BatchContext`, so `dispatch_reconcile`
    /// flips a flag the watch loop is actually watching.
    ///
    /// Outside `cqs watch --serve`, this is never called and the field
    /// stays at the no-op default (no listener picks it up).
    pub fn adopt_reconcile_signal(&mut self, shared: cqs::watch_status::SharedReconcileSignal) {
        self.reconcile_signal = shared;
    }

    /// Install the shared `FreshNotifier`. Called from the daemon thread
    /// alongside `adopt_watch_snapshot` so the `wait_fresh` handler parks on
    /// the same notifier the watch loop updates from `publish_watch_snapshot`.
    pub fn adopt_fresh_notifier(&mut self, shared: cqs::watch_status::SharedFreshNotifier) {
        self.fresh_notifier = shared;
    }

    /// Get or create the embedder (~500ms first call).
    pub fn embedder(&self) -> Result<&Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e.as_ref());
        }
        let _span = tracing::info_span!("batch_embedder_init").entered();
        let e = Embedder::new(self.model_config.clone())?;
        // Race is fine — OnceLock ensures only one value is stored
        let _ = self.embedder.set(std::sync::Arc::new(e));
        Ok(self
            .embedder
            .get()
            .map(|arc| arc.as_ref())
            .expect("embedder OnceLock populated by set() above"))
    }

    /// Get or lazily load the SPLADE encoder. Returns None if model unavailable.
    ///
    /// Path resolution is delegated to [`cqs::splade::resolve_splade_model_dir`]
    /// so the env-var override (`CQS_SPLADE_MODEL`) and vocab-mismatch probe
    /// stay consistent across the interactive (`cqs query`) and batch
    /// (`cqs search`) paths.
    pub fn splade_encoder(&self) -> Option<&cqs::splade::SpladeEncoder> {
        let opt = self.splade_encoder.get_or_init(|| {
            let model_dir = cqs::splade::resolve_splade_model_dir()?;
            match cqs::splade::SpladeEncoder::new(
                &model_dir,
                cqs::splade::SpladeEncoder::default_threshold(),
            ) {
                Ok(enc) => Some(enc),
                Err(e) => {
                    tracing::warn!(
                        path = %model_dir.display(),
                        error = %e,
                        "SPLADE encoder unavailable in batch mode"
                    );
                    None
                }
            }
        });
        opt.as_ref()
    }

    /// Ensure SPLADE index is loaded, then borrow it.
    /// Call `ensure_splade_index()` first, then `borrow_splade_index()`.
    ///
    /// Uses the same persist-and-load path as the single-shot CLI: tries
    /// `splade.index.bin` first, falls back to SQLite rebuild + persist if
    /// the file is absent, stale, or corrupt. Staleness is detected via
    /// the `splade_generation` metadata counter. If the generation cannot
    /// be read at all, this returns without populating the RefCell — falling
    /// through with `0` would let a later persist write a gen-0 file whose
    /// header lies about the DB state, creating a self-perpetuating
    /// cache-poison loop.
    pub fn ensure_splade_index(&self) {
        self.check_index_staleness();
        if self
            .splade_index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            return;
        }
        let generation = match self.store().splade_generation() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to read splade_generation — skipping SPLADE entirely for this \
                     batch session; search will fall back to dense-only"
                );
                return;
            }
        };
        let splade_path = self.cqs_dir.join(cqs::splade::index::SPLADE_INDEX_FILENAME);
        let store = self.store();
        // Time the build so operators can diagnose first-query latency spikes
        // after a reindex. Full rebuild on a 200k-chunk repo with SPLADE-Code
        // 0.6B takes ~45 s. The `rebuilt` flag comes back from `load_or_build`
        // so we can split the log into a cheap cache hit vs a visible rebuild.
        let build_start = Instant::now();
        let (idx, rebuilt) = cqs::splade::index::SpladeIndex::load_or_build(
            &splade_path,
            generation,
            || match store.load_all_sparse_vectors() {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load sparse vectors, falling back to cosine-only"
                    );
                    Vec::new()
                }
            },
        );
        let build_ms = build_start.elapsed().as_millis() as u64;
        if idx.is_empty() {
            // no vectors — cosine-only; leave the RefCell as None
            return;
        }
        if rebuilt {
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                rebuild_ms = build_ms,
                "SPLADE index rebuilt from SQLite (batch)"
            );
            // Surface very-long rebuilds at warn so operators notice. Empirical
            // threshold: 10 s on a 200k-chunk SPLADE-Code index is already
            // "user waited visibly"; 30 s is "probably a problem."
            if build_ms > 30_000 {
                tracing::warn!(
                    rebuild_ms = build_ms,
                    chunks = idx.len(),
                    "SPLADE index rebuild exceeded 30s — first daemon query after \
                     reindex will have been blocked this long"
                );
            }
        } else {
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                load_ms = build_ms,
                "SPLADE index loaded from disk (batch)"
            );
        }
        *self.splade_index.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(idx));
    }

    /// Snapshot the cached SPLADE index as an `Option<Arc<SpladeIndex>>`.
    ///
    /// Returns an Arc clone rather than a borrow, so search handlers can run
    /// outside any BatchContext borrow scope.
    pub fn borrow_splade_index(&self) -> Option<Arc<cqs::splade::index::SpladeIndex>> {
        self.splade_index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// Get or build the vector index (CAGRA/HNSW/brute-force, cached).
    ///
    /// Checks index staleness before returning cached value. If the index.db
    /// changed, rebuilds the vector index from the fresh Store.
    /// Returns a cloneable Arc so callers can hold a reference past RefCell borrow scope.
    ///
    /// If the cached index reports `is_poisoned()` (only the CAGRA GPU backend
    /// currently does), the cache slot is cleared and a fresh index is built.
    /// Reusing a poisoned CUDA context risks double-free and CUDA faults.
    pub fn vector_index(&self) -> Result<Option<std::sync::Arc<dyn VectorIndex>>> {
        self.check_index_staleness();
        {
            let cached = self.hnsw.borrow();
            if let Some(arc) = cached.as_ref() {
                if arc.is_poisoned() {
                    tracing::warn!(
                        name = arc.name(),
                        "Cached vector index is poisoned — discarding and rebuilding"
                    );
                } else {
                    return Ok(Some(std::sync::Arc::clone(arc)));
                }
            }
        }
        // Clear any poisoned cache before rebuilding.
        *self.hnsw.borrow_mut() = None;
        let _span = tracing::info_span!("batch_vector_index_init").entered();
        // Pull a snapshot Arc and pass `&Store<...>` via auto-deref.
        let store = self.store_arc_locked();
        let idx = build_vector_index(&store, &self.cqs_dir, self.config().ef_search)?;
        let result = idx.map(|boxed| -> Arc<dyn VectorIndex> { boxed.into() });
        let ret = result.clone();
        *self.hnsw.borrow_mut() = result;
        Ok(ret)
    }

    /// Get or build the base (non-enriched) vector index, cached.
    /// Returns `None` if the base index files don't exist or `CQS_DISABLE_BASE_INDEX=1`.
    pub fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        self.check_index_staleness();
        {
            let cached = self.base_hnsw.borrow();
            if let Some(arc) = cached.as_ref() {
                return Ok(Some(Arc::clone(arc)));
            }
        }
        let _span = tracing::info_span!("batch_base_vector_index_init").entered();
        let store = self.store_arc_locked();
        let idx = crate::cli::build_base_vector_index(&store, &self.cqs_dir)?;
        let result = idx.map(|boxed| -> Arc<dyn VectorIndex> { boxed.into() });
        let ret = result.clone();
        *self.base_hnsw.borrow_mut() = result;
        Ok(ret)
    }

    /// Get a cached reference index by name, loading on first access.
    ///
    /// Uses cached config and loads only the target reference, not all
    /// references.
    ///
    /// Before serving a cached entry, peek its `is_stale()` so a concurrent
    /// `cqs ref update <name>` (which rewrites the reference's `index.db`
    /// without touching the primary project's `.cqs/index.db`) forces a fresh
    /// load. Without this, a long-lived daemon would keep serving results from
    /// a closed WAL snapshot / stale HNSW bytes for days.
    pub fn get_ref(&self, name: &str) -> Result<()> {
        get_ref_via_refs_lru(&self.refs, &self.config(), name)
    }

    /// Return every configured reference as a shared `Arc`, populating the
    /// LRU cache on miss. Amortizes Store+HNSW loads across a daemon session —
    /// without this, each `--include-refs` query would rebuild every reference
    /// from scratch.
    ///
    /// Staleness is honored: a cached reference whose `index.db`
    /// identity changed (concurrent `cqs ref update <name>`) is evicted
    /// and reloaded.
    ///
    /// The LRU size cap still applies — references not in the cache at
    /// the moment of the call are loaded; if total configured refs
    /// exceed the cap, the oldest cached entries churn, but within a
    /// single call the returned `Vec` holds strong `Arc`s so eviction
    /// cannot race with in-flight searches.
    pub fn get_all_refs(&self) -> Result<Vec<Arc<ReferenceIndex>>> {
        get_all_refs_via_refs_lru(&self.refs, &self.config())
    }

    /// Get or build the file set for staleness checks (cached).
    ///
    /// Returns `Arc<HashSet<PathBuf>>` so callers don't clone the full set per
    /// invocation. Mirrors `call_graph` / `test_chunks`.
    pub(super) fn file_set(&self) -> Result<std::sync::Arc<HashSet<PathBuf>>> {
        self.check_index_staleness();
        {
            let cached = self.file_set.borrow();
            if let Some(fs) = cached.as_ref() {
                return Ok(std::sync::Arc::clone(fs));
            }
        }
        let _span = tracing::info_span!("batch_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        let arc = std::sync::Arc::new(set);
        *self.file_set.borrow_mut() = Some(std::sync::Arc::clone(&arc));
        Ok(arc)
    }

    /// Get cached audit state. Reloads from `.cqs/audit-mode.json` when the
    /// cached value is older than [`AUDIT_STATE_RELOAD_INTERVAL`] (default
    /// 30 s), then returns an owned snapshot.
    ///
    /// The file is sub-ms to read; the 30 s interval bounds staleness while
    /// keeping accessor cost negligible. Returning owned `AuditMode` (rather
    /// than `&AuditMode` from a borrow) lets the `let audit = ctx.audit_state();
    /// &audit` call-site pattern work without juggling `Ref<'_, ...>` lifetimes.
    pub(super) fn audit_state(&self) -> cqs::audit::AuditMode {
        let needs_reload = match self.audit_state.borrow().as_ref() {
            Some(c) => c.loaded_at.elapsed() >= AUDIT_STATE_RELOAD_INTERVAL,
            None => true,
        };
        if needs_reload {
            let fresh = cqs::audit::load_audit_state(&self.cqs_dir);
            *self.audit_state.borrow_mut() = Some(CachedReload {
                value: fresh,
                loaded_at: Instant::now(),
            });
        }
        // Clone the cached value for the caller. AuditMode is small (bool +
        // Option<DateTime>), so the clone is cheap and frees the RefCell
        // borrow before any downstream code runs.
        self.audit_state
            .borrow()
            .as_ref()
            .expect("audit_state populated above")
            .value
            .clone()
    }

    /// Get cached notes (parsed once per session, invalidated on index change).
    /// Returns `Arc<Vec<Note>>` so repeat calls bump a refcount instead of
    /// cloning the full Vec — mirrors `call_graph` / `test_chunks`.
    pub(super) fn notes(&self) -> std::sync::Arc<Vec<cqs::note::Note>> {
        self.check_index_staleness();
        {
            let cached = self.notes_cache.borrow();
            if let Some(notes) = cached.as_ref() {
                return std::sync::Arc::clone(notes);
            }
        }
        let notes_path = self.root.join("docs/notes.toml");
        let notes = if notes_path.exists() {
            match cqs::note::parse_notes(&notes_path) {
                Ok(notes) => notes,
                // Split absent-file (TOCTOU after the .exists() check above)
                // from genuine parse failures, and include the path in the
                // warn so the journal isn't ambiguous about which notes file
                // failed.
                Err(e) => {
                    if let cqs::NoteError::Io(ref io_err) = e {
                        if io_err.kind() == std::io::ErrorKind::NotFound {
                            tracing::debug!(
                                path = %notes_path.display(),
                                "notes.toml disappeared between exists() and parse — treating as empty"
                            );
                            vec![]
                        } else {
                            tracing::warn!(
                                path = %notes_path.display(),
                                error = %e,
                                "Failed to parse notes.toml for batch"
                            );
                            vec![]
                        }
                    } else {
                        tracing::warn!(
                            path = %notes_path.display(),
                            error = %e,
                            "Failed to parse notes.toml for batch"
                        );
                        vec![]
                    }
                }
            }
        } else {
            vec![]
        };
        let arc = std::sync::Arc::new(notes);
        *self.notes_cache.borrow_mut() = Some(std::sync::Arc::clone(&arc));
        arc
    }

    /// Borrow a reference index by name (must be loaded via `get_ref` first).
    ///
    /// Returns `None` if the reference hasn't been loaded yet.
    /// `LruCache::get()` promotes the entry (marks as recently used), which
    /// requires `&mut self`; the LRU is held under a Mutex so the lock guard
    /// gives us the needed `&mut`.
    pub fn borrow_ref(&self, name: &str) -> Option<Arc<ReferenceIndex>> {
        let mut cache = self.refs.lock().unwrap_or_else(|p| p.into_inner());
        cache.get(name).map(Arc::clone)
    }

    /// Get or load the call graph (cached, invalidated on index change).
    pub(super) fn call_graph(&self) -> Result<Arc<cqs::store::CallGraph>> {
        self.check_index_staleness();
        {
            let cached = self.call_graph.borrow();
            if let Some(g) = cached.as_ref() {
                return Ok(Arc::clone(g));
            }
        }
        let _span = tracing::info_span!("batch_call_graph_init").entered();
        let store = self.store_arc_locked();
        let g = store.get_call_graph()?;
        let result = Arc::clone(&g);
        *self.call_graph.borrow_mut() = Some(g);
        Ok(result)
    }

    /// Get or load test chunks (cached, invalidated on index change).
    /// Returns Arc<Vec<ChunkSummary>> — O(1) clone.
    pub(super) fn test_chunks(&self) -> Result<Arc<Vec<cqs::store::ChunkSummary>>> {
        self.check_index_staleness();
        {
            let cached = self.test_chunks.borrow();
            if let Some(tc) = cached.as_ref() {
                return Ok(Arc::clone(tc));
            }
        }
        let _span = tracing::info_span!("batch_test_chunks_init").entered();
        let store = self.store_arc_locked();
        let tc = store.find_test_chunks()?;
        let result = Arc::clone(&tc);
        *self.test_chunks.borrow_mut() = Some(tc);
        Ok(result)
    }

    /// Get cached project config. Reloads from `.cqs/config.toml` when the
    /// cached value is older than [`CONFIG_RELOAD_INTERVAL`] (default 5 min),
    /// then returns an owned snapshot.
    ///
    /// `.cqs/config.toml` edits (e.g. `splade_alpha`, `ef_search`) take effect
    /// after this interval without `systemctl restart cqs-watch`. The 5-minute
    /// interval is conservative enough to avoid hot-loop file reads while
    /// keeping ad-hoc tweaks usable. Returning owned `Config` lets call sites
    /// use `self.config().ef_search` and `self.config().references` via
    /// auto-deref.
    pub(super) fn config(&self) -> cqs::config::Config {
        let needs_reload = match self.config.borrow().as_ref() {
            Some(c) => c.loaded_at.elapsed() >= CONFIG_RELOAD_INTERVAL,
            None => true,
        };
        if needs_reload {
            let fresh = cqs::config::Config::load(&self.root);
            *self.config.borrow_mut() = Some(CachedReload {
                value: fresh,
                loaded_at: Instant::now(),
            });
        }
        self.config
            .borrow()
            .as_ref()
            .expect("config populated above")
            .value
            .clone()
    }

    /// Get or create the reranker (cached for session).
    ///
    /// Returns the trait object so callers don't pin to `OnnxReranker` — a
    /// `--reranker` flag can swap impls at construction time without touching
    /// the consumer.
    pub(super) fn reranker(&self) -> Result<Arc<dyn cqs::Reranker>> {
        if let Some(r) = self.reranker.get() {
            return Ok(Arc::clone(r));
        }
        let _span = tracing::info_span!("batch_reranker_init").entered();
        // Thread the `[reranker]` config section so .cqs.toml preset/model_path
        // is honoured instead of silently defaulting to ms-marco.
        let config = self.config();
        let r: Arc<dyn cqs::Reranker> = Arc::new(
            cqs::OnnxReranker::with_section(config.reranker.clone())
                .map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?,
        );
        let _ = self.reranker.set(Arc::clone(&r));
        Ok(r)
    }

    /// Take the BatchContext store mutex briefly, clone the inner Arc, drop the
    /// lock. Lower-level than [`Self::store`] — does NOT run the staleness
    /// check; callers that need staleness should call `store()` instead. Used
    /// by the BatchContext-internal accessors that have already passed through
    /// `check_index_staleness` upstream (e.g. `vector_index`, `call_graph`,
    /// `test_chunks`).
    fn store_arc_locked(&self) -> Arc<Store<ReadOnly>> {
        let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(&guard)
    }

    /// Build a `BatchView` from a `&self` borrow. Used by stdin batch
    /// (single-threaded) and by [`checkout_view`] after the outer Mutex is
    /// taken. Stdin batch passes `outer_lock=None` because there is no shared
    /// `Arc<Mutex<BatchContext>>` to back-channel through; the `Refresh`
    /// handler in that path can call `BatchContext::invalidate` directly
    /// through the BatchContext that owns the dispatch.
    pub(crate) fn build_view(&self, outer_lock: Option<Arc<Mutex<BatchContext>>>) -> BatchView {
        // Run staleness check once at snapshot time so the view sees the
        // current store generation. Subsequent queries that need fresh data
        // after a mid-flight reindex pick it up on their next checkout_view.
        self.check_index_staleness();
        let store = {
            let guard = self.store.lock().unwrap_or_else(|p| p.into_inner());
            Arc::clone(&guard)
        };
        let vector_index = self.hnsw.borrow().as_ref().map(Arc::clone);
        let base_vector_index = self.base_hnsw.borrow().as_ref().map(Arc::clone);
        let call_graph = self.call_graph.borrow().as_ref().map(Arc::clone);
        let test_chunks = self.test_chunks.borrow().as_ref().map(Arc::clone);
        let notes_cache = self.notes_cache.borrow().as_ref().map(Arc::clone);
        let file_set = self.file_set.borrow().as_ref().map(Arc::clone);
        let splade_index_snapshot = self
            .splade_index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(Arc::clone);
        let config = self.config();
        let audit_state = self.audit_state();
        BatchView {
            store,
            cached_vector_index: vector_index,
            cached_base_vector_index: base_vector_index,
            cached_call_graph: call_graph,
            cached_test_chunks: test_chunks,
            cached_notes: notes_cache,
            cached_file_set: file_set,
            cached_splade_index: splade_index_snapshot,
            splade_index_cell: Arc::clone(&self.splade_index),
            embedder_slot: Arc::clone(&self.embedder),
            reranker_slot: Arc::clone(&self.reranker),
            splade_encoder_slot: Arc::clone(&self.splade_encoder),
            refs: Arc::clone(&self.refs),
            config,
            audit_state,
            model_config: self.model_config.clone(),
            root: self.root.clone(),
            cqs_dir: self.cqs_dir.clone(),
            error_count: Arc::clone(&self.error_count),
            query_count: Arc::clone(&self.query_count),
            started_at: self.started_at,
            outer_lock,
            watch_snapshot: Arc::clone(&self.watch_snapshot),
            reconcile_signal: Arc::clone(&self.reconcile_signal),
            fresh_notifier: Arc::clone(&self.fresh_notifier),
        }
    }
}
