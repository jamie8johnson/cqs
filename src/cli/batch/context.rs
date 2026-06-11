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

// ─── Cross-project cache entry ───────────────────────────────────────────────

/// A cached cross-project context plus the references-config fingerprint it
/// was built from. The fingerprint lets a reader detect a config edit
/// (references added/removed/repointed/reweighted) and rebuild even though the
/// underlying `index.db` files never changed.
///
/// The context is wrapped in `Arc<Mutex<...>>` so a [`BatchView`] can hand the
/// `&mut CrossProjectContext` the cross cores require to a dispatch handler
/// without holding any BatchContext lock — and so the lazily-populated
/// per-store graph cache inside the context survives across requests.
pub(super) struct CachedCrossProject {
    pub(super) ctx: Arc<Mutex<cqs::cross_project::CrossProjectContext>>,
    pub(super) fingerprint: u64,
}

// ─── BatchContext ────────────────────────────────────────────────────────────

/// One bit per mutable-cache slot, used by the deferred-invalidation mask
/// (`BatchContext::pending_invalidation`) so a retry clears only the slots
/// that were actually deferred. `u16` rather than `u8`: the cross-project
/// cell pushed the slot count past 8.
mod slot {
    pub(super) const HNSW: u16 = 1 << 0;
    pub(super) const BASE_HNSW: u16 = 1 << 1;
    pub(super) const CALL_GRAPH: u16 = 1 << 2;
    pub(super) const TEST_CHUNKS: u16 = 1 << 3;
    pub(super) const FILE_SET: u16 = 1 << 4;
    pub(super) const NOTES: u16 = 1 << 5;
    pub(super) const SPLADE: u16 = 1 << 6;
    pub(super) const REFS: u16 = 1 << 7;
    pub(super) const CROSS_PROJECT: u16 = 1 << 8;
    pub(super) const ALL: u16 = u16::MAX;
}

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
/// **Mutable caches** (hnsw, base_hnsw, file_set, notes_cache, splade_index)
/// are shared `Arc<Mutex<Option<Arc<T>>>>` write-back cells carried by every
/// `BatchView`; call_graph / test_chunks stay view-local `RefCell`s (their
/// rebuild is a Store-internal cache hit). All are auto-invalidated when
/// index.db changes —
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
    /// shows up after `config_reload_interval` (default 5 min) without a daemon
    /// restart. The reload is a sub-ms file read; cost is negligible per query.
    pub(super) config: RefCell<Option<CachedReload<cqs::config::Config>>>,
    /// `Arc<OnceLock<...>>` so views share one slot with the BatchContext. The
    /// inner type is `Arc<dyn cqs::Reranker>` so the trait object can be
    /// swapped at construction time (NoopReranker for ablation, future
    /// LlmReranker for batch eval, etc.) without touching the cache surface.
    pub(super) reranker: Arc<OnceLock<Arc<dyn cqs::Reranker>>>,
    /// `RefCell<Option<CachedReload<AuditMode>>>` so the 30-min audit
    /// auto-expire fires while the daemon is up. Reloads from
    /// `.cqs/audit-mode.json` every `audit_state_reload_interval` (default
    /// 30 s); the file carries its own embedded `expires_at` so the load
    /// itself respects expiration.
    pub(super) audit_state: RefCell<Option<CachedReload<cqs::audit::AuditMode>>>,
    // Mutable caches — invalidated on index change.
    //
    // `hnsw` / `base_hnsw` / `file_set` / `notes_cache` are shared write-back
    // cells (`Arc<Mutex<Option<Arc<T>>>>`, same shape as `splade_index`):
    // `BatchView` carries a clone of each cell, and a view that builds the
    // value on a cache miss publishes it back (epoch-guarded, see
    // `invalidation_epoch`) so the next checkout snapshots it instead of
    // rebuilding. Without the write-back, every daemon search rebuilt the
    // vector index from disk (~400 ms per query against a 3-19 ms budget).
    //
    // `call_graph` / `test_chunks` stay view-local `RefCell`s: their view
    // fallback goes through the snapshot Store, which holds its own internal
    // `OnceLock` caches, so a per-view rebuild is a cheap cache hit there.
    pub(super) hnsw: Arc<Mutex<Option<Arc<dyn VectorIndex>>>>,
    pub(super) base_hnsw: Arc<Mutex<Option<Arc<dyn VectorIndex>>>>,
    pub(super) call_graph: RefCell<Option<Arc<cqs::store::CallGraph>>>,
    pub(super) test_chunks: RefCell<Option<Arc<Vec<cqs::store::ChunkSummary>>>>,
    /// Cache returns `Arc<HashSet<PathBuf>>` so callers don't clone the full
    /// set on every invocation.
    pub(super) file_set: Arc<Mutex<Option<Arc<HashSet<PathBuf>>>>>,
    /// Cached notes returned as `Arc<Vec<Note>>` so callers don't clone the
    /// full Vec on every dispatch.
    pub(super) notes_cache: Arc<Mutex<Option<Arc<Vec<cqs::note::Note>>>>>,
    /// Monotonic counter bumped at the start of every mutable-cache
    /// invalidation. Views snapshot it at checkout; a view-side cache build
    /// publishes into the shared cells only while the epoch is unchanged, so
    /// a value built from a pre-invalidation store snapshot can never land
    /// after (and survive) a fresh invalidation.
    pub(super) invalidation_epoch: Arc<AtomicU64>,
    /// Sticky per-slot deferral mask (bits from the `slot` constants below;
    /// `0` = nothing pending). Set when [`Self::invalidate_mutable_caches`]
    /// had to defer at least one slot (a handler held its borrow/lock
    /// mid-invalidation). [`Self::check_index_staleness`] honors it
    /// regardless of the identity / data_version discriminators — those were
    /// already consumed when the invalidation first fired, so without the
    /// mask the deferred slots would never be retried. The retry clears ONLY
    /// the masked slots and does NOT bump the epoch: re-bumping would
    /// discard every in-flight fresh publish and wipe freshly rebuilt slots
    /// on every dispatch while one slot stays contended. Cleared (to 0) only
    /// when every deferred slot actually cleared.
    pub(super) pending_invalidation: Cell<u16>,
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
    /// Cached cross-project context (opened reference stores + per-store call
    /// graphs), shared with every `BatchView` so the daemon's
    /// `--cross-project` graph dispatchers (callers/callees/impact/test-map/
    /// trace) stop rebuilding it per request. Without this, each cross-project
    /// query reopened N reference stores (~64 MB mmap × N) and re-merged every
    /// project's call graph.
    ///
    /// The inner `Arc<Mutex<CrossProjectContext>>` gives the dispatch handlers
    /// the `&mut` they need (the cross cores lazily populate the per-store
    /// graph cache), while the outer cell follows the same write-back shape as
    /// the other mutable caches.
    ///
    /// # Staleness contract
    ///
    /// Three independent triggers force a rebuild:
    ///
    /// 1. **Local reindex** — the merged graph includes the local project's
    ///    edges, so a primary-`index.db` change must drop it. The
    ///    `CROSS_PROJECT` slot is in [`slot::ALL`], so `invalidate_mutable_caches`
    ///    (fired by the identity / data_version staleness check) clears it
    ///    alongside the other caches.
    /// 2. **Reference store update** (`cqs ref update <name>`) — rewrites a
    ///    reference's `index.db` without touching the primary file. The cached
    ///    context captures each store's `(mtime, size)` at open; the view
    ///    accessor calls `CrossProjectContext::is_stale()` before serving and
    ///    rebuilds on a mismatch.
    /// 3. **References config edit** (`.cqs.toml` / `slot.toml`) — the cell is
    ///    keyed by a fingerprint of the `references` config; a fingerprint
    ///    mismatch (config reload picks up the edit after
    ///    `CONFIG_RELOAD_INTERVAL`) rebuilds. The fingerprint rides alongside
    ///    the context so a stale cell whose config moved is never served.
    pub(super) cross_project: Arc<Mutex<Option<CachedCrossProject>>>,
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
    /// `index_id` alone cannot see.
    ///
    /// `None` when the probe couldn't be opened (warned, identity-only
    /// fallback); re-opened lazily on the next staleness check.
    pub(super) data_version_probe: RefCell<Option<DataVersionProbe>>,
    /// When the staleness check last ran. Used to rate-limit `fs::metadata`
    /// on `index.db` — see [`staleness_check_interval`].
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
            hnsw: Arc::new(Mutex::new(None)),
            base_hnsw: Arc::new(Mutex::new(None)),
            call_graph: RefCell::new(None),
            test_chunks: RefCell::new(None),
            file_set: Arc::new(Mutex::new(None)),
            notes_cache: Arc::new(Mutex::new(None)),
            invalidation_epoch: Arc::new(AtomicU64::new(0)),
            pending_invalidation: Cell::new(0),
            splade_encoder: Arc::new(OnceLock::new()),
            splade_index: Arc::new(Mutex::new(None)),
            cross_project: Arc::new(Mutex::new(None)),
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
    ///    number of incremental reindexes.
    ///
    /// False positives cost one cache reload; false negatives are the bug,
    /// so the probe falls back to identity-only (with a warn) rather than
    /// blocking the check when it can't be opened or queried.
    ///
    /// Rate-limited to at most once per [`staleness_check_interval`] —
    /// except when a prior invalidation deferred a busy slot, in which case
    /// the sticky `pending_invalidation` flag forces a retry. Every
    /// `ctx.store()`, every `build_view` checkout, and the remaining
    /// BatchContext accessors call this.
    pub(crate) fn check_index_staleness(&self) {
        // A pending (deferred) invalidation bypasses the rate limit: the
        // discriminators below were already consumed when the invalidation
        // first fired (probe baseline advanced, index_id refreshed), so
        // waiting on them would never retry the deferred slots.
        let pending = self.pending_invalidation.get();
        let now = Instant::now();
        if pending == 0 {
            if let Some(prev) = self.last_staleness_check.get() {
                if now.duration_since(prev) < staleness_check_interval() {
                    return;
                }
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
        } else if pending != 0 {
            // Retry a deferred invalidation. The Store re-open, probe
            // rebaseline, and epoch bump already happened when the
            // invalidation first fired; only the slots that were busy still
            // need clearing. Clear-only, masked: no epoch re-bump (which
            // would discard in-flight fresh publishes) and no wipe of slots
            // that were already cleared and freshly rebuilt since.
            let _span = tracing::info_span!("batch_index_invalidation_retry").entered();
            self.clear_cache_slots(pending);
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
    /// Clearing the cell lets `ensure_splade_index` see `None` on the next
    /// call and rebuild from
    /// the freshly persisted `splade.index.bin` (or SQLite fallback).
    /// Returns `true` when every slot actually cleared; `false` when at
    /// least one slot was deferred (a handler held its borrow/lock). On
    /// deferral the sticky `pending_invalidation` mask records the deferred
    /// slots so [`Self::check_index_staleness`] retries the clear even
    /// though the identity / data_version discriminators were already
    /// consumed.
    fn invalidate_mutable_caches(&self) -> bool {
        // Bump the epoch BEFORE clearing any slot: a view that snapshotted
        // the previous epoch at checkout sees the mismatch when it tries to
        // publish a freshly built value, so an index built from the
        // pre-invalidation store snapshot is discarded instead of being
        // re-published over the cleared cell.
        self.invalidation_epoch.fetch_add(1, Ordering::SeqCst);
        self.clear_cache_slots(slot::ALL)
    }

    /// Clear the cache slots named in `mask`. Does NOT bump the epoch — the
    /// deferred-invalidation retry path depends on that: any value that can
    /// be published into a slot after the original invalidation was built at
    /// or after the already-bumped epoch (older builds fail the publish
    /// guard), so re-bumping here would only discard legitimate fresh
    /// publishes and re-clear freshly rebuilt slots on every dispatch while
    /// one slot stays contended.
    ///
    /// Uses `try_borrow_mut` / `try_lock`: a search handler may still hold a
    /// borrow on a cache slot across an accessor call that triggers a
    /// staleness re-check (for example handlers/search.rs does
    /// `let splade_index_ref = ctx.borrow_splade_index()` then later calls
    /// `ctx.store().search_hybrid(...)`). Panicking on borrow_mut() would
    /// crash the whole batch session for what is just a deferral case.
    /// Busy slots stay populated and are recorded in the sticky
    /// `pending_invalidation` mask for the next retry.
    fn clear_cache_slots(&self, mask: u16) -> bool {
        let mut deferred: u16 = 0;
        macro_rules! try_clear_refcell {
            ($field:expr, $bit:expr, $name:literal) => {
                if mask & $bit != 0 {
                    match $field.try_borrow_mut() {
                        Ok(mut g) => *g = None,
                        Err(_) => {
                            deferred |= $bit;
                            tracing::debug!(slot = $name, "borrow held; deferring invalidation");
                        }
                    }
                }
            };
        }
        // Shared write-back cells are `Arc<Mutex<...>>`; `try_lock` mirrors
        // the borrow-deferral semantics of the RefCell branches.
        macro_rules! try_clear_cell {
            ($field:expr, $bit:expr, $name:literal) => {
                if mask & $bit != 0 {
                    match $field.try_lock() {
                        Ok(mut g) => *g = None,
                        Err(_) => {
                            deferred |= $bit;
                            tracing::debug!(slot = $name, "lock held; deferring invalidation");
                        }
                    }
                }
            };
        }
        try_clear_cell!(self.hnsw, slot::HNSW, "hnsw");
        try_clear_cell!(self.base_hnsw, slot::BASE_HNSW, "base_hnsw");
        try_clear_refcell!(self.call_graph, slot::CALL_GRAPH, "call_graph");
        try_clear_refcell!(self.test_chunks, slot::TEST_CHUNKS, "test_chunks");
        try_clear_cell!(self.file_set, slot::FILE_SET, "file_set");
        try_clear_cell!(self.notes_cache, slot::NOTES, "notes_cache");
        try_clear_cell!(self.splade_index, slot::SPLADE, "splade_index");
        // The cross-project cell holds `Option<CachedCrossProject>` (not the
        // `Option<Arc<T>>` shape the macro's siblings carry), but the clear is
        // identical: drop the cached context so the next view rebuilds it.
        try_clear_cell!(self.cross_project, slot::CROSS_PROJECT, "cross_project");
        // refs LRU is `Arc<Mutex<...>>` (shared with BatchView). If a
        // handler thread holds the mutex (e.g. iterating refs in a parallel
        // search), the eviction is deferred to the next retry.
        if mask & slot::REFS != 0 {
            match self.refs.try_lock() {
                Ok(mut g) => g.clear(),
                Err(_) => {
                    deferred |= slot::REFS;
                    tracing::debug!(slot = "refs", "lock held; deferring invalidation");
                }
            }
        }

        // Sticky: stays set across checks until a retry clears every
        // deferred slot.
        self.pending_invalidation.set(deferred);
        if deferred != 0 {
            tracing::debug!(
                deferred_mask = deferred,
                "partial cache invalidation; pending mask set — next staleness check retries"
            );
        }
        deferred == 0
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

    /// Write a success payload to `out`, logging and counting a write failure
    /// instead of silently dropping it. For the daemon path `out` is an
    /// in-memory `Vec` (infallible); for the stdin batch surface `out` is
    /// stdout, where an EPIPE or full-disk redirect would otherwise leave the
    /// agent with no response line, no log, and no `error_count` bump. Mirrors
    /// the daemon socket's `write_daemon_error_tracked` (socket.rs).
    fn write_ok_tracked(&self, out: &mut impl std::io::Write, value: &serde_json::Value) {
        if let Err(e) = write_json_line(out, value) {
            self.error_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %e, "Batch dispatch: failed to write response line");
        }
    }

    /// Write an error envelope to `out`, logging a write failure. The caller
    /// has already bumped `error_count` for the dispatch error itself, so this
    /// only warns when the envelope write also fails (e.g. EPIPE on stdout) —
    /// the symptom is otherwise invisible.
    fn write_err_tracked(&self, out: &mut impl std::io::Write, code: &str, message: &str) {
        if let Err(e) = write_envelope_error(out, code, message) {
            tracing::warn!(error = %e, "Batch dispatch: failed to write error envelope");
        }
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
                self.write_err_tracked(out, error_codes::PARSE_ERROR, &msg);
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
            self.write_err_tracked(out, error_codes::INVALID_INPUT, msg);
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
                            self.write_ok_tracked(
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
                            self.write_err_tracked(out, code.as_str(), &msg);
                        }
                    }
                    return;
                }
                match commands::dispatch(&view, input.cmd) {
                    Ok(value) => {
                        self.write_ok_tracked(out, &value);
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
                        self.write_err_tracked(out, code.as_str(), &msg);
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
                self.write_err_tracked(out, error_codes::PARSE_ERROR, &msg);
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

    /// Get cached audit state. Reloads from `.cqs/audit-mode.json` when the
    /// cached value is older than [`audit_state_reload_interval`] (default
    /// 30 s), then returns an owned snapshot.
    ///
    /// The file is sub-ms to read; the 30 s interval bounds staleness while
    /// keeping accessor cost negligible. Returning owned `AuditMode` (rather
    /// than `&AuditMode` from a borrow) lets the `let audit = ctx.audit_state();
    /// &audit` call-site pattern work without juggling `Ref<'_, ...>` lifetimes.
    pub(super) fn audit_state(&self) -> cqs::audit::AuditMode {
        let needs_reload = match self.audit_state.borrow().as_ref() {
            Some(c) => c.loaded_at.elapsed() >= audit_state_reload_interval(),
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
    /// cached value is older than [`config_reload_interval`] (default 5 min),
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
            Some(c) => c.loaded_at.elapsed() >= config_reload_interval(),
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
        // Epoch is captured after the staleness check above, in the same
        // critical section as the store snapshot — the pair is coherent. A
        // later invalidation bumps the shared counter, and the view's
        // publish-back path compares against this captured value.
        let checkout_epoch = self.invalidation_epoch.load(Ordering::SeqCst);
        fn snapshot_cell<T: ?Sized>(cell: &Mutex<Option<Arc<T>>>) -> Option<Arc<T>> {
            cell.lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(Arc::clone)
        }
        let vector_index = snapshot_cell(&self.hnsw);
        let base_vector_index = snapshot_cell(&self.base_hnsw);
        let call_graph = self.call_graph.borrow().as_ref().map(Arc::clone);
        let test_chunks = self.test_chunks.borrow().as_ref().map(Arc::clone);
        let notes_cache = snapshot_cell(&self.notes_cache);
        let file_set = snapshot_cell(&self.file_set);
        let splade_index_snapshot = snapshot_cell(&self.splade_index);
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
            vector_index_cell: Arc::clone(&self.hnsw),
            base_vector_index_cell: Arc::clone(&self.base_hnsw),
            file_set_cell: Arc::clone(&self.file_set),
            notes_cell: Arc::clone(&self.notes_cache),
            splade_index_cell: Arc::clone(&self.splade_index),
            cross_project_cell: Arc::clone(&self.cross_project),
            invalidation_epoch: Arc::clone(&self.invalidation_epoch),
            checkout_epoch,
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
