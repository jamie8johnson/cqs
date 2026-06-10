//! `BatchView`: an owned-`Arc` snapshot of a `BatchContext`, plus the
//! handler-routing glue (`checkout_view_from_arc`, `dispatch_via_view`) and
//! the `SearchCtx` impl that drives the shared query core.
//!
//! Split out of the former monolithic `cli/batch/mod.rs` (issue #1691).

use super::*;

/// Produce a `BatchView` from an `Arc<Mutex<BatchContext>>`. Lock the mutex
/// briefly, snapshot the Arcs, drop the guard. The view carries the
/// `Arc<Mutex<BatchContext>>` as a back-channel for `Refresh`.
///
/// Free function (not an inherent method) because Rust does not yet allow
/// `self: &Arc<Mutex<Self>>` in stable.
pub(crate) fn checkout_view_from_arc(ctx: &Arc<Mutex<BatchContext>>) -> BatchView {
    let guard = ctx.lock().unwrap_or_else(|p| p.into_inner());
    // Counter bumps and idle-timeout sweep happen here under the brief lock
    // because they're cheap (one atomic each, optional cache evictions) and
    // every dispatch needs them. Note: query_count itself is bumped in
    // `dispatch_via_view`, not here, because callers may early-return on
    // empty tokens; we want the counter to reflect "dispatch reached".
    guard.check_idle_timeout();
    guard.build_view(Some(Arc::clone(ctx)))
}

/// Handler-routing layer that operates on a [`BatchView`] snapshot.
///
/// This is the inner half of the dispatch split. The outer half
/// ([`BatchContext::dispatch_parsed_tokens`] for stdin batch and the daemon
/// caller in `cli::watch::handle_socket_client`) handles tokenization,
/// idle-bookkeeping, and view construction. Once a view is in hand, this
/// function:
///
///   1. Rejects NUL bytes with `invalid_input`.
///   2. Parses the tokens via clap.
///   3. Routes `Refresh` through the view's `outer_lock` back-channel
///      (daemon path) or returns an error (stdin batch should not reach
///      this function with `Refresh`; it goes through
///      `BatchContext::invalidate` directly).
///   4. Routes every other command through [`commands::dispatch`] which
///      operates on `&BatchView`.
///   5. Bumps `error_count` on parse / dispatch failure via the view's
///      shared `Arc<AtomicU64>` (no re-lock needed).
///
/// Daemon callers MUST have already bumped `query_count` and run
/// `check_idle_timeout` under the outer lock (in `checkout_view_from_arc`
/// or equivalent); this function does not duplicate that work.
pub(crate) fn dispatch_via_view(
    view: &BatchView,
    command: &str,
    args: &[String],
    out: &mut impl std::io::Write,
) {
    use crate::cli::json_envelope::error_codes;

    // Tokens reconstructed for the clap parser: ["command", "args"...].
    // Empty command is a no-op (parity with `BatchContext::dispatch_tokens`).
    if command.is_empty() {
        return;
    }
    let tokens: Vec<String> = std::iter::once(command.to_string())
        .chain(args.iter().cloned())
        .collect();

    // NUL byte rejection — same contract as the stdin loop.
    if let Err(msg) = reject_null_tokens(&tokens) {
        view.error_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            code = error_codes::INVALID_INPUT,
            error = msg,
            "Daemon dispatch: NUL byte in tokens"
        );
        let _ = write_envelope_error(out, error_codes::INVALID_INPUT, msg);
        return;
    }
    // query_count bumped here (after NUL check) so the contract matches
    // the stdin path: NUL rejection does not count as a query.
    view.query_count.fetch_add(1, Ordering::Relaxed);

    match commands::BatchInput::try_parse_from(&tokens) {
        Ok(input) => {
            if matches!(input.cmd, commands::BatchCmd::Refresh) {
                match view.invalidate_via_outer() {
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
                        view.error_count.fetch_add(1, Ordering::Relaxed);
                        let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                        let _ = write_envelope_error(out, code.as_str(), &msg);
                    }
                }
                return;
            }
            match commands::dispatch(view, input.cmd) {
                Ok(value) => {
                    let _ = write_json_line(out, &value);
                }
                Err(e) => {
                    view.error_count.fetch_add(1, Ordering::Relaxed);
                    let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                    let _ = write_envelope_error(out, code.as_str(), &msg);
                }
            }
        }
        Err(e) => {
            view.error_count.fetch_add(1, Ordering::Relaxed);
            let msg = format!("{e:#}");
            tracing::warn!(
                code = error_codes::PARSE_ERROR,
                error = %msg,
                "Daemon dispatch: clap parse failed"
            );
            let _ = write_envelope_error(out, error_codes::PARSE_ERROR, &msg);
        }
    }
}

/// Shared helper for `BatchContext::get_ref` and `BatchView::get_ref`.
/// Operates directly on the LRU mutex so both paths see the same cache.
pub(crate) fn get_ref_via_refs_lru(
    refs: &Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>,
    config: &cqs::config::Config,
    name: &str,
) -> Result<()> {
    let _span = tracing::info_span!("batch_get_ref", %name).entered();
    {
        let mut cache = refs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = cache.peek(name) {
            if existing.is_stale() {
                tracing::info!(
                    reference = %name,
                    "Cached reference stale (index.db mtime/size changed) — evicting for reload"
                );
                cache.pop(name);
            } else {
                return Ok(());
            }
        }
    }

    // Filter to just the target reference instead of loading all.
    let single: Vec<_> = config
        .references
        .iter()
        .filter(|r| r.name == name)
        .cloned()
        .collect();
    if single.is_empty() {
        anyhow::bail!(
            "Reference '{}' not found. Run 'cqs ref list' to see available references.",
            name
        );
    }
    let loaded = cqs::reference::load_references(&single);
    let found = loaded.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to load reference '{}'. Run 'cqs ref update {}' first.",
            name,
            name
        )
    })?;
    refs.lock()
        .unwrap_or_else(|p| p.into_inner())
        .put(name.to_string(), Arc::new(found));
    Ok(())
}

/// Shared helper for `BatchContext::get_all_refs` and the equivalent on
/// `BatchView`. Walks the configured references, partitions hits/misses
/// against the LRU under one lock, then loads misses outside the lock and
/// re-acquires briefly to stash them.
pub(crate) fn get_all_refs_via_refs_lru(
    refs: &Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>,
    config: &cqs::config::Config,
) -> Result<Vec<Arc<ReferenceIndex>>> {
    let _span = tracing::info_span!("batch_get_all_refs").entered();
    if config.references.is_empty() {
        return Ok(Vec::new());
    }

    let mut hits: Vec<Arc<ReferenceIndex>> = Vec::with_capacity(config.references.len());
    let mut misses: Vec<cqs::config::ReferenceConfig> = Vec::new();
    {
        let mut cache = refs.lock().unwrap_or_else(|p| p.into_inner());
        for cfg in &config.references {
            if let Some(existing) = cache.peek(&cfg.name) {
                if existing.is_stale() {
                    tracing::info!(
                        reference = %cfg.name,
                        "Cached reference stale (index.db mtime/size changed) — evicting for reload"
                    );
                    cache.pop(&cfg.name);
                    misses.push(cfg.clone());
                } else {
                    let arc = cache
                        .get(&cfg.name)
                        .map(Arc::clone)
                        .expect("peek hit above");
                    hits.push(arc);
                }
            } else {
                misses.push(cfg.clone());
            }
        }
    }

    if !misses.is_empty() {
        let loaded = cqs::reference::load_references(&misses);
        let mut cache = refs.lock().unwrap_or_else(|p| p.into_inner());
        for ri in loaded {
            let arc = Arc::new(ri);
            cache.put(arc.name.clone(), Arc::clone(&arc));
            hits.push(arc);
        }
    }

    Ok(hits)
}

// ─── BatchView ──────────────────────────────────────────────────────────────
//
// Snapshot of the BatchContext fields a daemon-dispatchable handler needs.
// Built by `BatchContext::checkout_view` (or `build_view` for stdin batch)
// under a brief critical section, then handed to handlers running outside the
// BatchContext lock. The view owns Arc clones — no borrows into BatchContext —
// so it is `Send` and survives lock release.
//
// Two reasons this is the right shape (vs `RwLock<BatchContext>`):
//
//   1. `BatchContext: !Sync` because of its `RefCell`/`Cell` interior; the
//      single-threaded "stable cache" pattern is correct for everything
//      except the store / refs LRU. Converting all 12+ cells to RwLock would
//      be a much bigger refactor.
//   2. The brief mutex hold is essentially free even under contention, while
//      RwLock readers still have to acquire reader-state atomically per
//      query. The snapshot pattern collapses both concerns into one short
//      critical section.
//
// The view exposes a read-only-handler API mirroring the BatchContext
// surface: `store()`, `vector_index()`, `embedder()`, `reranker()`,
// `notes()`, `audit_state()`, `config()`, `splade_encoder()`,
// `ensure_splade_index()`, `borrow_splade_index()`, `get_all_refs()`,
// `get_ref()`, `file_set()`, `call_graph()`, `test_chunks()`. The
// `Refresh` handler back-channels through `outer_lock` — the only
// daemon-dispatchable command that mutates BatchContext interior.
pub(crate) struct BatchView {
    pub(super) store: Arc<Store<ReadOnly>>,
    /// HNSW snapshot taken at checkout. Handlers that need a fresh build
    /// fall back to `lazy_vector_index` which rebuilds via the store; the
    /// rebuild path doesn't touch BatchContext (the Arc<dyn VectorIndex>
    /// is constructed fresh each time the cached snapshot is None and
    /// stays local to this view).
    pub(super) cached_vector_index: Option<Arc<dyn VectorIndex>>,
    pub(super) cached_base_vector_index: Option<Arc<dyn VectorIndex>>,
    pub(super) cached_call_graph: Option<Arc<cqs::store::CallGraph>>,
    pub(super) cached_test_chunks: Option<Arc<Vec<cqs::store::ChunkSummary>>>,
    pub(super) cached_notes: Option<Arc<Vec<cqs::note::Note>>>,
    pub(super) cached_file_set: Option<Arc<HashSet<PathBuf>>>,
    pub(super) cached_splade_index: Option<Arc<cqs::splade::index::SpladeIndex>>,
    /// Shared `Arc<Mutex<...>>` to the BatchContext's splade_index cell.
    /// `ensure_splade_index` populates it for handlers running through
    /// the view; the BatchContext path picks up the same value on its
    /// next `checkout_view`.
    pub(super) splade_index_cell: Arc<Mutex<Option<Arc<cqs::splade::index::SpladeIndex>>>>,
    /// Shared `Arc<OnceLock<...>>` to the BatchContext embedder slot. Init
    /// from the view propagates to the BatchContext (and any other view
    /// holding the same Arc).
    pub(super) embedder_slot: Arc<OnceLock<Arc<Embedder>>>,
    pub(super) reranker_slot: Arc<OnceLock<Arc<dyn cqs::Reranker>>>,
    pub(super) splade_encoder_slot: Arc<OnceLock<Option<cqs::splade::SpladeEncoder>>>,
    /// Shared refs LRU.
    pub(super) refs: Arc<Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>>,
    /// Cheap clones at checkout. A reload mid-flight returns stale data for
    /// the in-flight query.
    pub(super) config: cqs::config::Config,
    pub(super) audit_state: cqs::audit::AuditMode,
    pub model_config: cqs::embedder::ModelConfig,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    /// Counter handles. `Arc<AtomicU64>` so handlers and the daemon both
    /// see the same counter without re-locking the outer BatchContext.
    pub(crate) error_count: Arc<AtomicU64>,
    pub(crate) query_count: Arc<AtomicU64>,
    pub(super) started_at: Instant,
    /// Back-channel to the BatchContext mutex for the `Refresh` handler.
    /// `None` for stdin batch (single-threaded — `BatchContext::invalidate`
    /// is reachable directly through the path that owns the dispatch).
    /// `Some` for daemon connections, where `dispatch_refresh` re-acquires
    /// the mutex briefly to call `invalidate`.
    pub(super) outer_lock: Option<Arc<Mutex<BatchContext>>>,
    /// Shared snapshot of watch-loop freshness state. Cloned from
    /// `BatchContext::watch_snapshot` at view checkout — the Arc itself is
    /// shared with the watch loop, so a `dispatch_status` handler reads the
    /// *current* snapshot the loop most recently published, not a stale one
    /// from the moment the view was built.
    pub(super) watch_snapshot: cqs::watch_status::SharedWatchSnapshot,
    /// Shared one-shot reconcile signal. Cloned the same way as
    /// `watch_snapshot`. `dispatch_reconcile` flips this to `true` on the
    /// daemon's behalf; the watch loop swaps it back to `false` and runs an
    /// immediate reconcile pass.
    pub(super) reconcile_signal: cqs::watch_status::SharedReconcileSignal,
    /// Shared event-driven freshness notifier. Cloned the same way;
    /// `dispatch_wait_fresh` parks on this until the watch loop publishes a
    /// Fresh transition or the caller's deadline runs out.
    pub(super) fresh_notifier: cqs::watch_status::SharedFreshNotifier,
}

impl BatchView {
    /// Cloned `Arc<Store>` — handlers hold this for the lifetime of the
    /// dispatch.
    pub fn store(&self) -> Arc<Store<ReadOnly>> {
        Arc::clone(&self.store)
    }

    /// HNSW vector index for this snapshot. If the cache was empty at
    /// checkout, build a fresh one from the snapshot store (no BatchContext
    /// access needed).
    pub fn vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        if let Some(arc) = &self.cached_vector_index {
            if !arc.is_poisoned() {
                return Ok(Some(Arc::clone(arc)));
            }
            tracing::warn!(
                name = arc.name(),
                "BatchView vector index is poisoned — rebuilding from snapshot store"
            );
        }
        let _span = tracing::info_span!("batch_view_vector_index_init").entered();
        let idx = build_vector_index(&self.store, &self.cqs_dir, self.config.ef_search)?;
        Ok(idx.map(|boxed| -> Arc<dyn VectorIndex> { boxed.into() }))
    }

    pub fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        if let Some(arc) = &self.cached_base_vector_index {
            return Ok(Some(Arc::clone(arc)));
        }
        let _span = tracing::info_span!("batch_view_base_vector_index_init").entered();
        let idx = crate::cli::build_base_vector_index(&self.store, &self.cqs_dir)?;
        Ok(idx.map(|boxed| -> Arc<dyn VectorIndex> { boxed.into() }))
    }

    pub fn embedder(&self) -> Result<&Embedder> {
        if let Some(e) = self.embedder_slot.get() {
            return Ok(e.as_ref());
        }
        let _span = tracing::info_span!("batch_view_embedder_init").entered();
        let e = Embedder::new(self.model_config.clone())?;
        let _ = self.embedder_slot.set(Arc::new(e));
        Ok(self
            .embedder_slot
            .get()
            .map(|arc| arc.as_ref())
            .expect("embedder OnceLock populated by set() above"))
    }

    pub fn reranker(&self) -> Result<Arc<dyn cqs::Reranker>> {
        if let Some(r) = self.reranker_slot.get() {
            return Ok(Arc::clone(r));
        }
        let _span = tracing::info_span!("batch_view_reranker_init").entered();
        let r: Arc<dyn cqs::Reranker> = Arc::new(
            cqs::OnnxReranker::with_section(self.config.reranker.clone())
                .map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?,
        );
        let _ = self.reranker_slot.set(Arc::clone(&r));
        Ok(r)
    }

    pub fn splade_encoder(&self) -> Option<&cqs::splade::SpladeEncoder> {
        let opt = self.splade_encoder_slot.get_or_init(|| {
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

    pub fn ensure_splade_index(&self) {
        if self
            .splade_index_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            return;
        }
        let generation = match self.store.splade_generation() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to read splade_generation — skipping SPLADE for this view; \
                     search will fall back to dense-only"
                );
                return;
            }
        };
        let splade_path = self.cqs_dir.join(cqs::splade::index::SPLADE_INDEX_FILENAME);
        let build_start = Instant::now();
        let (idx, rebuilt) =
            cqs::splade::index::SpladeIndex::load_or_build(&splade_path, generation, || match self
                .store
                .load_all_sparse_vectors()
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to load sparse vectors, falling back to cosine-only"
                    );
                    Vec::new()
                }
            });
        let build_ms = build_start.elapsed().as_millis() as u64;
        if idx.is_empty() {
            return;
        }
        if rebuilt {
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                rebuild_ms = build_ms,
                "SPLADE index rebuilt from SQLite (view)"
            );
            if build_ms > 30_000 {
                tracing::warn!(
                    rebuild_ms = build_ms,
                    chunks = idx.len(),
                    "SPLADE index rebuild exceeded 30s (view)"
                );
            }
        } else {
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                load_ms = build_ms,
                "SPLADE index loaded from disk (view)"
            );
        }
        *self
            .splade_index_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(idx));
    }

    pub fn borrow_splade_index(&self) -> Option<Arc<cqs::splade::index::SpladeIndex>> {
        // Prefer the snapshot taken at checkout; fall back to the live
        // cell so a freshly populated index (via `ensure_splade_index`
        // during this dispatch) is observable without re-checkout.
        if let Some(arc) = &self.cached_splade_index {
            return Some(Arc::clone(arc));
        }
        self.splade_index_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// Borrowed access keeps the snapshot Config in place; clone-on-access
    /// in case a handler wants ownership.
    #[allow(
        dead_code,
        reason = "pinned for handler-rename parity with BatchContext"
    )]
    pub fn config(&self) -> cqs::config::Config {
        self.config.clone()
    }

    pub fn audit_state(&self) -> cqs::audit::AuditMode {
        self.audit_state.clone()
    }

    pub fn notes(&self) -> Arc<Vec<cqs::note::Note>> {
        if let Some(notes) = &self.cached_notes {
            return Arc::clone(notes);
        }
        // Snapshot was empty at checkout — load once from disk into a fresh
        // Arc; we don't write back into the BatchContext cache because the next
        // `checkout_view` after a real reindex will re-stat notes.toml anyway.
        let notes_path = self.root.join("docs/notes.toml");
        let notes = if notes_path.exists() {
            cqs::note::parse_notes(&notes_path).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %notes_path.display(),
                    error = %e,
                    "Failed to parse notes.toml for view"
                );
                vec![]
            })
        } else {
            vec![]
        };
        Arc::new(notes)
    }

    pub fn file_set(&self) -> Result<Arc<HashSet<PathBuf>>> {
        if let Some(fs) = &self.cached_file_set {
            return Ok(Arc::clone(fs));
        }
        let _span = tracing::info_span!("batch_view_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        Ok(Arc::new(set))
    }

    pub fn call_graph(&self) -> Result<Arc<cqs::store::CallGraph>> {
        if let Some(g) = &self.cached_call_graph {
            return Ok(Arc::clone(g));
        }
        let _span = tracing::info_span!("batch_view_call_graph").entered();
        Ok(self.store.get_call_graph()?)
    }

    pub fn test_chunks(&self) -> Result<Arc<Vec<cqs::store::ChunkSummary>>> {
        if let Some(tc) = &self.cached_test_chunks {
            return Ok(Arc::clone(tc));
        }
        let _span = tracing::info_span!("batch_view_test_chunks").entered();
        Ok(self.store.find_test_chunks()?)
    }

    pub fn get_ref(&self, name: &str) -> Result<()> {
        get_ref_via_refs_lru(&self.refs, &self.config, name)
    }

    pub fn get_all_refs(&self) -> Result<Vec<Arc<ReferenceIndex>>> {
        get_all_refs_via_refs_lru(&self.refs, &self.config)
    }

    pub fn borrow_ref(&self, name: &str) -> Option<Arc<ReferenceIndex>> {
        let mut cache = self.refs.lock().unwrap_or_else(|p| p.into_inner());
        cache.get(name).map(Arc::clone)
    }

    /// Take a deep clone of the latest [`cqs::watch_status::WatchSnapshot`]
    /// the watch loop published. Reads through the shared `Arc<RwLock<...>>`,
    /// holding the read guard only long enough to clone the small struct out.
    /// Outside `cqs watch --serve` (e.g. one-shot `cqs batch`) returns the
    /// default `unknown` snapshot — the watch loop never ticks, so the field
    /// stays at its initial value.
    pub fn watch_snapshot(&self) -> cqs::watch_status::WatchSnapshot {
        self.watch_snapshot
            .read()
            .map(|guard| (*guard).clone())
            .unwrap_or_else(|p| (*p.into_inner()).clone())
    }

    /// Flip the shared one-shot reconcile flag. Returns `true` if the flag was
    /// already pending (caller can dedupe in log lines), `false` if this call
    /// set it. Either way the watch loop runs the reconcile on its next 100 ms
    /// tick.
    ///
    /// `Release` ordering is enough: the watch loop's matching `swap` uses
    /// `AcqRel`, so any state the daemon thread published before flipping
    /// the bit is visible to the loop when it observes the flip.
    pub fn request_reconcile(&self) -> bool {
        self.reconcile_signal
            .swap(true, std::sync::atomic::Ordering::Release)
    }

    /// Borrow the shared freshness notifier so a `wait_fresh` handler can park
    /// on it. Returns the `Arc` clone so the daemon thread can call
    /// `wait_until_fresh` without holding the BatchContext mutex (the wait can
    /// be minutes).
    pub fn fresh_notifier(&self) -> cqs::watch_status::SharedFreshNotifier {
        Arc::clone(&self.fresh_notifier)
    }

    /// Test-only helpers used by `dispatch_wait_fresh`'s unit suite to seed the
    /// shared snapshot. Production code reaches the snapshot through the watch
    /// loop's `publish_watch_snapshot`, not these accessors. `pub(crate)` keeps
    /// the test wiring out of the public API.
    #[cfg(test)]
    pub(crate) fn test_overwrite_watch_snapshot(&self, snap: cqs::watch_status::WatchSnapshot) {
        let mut guard = self
            .watch_snapshot
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *guard = snap;
    }

    #[cfg(test)]
    pub(crate) fn test_watch_snapshot_handle(&self) -> cqs::watch_status::SharedWatchSnapshot {
        Arc::clone(&self.watch_snapshot)
    }

    /// Build a [`cqs::daemon_translate::PingResponse`] from the snapshot.
    /// Mirrors `BatchContext::ping_snapshot` but reads through the shared
    /// Arc handles in the view.
    pub fn ping_snapshot(&self) -> cqs::daemon_translate::PingResponse {
        // Surface overflow as None (treated same as "missing mtime") instead
        // of silently wrapping past `i64::MAX`. Different shape from
        // `unix_secs_i64()` — reads file mtime, not wall-clock.
        // Slot-aware index resolution.
        let last_indexed_at = std::fs::metadata(cqs::resolve_index_db(&self.cqs_dir))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok());
        let splade_loaded = self
            .splade_encoder_slot
            .get()
            .map(|opt| opt.is_some())
            .unwrap_or(false);
        cqs::daemon_translate::PingResponse {
            model: self.model_config.name.clone(),
            dim: u32::try_from(self.model_config.dim).unwrap_or(u32::MAX),
            uptime_secs: self.started_at.elapsed().as_secs(),
            last_indexed_at,
            error_count: self.error_count.load(Ordering::Relaxed),
            total_queries: self.query_count.load(Ordering::Relaxed),
            splade_loaded,
            reranker_loaded: self.reranker_slot.get().is_some(),
        }
    }

    /// Invalidate all caches and reopen the store. The Refresh handler
    /// path. Daemon connections back-channel through `outer_lock`; stdin
    /// batch falls through and returns an error if no back-channel is
    /// available (refresh from stdin batch goes through BatchContext
    /// directly, not via this method).
    pub fn invalidate_via_outer(&self) -> Result<()> {
        match &self.outer_lock {
            Some(lock) => {
                let ctx = lock.lock().unwrap_or_else(|p| p.into_inner());
                ctx.invalidate()
            }
            None => Err(anyhow::anyhow!(
                "BatchView::invalidate_via_outer called without outer_lock — \
                 refresh from this surface should go through BatchContext directly"
            )),
        }
    }
}

// ─── SearchCtx impl: BatchView drives the shared query_core ──────────────────
//
// Phase 2b: the daemon search handler routes through the same
// `search::query::query_core` the CLI uses. `BatchView` produces the
// surface-agnostic resource surface the core needs; the differences from the
// CLI context (Arc snapshots vs borrows, ensure-then-borrow SPLADE priming) are
// erased here so the core stays single-implementation.
impl crate::cli::commands::search::search_ctx::SearchCtx for BatchView {
    fn store(&self) -> &Store<ReadOnly> {
        // The view holds the snapshot Arc as a field; lend a borrow out of it
        // (Arc derefs to Store) rather than cloning per accessor call.
        &self.store
    }

    fn cqs_dir(&self) -> &Path {
        &self.cqs_dir
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn embedder(&self) -> Result<&Embedder> {
        BatchView::embedder(self)
    }

    fn reranker(&self) -> Result<Arc<dyn cqs::Reranker>> {
        BatchView::reranker(self)
    }

    fn splade_encode(&self, query: &str) -> Option<cqs::splade::SparseVector> {
        // Daemon SPLADE: prime the inverted index first (the snapshot may have
        // been empty at checkout), then encode the query. Mirrors the
        // pre-refactor `dispatch_search` ordering: `ensure_splade_index()`
        // followed by `splade_encoder().encode()`.
        self.ensure_splade_index();
        self.splade_encoder()
            .and_then(|enc| match enc.encode(query) {
                Ok(sv) => Some(sv),
                Err(e) => {
                    tracing::warn!(error = %e, "SPLADE query encoding failed, falling back to cosine-only");
                    None
                }
            })
    }

    fn splade_index(&self) -> Option<crate::cli::commands::search::search_ctx::SpladeIndexRef<'_>> {
        // `splade_encode` already called `ensure_splade_index`; borrow the Arc
        // snapshot back as an owned handle.
        self.borrow_splade_index()
            .map(crate::cli::commands::search::search_ctx::SpladeIndexRef::Owned)
    }

    fn vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        BatchView::vector_index(self)
    }

    fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        BatchView::base_vector_index(self)
    }

    fn audit_state(&self) -> cqs::audit::AuditMode {
        BatchView::audit_state(self)
    }
}
