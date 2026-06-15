//! `BatchView`: an owned-`Arc` snapshot of a `BatchContext`, plus the
//! handler-routing glue (`checkout_view_from_arc`, `dispatch_via_view`) and
//! the `SearchCtx` impl that drives the shared query core.
//!
//! Split out of the former monolithic `cli/batch/mod.rs` (issue #1691).

use super::*;

#[cfg(test)]
thread_local! {
    /// Per-test overlay override for the daemon-path handler tests. When set,
    /// [`BatchView::resolve_overlay`] returns it directly, bypassing the LRU /
    /// embedder / git-delta build so a test can drive `dispatch_callers` /
    /// `dispatch_callees` with a hand-built [`cqs::worktree_overlay::WorktreeOverlay`]
    /// and assert the `_meta.overlay_graph` marker's honesty.
    static TEST_OVERLAY_OVERRIDE: std::cell::RefCell<
        Option<Arc<cqs::worktree_overlay::WorktreeOverlay>>,
    > = const { std::cell::RefCell::new(None) };
}

/// Install a thread-local overlay override for the current test, returning a
/// guard that clears it on drop (so a reused test thread never leaks the
/// override into the next test).
#[cfg(test)]
pub(crate) fn set_test_overlay_override(
    overlay: Arc<cqs::worktree_overlay::WorktreeOverlay>,
) -> TestOverlayGuard {
    TEST_OVERLAY_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(overlay));
    TestOverlayGuard
}

/// RAII guard from [`set_test_overlay_override`]; clears the thread-local on drop.
#[cfg(test)]
pub(crate) struct TestOverlayGuard;

#[cfg(test)]
impl Drop for TestOverlayGuard {
    fn drop(&mut self) {
        TEST_OVERLAY_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    }
}

#[cfg(test)]
fn test_overlay_override() -> Option<Arc<cqs::worktree_overlay::WorktreeOverlay>> {
    TEST_OVERLAY_OVERRIDE.with(|cell| cell.borrow().clone())
}

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
                    "Cached reference stale (index.db changed) — evicting for reload"
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
                        "Cached reference stale (index.db changed) — evicting for reload"
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

/// Resolve a worktree overlay through the daemon's overlay LRU (result-trust
/// §3), building it on a miss and revalidating its fingerprint on a hit.
///
/// Mirrors [`get_ref_via_refs_lru`]'s load-outside-lock shape, with the
/// overlay-specific invalidator (per-entry fingerprint debounce) in place of
/// the reference's `is_stale` check:
///
/// 1. **Hit + fresh** — the entry was fingerprint-validated within
///    `debounce` → reuse it without re-running git (the WSL-latency guard: two
///    git spawns per query collapse to one per burst).
/// 2. **Hit + stale-stamp** — past the debounce → recompute the fingerprint
///    (discover the live delta, re-hash); on match, touch the stamp and reuse;
///    on mismatch, rebuild **outside the LRU lock** and `put` the fresh entry
///    (last-write-wins; identical fingerprints are idempotent).
/// 3. **Miss** — build outside the lock and `put`.
///
/// The build is the embedder-dependent cost; `build_overlay` discovers the
/// delta, opens an in-memory store, and parses+embeds the dirty files into it.
/// Returns `Ok(None)` for a clean worktree (nothing to overlay) and
/// `Err(DeltaTooLarge)` when the delta exceeds the file cap — the caller maps
/// the latter to `skipped-delta-too-large`.
///
/// `worktree_root` MUST already be validated (canonicalized + proven a
/// worktree of `parent_root`) by the caller — this function reads + embeds its
/// files, so an unvalidated path would be an arbitrary-directory read primitive
/// (the security seam, plan §8).
#[allow(clippy::too_many_arguments)]
pub(crate) fn get_overlay_via_lru<M>(
    overlays: &Mutex<lru::LruCache<PathBuf, Arc<super::OverlayCacheEntry>>>,
    worktree_root: &Path,
    parent_root: &Path,
    parser: &cqs::parser::Parser,
    embedder: &Embedder,
    parent_store: &Store<M>,
    global_cache: Option<&cqs::cache::EmbeddingCache>,
    debounce: std::time::Duration,
) -> Result<Option<Arc<cqs::worktree_overlay::WorktreeOverlay>>, cqs::worktree_overlay::OverlayError>
{
    let _span =
        tracing::info_span!("overlay_get_via_lru", worktree = %worktree_root.display()).entered();

    // Hit path: peek the entry; if its validation stamp is within the debounce
    // window, reuse without touching git. Clone the Arc out and drop the lock
    // before any git/build work.
    let cached: Option<Arc<super::OverlayCacheEntry>> = {
        let mut cache = overlays.lock().unwrap_or_else(|p| p.into_inner());
        cache.get(worktree_root).map(Arc::clone)
    };

    if let Some(entry) = &cached {
        let within_window = {
            let stamp = entry
                .last_validated
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            stamp.elapsed() < debounce
        };
        if within_window {
            tracing::debug!("overlay cache hit (within fingerprint debounce)");
            return Ok(Some(Arc::clone(&entry.overlay)));
        }

        // Past the debounce: recompute the fingerprint against the live
        // worktree state. A match means the delta is unchanged — touch the
        // stamp and reuse the cached store (no rebuild). A mismatch (or a
        // discovery error) falls through to a rebuild below.
        match cqs::worktree_overlay::discover_delta(worktree_root, parent_root) {
            Ok(delta) => {
                // Recompute the same fingerprint the build stamped — including
                // the notes-revision token, so a parent notes mutation since the
                // cached build registers as a fingerprint change (rebuild) and a
                // notes-unchanged re-validation still matches. The zero-token
                // fallback mirrors `build_overlay` so a read failure does not
                // spuriously diverge the two computations.
                let notes_revision = parent_store.notes_revision().unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "overlay re-validation: failed to read parent notes-revision — using zero token");
                    [0u8; 32]
                });
                let fp = cqs::worktree_overlay::fingerprint(worktree_root, &delta, &notes_revision);
                if fp == entry.overlay.fingerprint {
                    tracing::debug!(
                        "overlay fingerprint re-validated (unchanged) — reusing cached build"
                    );
                    let mut stamp = entry
                        .last_validated
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    *stamp = Instant::now();
                    return Ok(Some(Arc::clone(&entry.overlay)));
                }
                tracing::debug!("overlay fingerprint changed — rebuilding");
            }
            Err(cqs::worktree_overlay::OverlayError::DeltaTooLarge { count, cap }) => {
                // The worktree drifted past the cap since the cached build.
                // Surface the skip rather than serving a now-stale overlay.
                return Err(cqs::worktree_overlay::OverlayError::DeltaTooLarge { count, cap });
            }
            Err(e) => {
                tracing::warn!(error = %e, "overlay re-validation discover failed — rebuilding");
            }
        }
    }

    // Miss, or hit with a changed fingerprint: build outside the LRU lock.
    let built = crate::cli::worktree_overlay_build::build_overlay(
        worktree_root,
        parent_root,
        parser,
        embedder,
        parent_store,
        global_cache,
    )?;

    let Some(overlay) = built else {
        // Clean worktree → nothing to overlay. Drop any stale cached entry so a
        // later edit rebuilds rather than serving the empty/old delta.
        let mut cache = overlays.lock().unwrap_or_else(|p| p.into_inner());
        cache.pop(worktree_root);
        return Ok(None);
    };

    let overlay = Arc::new(overlay);
    let entry = Arc::new(super::OverlayCacheEntry {
        overlay: Arc::clone(&overlay),
        last_validated: Mutex::new(Instant::now()),
    });
    // Last-write-wins: a concurrent builder may have `put` an identical-
    // fingerprint entry meanwhile; overwriting is idempotent (same delta) and
    // cheap (the duplicate build is a few hundred chunks).
    overlays
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .put(worktree_root.to_path_buf(), entry);
    Ok(Some(overlay))
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
    /// Vector-index snapshot taken at checkout. When `None`, the accessor
    /// falls back to the shared cell (another view may have built and
    /// published since checkout), then to a fresh build from the snapshot
    /// store — which is published back into the shared cell (epoch-guarded)
    /// so subsequent checkouts snapshot it instead of rebuilding.
    pub(super) cached_vector_index: Option<Arc<dyn VectorIndex>>,
    pub(super) cached_base_vector_index: Option<Arc<dyn VectorIndex>>,
    pub(super) cached_call_graph: Option<Arc<cqs::store::CallGraph>>,
    pub(super) cached_test_chunks: Option<Arc<Vec<cqs::store::ChunkSummary>>>,
    pub(super) cached_notes: Option<Arc<Vec<cqs::note::Note>>>,
    pub(super) cached_file_set: Option<Arc<HashSet<PathBuf>>>,
    pub(super) cached_splade_index: Option<Arc<cqs::splade::index::SpladeIndex>>,
    /// Shared `Arc<Mutex<...>>` write-back cells aliasing the BatchContext's
    /// mutable caches. A view-side cache miss builds the value, then
    /// publishes it back through [`Self::publish_if_current`]; the
    /// BatchContext path picks the same value up on its next
    /// `checkout_view`. Cleared by `invalidate_mutable_caches`.
    pub(super) vector_index_cell: Arc<Mutex<Option<EpochCell<dyn VectorIndex>>>>,
    pub(super) base_vector_index_cell: Arc<Mutex<Option<EpochCell<dyn VectorIndex>>>>,
    pub(super) file_set_cell: Arc<Mutex<Option<EpochCell<HashSet<PathBuf>>>>>,
    pub(super) notes_cell: Arc<Mutex<Option<EpochCell<Vec<cqs::note::Note>>>>>,
    pub(super) splade_index_cell: Arc<Mutex<Option<EpochCell<cqs::splade::index::SpladeIndex>>>>,
    /// Shared cross-project cache cell. Unlike the other cells the view holds
    /// no checkout-time snapshot — the cached context is itself mutable
    /// (`Arc<Mutex<CrossProjectContext>>`), so [`Self::cross_project`] reads or
    /// builds it live under the same epoch / fingerprint / staleness guards.
    pub(super) cross_project_cell: Arc<Mutex<Option<super::context::CachedCrossProject>>>,
    /// Shared invalidation-epoch counter plus the value it held when this
    /// view was checked out. `publish_if_current` compares the two under
    /// the cell lock: any invalidation since checkout bumped the counter,
    /// so a value built from this view's (now stale) store snapshot is
    /// discarded instead of being published over the fresh invalidation.
    pub(super) invalidation_epoch: Arc<AtomicU64>,
    pub(super) checkout_epoch: u64,
    /// Shared `Arc<OnceLock<...>>` to the BatchContext embedder slot. Init
    /// from the view propagates to the BatchContext (and any other view
    /// holding the same Arc).
    pub(super) embedder_slot: Arc<OnceLock<Arc<Embedder>>>,
    pub(super) reranker_slot: Arc<OnceLock<Arc<dyn cqs::Reranker>>>,
    pub(super) splade_encoder_slot: Arc<OnceLock<Option<cqs::splade::SpladeEncoder>>>,
    /// Shared refs LRU.
    pub(super) refs: Arc<Mutex<lru::LruCache<String, Arc<ReferenceIndex>>>>,
    /// Shared worktree-overlay LRU (result-trust §3). Same `Arc<Mutex<LruCache>>`
    /// alias-the-context shape as `refs`. `SearchCtx::overlay()` resolves through
    /// this via `get_overlay_via_lru` when `overlay_request` is set.
    pub(super) overlays: Arc<Mutex<lru::LruCache<PathBuf, Arc<super::OverlayCacheEntry>>>>,
    /// The validated worktree root to overlay for the *current* dispatch, or
    /// `None` when no overlay was requested / the request failed validation.
    /// Set by the search handlers (`dispatch_search` / `dispatch_search_with_refs`)
    /// from the wire `--overlay-root` after canonicalize + `resolve_main_project_dir
    /// == root` validation, then read by `SearchCtx::overlay()` inside the shared
    /// core. A `RefCell` because the view is handed to the core as `&BatchView`
    /// but the handler must stamp the per-query request onto it first; the view
    /// is single-threaded per dispatch (handlers run outside the BatchContext
    /// lock on one connection thread), so interior mutability is sound.
    pub(super) overlay_request: RefCell<Option<PathBuf>>,
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

    /// Publish a freshly built cache value into a shared BatchContext cell —
    /// unless an invalidation ran since this view was checked out. The epoch
    /// is compared under the cell lock, and `invalidate_mutable_caches`
    /// bumps the counter before clearing the cells, so a build from a
    /// pre-invalidation store snapshot can never land after (and survive) a
    /// fresh invalidation. The discarded value still serves the in-flight
    /// dispatch: it matches the view's own store snapshot, so the current
    /// query stays internally consistent.
    fn publish_if_current<T: ?Sized>(
        &self,
        cell: &Mutex<Option<EpochCell<T>>>,
        value: &Arc<T>,
        slot: &'static str,
    ) {
        let mut guard = cell.lock().unwrap_or_else(|p| p.into_inner());
        if self.invalidation_epoch.load(Ordering::SeqCst) != self.checkout_epoch {
            tracing::debug!(
                slot,
                "invalidation ran since view checkout — discarding freshly built cache value"
            );
            return;
        }
        // Tag the published value with this view's `checkout_epoch` — the
        // generation its store snapshot belongs to. Every later read compares
        // the tag against its own checkout_epoch, so a value that lingers past
        // an invalidation (a deferred clear the (C) re-check below and the
        // sticky retry both raced and missed) is detected on read regardless of
        // whether any clear ever ran. This is the load-bearing half of the
        // interleaving-auditor fix; the (C) re-check stays as a best-effort
        // early clear.
        *guard = Some((self.checkout_epoch, Arc::clone(value)));
        // An invalidation may have bumped the epoch between the check above
        // and the store: it found this cell locked and deferred the clear to
        // the lock holder. Re-check and perform that deferred clear here,
        // while the lock is still held — otherwise the just-published stale
        // value would survive until the next sticky retry.
        if self.invalidation_epoch.load(Ordering::SeqCst) != self.checkout_epoch {
            *guard = None;
            tracing::debug!(slot, "invalidation raced the publish — clearing the cell");
        }
    }

    /// Read a shared cell — but only when its value belongs to this view's
    /// checkout generation. A value tagged with a different epoch belongs to a
    /// different index generation than this view's store snapshot; serving it
    /// against this store would mix generations (reindex reassigns chunk rowids,
    /// so results would be silently wrong). The tag comparison subsumes the
    /// older "epoch moved since checkout" check AND catches a stale residue that
    /// coexists with the current epoch (the deferred-clear race). On a miss the
    /// caller falls back to building from its own snapshot store, and the
    /// matching publish guard then discards that build if the epoch has since
    /// moved.
    fn read_cell_if_current<T: ?Sized>(
        &self,
        cell: &Mutex<Option<EpochCell<T>>>,
    ) -> Option<Arc<T>> {
        let guard = cell.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .as_ref()
            .filter(|(epoch, _)| *epoch == self.checkout_epoch)
            .map(|(_, value)| Arc::clone(value))
    }

    /// Vector index for this snapshot. Falls back to the shared cell (a
    /// sibling view may have built it since checkout), then to a fresh build
    /// from the snapshot store — published back into the shared cell so the
    /// next checkout snapshots it instead of rebuilding from disk.
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
        {
            // Hand-rolled (not `read_cell_if_current`) because of the poison
            // handling: a poisoned value is nulled out under the same guard
            // regardless of epoch — leaving it would let a later reader or
            // checkout snapshot revive a dead CUDA context — while serving a
            // healthy value is epoch-tag-gated like every other cell read.
            let mut guard = self
                .vector_index_cell
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some((epoch, arc)) = guard.as_ref() {
                if arc.is_poisoned() {
                    tracing::warn!(
                        name = arc.name(),
                        "Poisoned vector index in shared cell — discarding"
                    );
                    *guard = None;
                } else if *epoch == self.checkout_epoch {
                    return Ok(Some(Arc::clone(arc)));
                }
            }
        }
        let _span = tracing::info_span!("batch_view_vector_index_init").entered();
        let idx = build_vector_index(&self.store, &self.cqs_dir, self.config.ef_search)?;
        let result = idx.map(|boxed| -> Arc<dyn VectorIndex> { boxed.into() });
        if let Some(arc) = &result {
            self.publish_if_current(&self.vector_index_cell, arc, "hnsw");
        }
        Ok(result)
    }

    pub fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        if let Some(arc) = &self.cached_base_vector_index {
            return Ok(Some(Arc::clone(arc)));
        }
        if let Some(arc) = self.read_cell_if_current(&self.base_vector_index_cell) {
            return Ok(Some(arc));
        }
        let _span = tracing::info_span!("batch_view_base_vector_index_init").entered();
        let idx = crate::cli::build_base_vector_index(&self.store, &self.cqs_dir)?;
        let result = idx.map(|boxed| -> Arc<dyn VectorIndex> { boxed.into() });
        if let Some(arc) = &result {
            self.publish_if_current(&self.base_vector_index_cell, arc, "base_hnsw");
        }
        Ok(result)
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
        // Epoch-gated: a cell populated after an invalidation belongs to a
        // newer generation than this view's snapshot store — don't treat it
        // as "already ensured" for this view. The stale-view rebuild below
        // is then discarded by the publish guard; that dispatch falls back
        // to dense-only rather than mixing generations.
        if self.read_cell_if_current(&self.splade_index_cell).is_some() {
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
        // Epoch-guarded publish: if an invalidation cleared the cell while
        // the (potentially long) build above ran, the index was built from a
        // stale snapshot and must not be re-published over the clear.
        self.publish_if_current(&self.splade_index_cell, &Arc::new(idx), "splade_index");
    }

    pub fn borrow_splade_index(&self) -> Option<Arc<cqs::splade::index::SpladeIndex>> {
        // Prefer the snapshot taken at checkout; fall back to the live
        // cell so a freshly populated index (via `ensure_splade_index`
        // during this dispatch) is observable without re-checkout. The
        // cell read is epoch-gated — a post-invalidation cell value would
        // mix generations with this view's store snapshot.
        if let Some(arc) = &self.cached_splade_index {
            return Some(Arc::clone(arc));
        }
        self.read_cell_if_current(&self.splade_index_cell)
    }

    /// Get the cached cross-project context, building it on a miss and caching
    /// it in the shared cell for subsequent requests.
    ///
    /// Returns `Arc<Mutex<CrossProjectContext>>` so the daemon's cross-project
    /// graph dispatchers get the `&mut CrossProjectContext` the cross cores
    /// require (lock the inner mutex) without holding any BatchContext lock.
    ///
    /// # Staleness
    ///
    /// A cached context is served only when all three hold:
    ///
    /// 1. **Epoch tag matches.** The cached entry carries the `checkout_epoch`
    ///    it was published under (`published_epoch`); a populated cell tagged
    ///    with a different generation than this view's store snapshot is
    ///    discarded (same tag contract as every `EpochCell`). Gating on the
    ///    baked-in tag rather than the live counter rejects a deferred-clear
    ///    residue that lingers past an invalidation even on the WAL
    ///    `data_version` path where `is_stale()` is blind. A local reindex
    ///    invalidates via the `CROSS_PROJECT` slot, so the merged graph — which
    ///    folds in the local project's edges — is never served stale.
    /// 2. **Fingerprint matches.** The cached entry carries the references-
    ///    config fingerprint it was built from; a `.cqs.toml` / `slot.toml`
    ///    references edit (visible after `CONFIG_RELOAD_INTERVAL`) moves the
    ///    fingerprint and forces a rebuild.
    /// 3. **Underlying DBs unchanged.** `CrossProjectContext::is_stale()` is
    ///    checked before serving so a `cqs ref update <name>` (which rewrites a
    ///    reference's `index.db` without touching the primary file or bumping
    ///    the epoch) forces a reload.
    ///
    /// A freshly built context is published back into the cell under the same
    /// epoch guard as the other write-back caches: an invalidation that ran
    /// during the (potentially slow) build discards the publish rather than
    /// resurrecting a stale generation.
    pub fn cross_project(&self) -> Result<Arc<Mutex<cqs::cross_project::CrossProjectContext>>> {
        let current_fingerprint =
            cqs::cross_project::CrossProjectContext::config_fingerprint(&self.config.references);

        // Fast path: serve the cached context when epoch, fingerprint, and the
        // underlying DB identities all still hold.
        {
            let guard = self
                .cross_project_cell
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = guard.as_ref() {
                // Gate on the value's TAG (`published_epoch`), not the live
                // `invalidation_epoch` counter. A residue from a superseded
                // generation carries the older tag and is rejected here even
                // when the live counter has caught up to this reader's
                // `checkout_epoch` — the deferred-clear-residue interleaving the
                // bare-counter load missed on the WAL `data_version` path, where
                // `is_stale()` is blind. Mirrors `read_cell_if_current`.
                let epoch_ok = cached.published_epoch == self.checkout_epoch;
                let fingerprint_ok = cached.fingerprint == current_fingerprint;
                // `is_stale` stats each store's index.db; cheap relative to a
                // full reopen + graph rebuild, and only runs on the cache-hit
                // path.
                let dbs_fresh = !cached
                    .ctx
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_stale();
                if epoch_ok && fingerprint_ok && dbs_fresh {
                    return Ok(Arc::clone(&cached.ctx));
                }
                tracing::debug!(
                    epoch_ok,
                    fingerprint_ok,
                    dbs_fresh,
                    "cross-project cache miss — rebuilding context"
                );
            }
        }

        // Build outside the cell lock (reopening stores is slow).
        let _span = tracing::info_span!("batch_view_cross_project_init").entered();
        let built = cqs::cross_project::CrossProjectContext::from_config(&self.root)?;
        let arc = Arc::new(Mutex::new(built));

        // Publish back, epoch-guarded. An invalidation since checkout means a
        // newer generation arrived; don't overwrite it with this stale build.
        {
            let mut guard = self
                .cross_project_cell
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if self.invalidation_epoch.load(Ordering::SeqCst) == self.checkout_epoch {
                // Tag the published value with this view's `checkout_epoch` —
                // the generation its store snapshot belongs to. Every later read
                // compares the tag against its own checkout_epoch, so a value
                // that lingers past an invalidation (a deferred clear the
                // re-check below and the sticky retry both raced and missed) is
                // detected on read regardless of the live counter. This is the
                // load-bearing half of the fix; the re-check is best-effort
                // early clear. Mirrors `publish_if_current`.
                *guard = Some(super::context::CachedCrossProject {
                    ctx: Arc::clone(&arc),
                    fingerprint: current_fingerprint,
                    published_epoch: self.checkout_epoch,
                });
                // An invalidation may have bumped the epoch between the check
                // above and the store: it found this cell locked and deferred
                // the clear to the lock holder. Re-check and perform that
                // deferred clear here, while the lock is still held — otherwise
                // the just-published stale value would survive until the next
                // sticky retry.
                if self.invalidation_epoch.load(Ordering::SeqCst) != self.checkout_epoch {
                    *guard = None;
                    tracing::debug!(
                        "invalidation raced the cross-project publish — clearing the cell"
                    );
                }
            } else {
                tracing::debug!(
                    "invalidation ran since checkout — not publishing cross-project context"
                );
            }
        }
        Ok(arc)
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
        if let Some(notes) = self.read_cell_if_current(&self.notes_cell) {
            return notes;
        }
        // Snapshot and cell were both empty — load once from disk and
        // publish back so subsequent dispatches reuse the parse.
        let notes_path = self.root.join("docs/notes.toml");
        let notes = if notes_path.exists() {
            match cqs::note::parse_notes(&notes_path) {
                Ok(notes) => notes,
                // Split absent-file (TOCTOU after the .exists() check above)
                // from genuine parse failures, and include the path in the
                // warn so the journal isn't ambiguous about which notes file
                // failed.
                Err(e) => {
                    if matches!(
                        &e,
                        cqs::NoteError::Io(io_err)
                            if io_err.kind() == std::io::ErrorKind::NotFound
                    ) {
                        tracing::debug!(
                            path = %notes_path.display(),
                            "notes.toml disappeared between exists() and parse — treating as empty"
                        );
                    } else {
                        tracing::warn!(
                            path = %notes_path.display(),
                            error = %e,
                            "Failed to parse notes.toml for view"
                        );
                    }
                    vec![]
                }
            }
        } else {
            vec![]
        };
        let arc = Arc::new(notes);
        self.publish_if_current(&self.notes_cell, &arc, "notes_cache");
        arc
    }

    pub fn file_set(&self) -> Result<Arc<HashSet<PathBuf>>> {
        if let Some(fs) = &self.cached_file_set {
            return Ok(Arc::clone(fs));
        }
        if let Some(fs) = self.read_cell_if_current(&self.file_set_cell) {
            return Ok(fs);
        }
        let _span = tracing::info_span!("batch_view_file_set").entered();
        let exts: Vec<&str> = cqs::language::REGISTRY.supported_extensions().collect();
        let files = cqs::enumerate_files(&self.root, &exts, false)?;
        let set: HashSet<PathBuf> = files.into_iter().collect();
        let arc = Arc::new(set);
        self.publish_if_current(&self.file_set_cell, &arc, "file_set");
        Ok(arc)
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

    /// Validate a client-supplied `--overlay-root` and, if it passes, stamp it
    /// onto `overlay_request` for the current dispatch (read back by
    /// `SearchCtx::overlay()`).
    ///
    /// **Security seam (plan §8).** The daemon's cwd is the parent project and
    /// the wire request carries an arbitrary `--overlay-root` string; an
    /// unvalidated path would let any socket client name a directory whose
    /// files the daemon then reads + embeds. Validation:
    ///
    /// 1. `dunce::canonicalize` the path (rejects non-existent paths and
    ///    normalizes symlinks / `..` so the membership check below compares the
    ///    real path, not a name that merely *spells* like a worktree).
    /// 2. Cheap pre-check (fast reject, NOT the security gate): require
    ///    `resolve_main_project_dir(canonical) == self.root`. This trusts the
    ///    worktree's own `.git` → `commondir` chain (which the socket client
    ///    controls and can forge), so it filters obvious foreign / non-worktree
    ///    inputs but is never relied on alone.
    /// 3. **The gate — authoritative registry membership.** Earlier
    ///    file-parsing checks (forward gitdir-under-`.git/worktrees/`, then the
    ///    `<gitdir>/gitdir` back-pointer) were each defeated at the
    ///    symlink-following path layer: a `.git` symlink to a real registered
    ///    worktree's `.git` makes BOTH the forward resolution and the
    ///    back-pointer follow through to the real worktree, so the masquerade
    ///    passes while the daemon still enumerates the ATTACKER tree's files.
    ///    Instead, query git's OWN registry — `git -C <self.root> worktree
    ///    list --porcelain`, rooted at the daemon-controlled served root, NOT
    ///    the attacker's tree — and require the canonical `overlay_root` to be a
    ///    member. A symlink/forgery masquerade is invisible to that registry, so
    ///    it is absent and rejected; this single check subsumes the forged-gitdir
    ///    hijack, the symlinked-`.git` masquerade, and the unregistered-forgery
    ///    case. A real `git worktree add` worktree is listed and accepted.
    ///
    /// Mirrors the overlay path's existing `git -C ...` posture (`discover_delta`
    /// already shells git): a git-invocation failure rejects loudly with a wire
    /// error rather than silently degrading. Returns an error the caller
    /// surfaces over the socket; on success the request is stamped and `Ok(())`.
    pub(in crate::cli) fn set_validated_overlay_request(&self, overlay_root: &Path) -> Result<()> {
        let _span = tracing::info_span!(
            "overlay_validate_root",
            requested = %overlay_root.display()
        )
        .entered();

        let canonical = dunce::canonicalize(overlay_root).map_err(|e| {
            tracing::warn!(error = %e, "overlay-root canonicalize failed — rejecting");
            anyhow::anyhow!(
                "overlay-root {} is not a readable path: {e}",
                overlay_root.display()
            )
        })?;

        let main = cqs::worktree::resolve_main_project_dir(&canonical);
        let root_canonical = dunce::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());

        // Cheap pre-check (fast reject, NOT the security gate): the requested
        // path must at least resolve its main project to the served root. This
        // is forgeable (it trusts the worktree's own `.git` → `commondir`
        // chain), so it cannot be trusted alone — it only filters obvious
        // foreign/non-worktree inputs before the authoritative query below.
        match main {
            Some(m) if m == root_canonical => {}
            other => {
                tracing::warn!(
                    requested = %canonical.display(),
                    resolved_main = ?other,
                    served_root = %root_canonical.display(),
                    "overlay-root is not a worktree of this project — rejecting"
                );
                anyhow::bail!(
                    "overlay-root {} is not a worktree of the served project {}",
                    canonical.display(),
                    root_canonical.display()
                )
            }
        }

        // THE GATE — authoritative membership in git's own registry. We query
        // `git -C <served_root> worktree list` rooted at the DAEMON-controlled
        // served root, not the attacker's tree, and require the canonical
        // overlay_root to be one of the worktrees git itself tracks. This
        // subsumes every file-parsing bypass: a forged `.git` that points at a
        // real registered gitdir (Attack A), or a `.git` symlink to a real
        // worktree's `.git` (Attack B), is a masquerade git's registry never
        // lists, so it is absent → rejected. The unregistered-forgery case is
        // likewise absent. A real `git worktree add` worktree IS listed → kept.
        let registered =
            cqs::worktree_overlay::registered_worktrees(&root_canonical).map_err(|e| {
                tracing::warn!(
                    requested = %canonical.display(),
                    served_root = %root_canonical.display(),
                    error = %e,
                    "could not enumerate the served project's worktrees — rejecting overlay-root"
                );
                anyhow::anyhow!(
                    "overlay-root {} could not be validated against the served project {}: {e}",
                    canonical.display(),
                    root_canonical.display()
                )
            })?;

        if registered.contains(&canonical) {
            tracing::debug!(
                worktree = %canonical.display(),
                "overlay-root validated (registered worktree per git worktree list)"
            );
            *self.overlay_request.borrow_mut() = Some(canonical);
            Ok(())
        } else {
            tracing::warn!(
                requested = %canonical.display(),
                served_root = %root_canonical.display(),
                registered_count = registered.len(),
                "overlay-root is not a registered worktree of the served project \
                 (absent from git worktree list — forged/symlinked/unregistered) — rejecting"
            );
            anyhow::bail!(
                "overlay-root {} is not a registered worktree of the served project {}",
                canonical.display(),
                root_canonical.display()
            )
        }
    }

    /// Resolve the worktree overlay for the current dispatch through the daemon
    /// overlay LRU, building + caching it on a miss. Returns `None` when no
    /// overlay was requested/validated for this query, when the worktree is
    /// clean (no delta), or when the delta exceeds the file cap — the last case
    /// also records the `skipped-delta-too-large` envelope meta so the agent
    /// knows the result reflects the parent index.
    ///
    /// The embedder + a fresh parser + the parent's embedding cache drive
    /// `build_overlay`. The parser is created per call (it is cheap — the watch
    /// hot path does the same); the cache is the parent project's
    /// `embeddings_cache.db` (the intentional cross-boundary cache write,
    /// documented in `worktree_overlay_build`).
    fn resolve_overlay(&self) -> Option<Arc<cqs::worktree_overlay::WorktreeOverlay>> {
        // Test seam: a directly-injected overlay short-circuits the LRU /
        // embedder / git-delta build, so daemon-path handler tests can exercise
        // the marker-honesty gate without a real worktree + CPU model.
        // Thread-local because the override is per-test and the view is handed
        // to handlers as `&BatchView` (no `&mut`); single-threaded per dispatch.
        #[cfg(test)]
        if let Some(ov) = test_overlay_override() {
            return Some(ov);
        }
        let worktree_root = self.overlay_request.borrow().clone()?;
        let _span = tracing::info_span!(
            "batch_view_resolve_overlay",
            worktree = %worktree_root.display()
        )
        .entered();

        let embedder = match self.embedder() {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "overlay skipped: embedder unavailable");
                return None;
            }
        };
        let parser = match cqs::parser::Parser::new() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "overlay skipped: parser init failed");
                return None;
            }
        };

        // Parent project's embedding cache (best-effort: a cache open failure
        // just means a slower build, never a skipped overlay).
        let cache_path = cqs::cache::EmbeddingCache::project_default_path(&self.cqs_dir);
        let cache = cqs::cache::EmbeddingCache::open(&cache_path).ok();

        match get_overlay_via_lru(
            &self.overlays,
            &worktree_root,
            &self.root,
            &parser,
            embedder,
            &self.store,
            cache.as_ref(),
            super::overlay_fp_debounce(),
        ) {
            Ok(Some(ov)) => {
                tracing::debug!(
                    files = ov.stats.files_in_delta,
                    chunks = ov.stats.chunks_indexed,
                    build_ms = ov.stats.build_ms,
                    "overlay resolved"
                );
                Some(ov)
            }
            Ok(None) => {
                tracing::debug!("overlay: clean worktree, serving parent index");
                None
            }
            Err(cqs::worktree_overlay::OverlayError::DeltaTooLarge { count, cap }) => {
                tracing::warn!(count, cap, "overlay skipped: delta too large");
                cqs::worktree_overlay::set_overlay_meta(
                    cqs::worktree_overlay::OverlayMeta::SkippedDeltaTooLarge,
                );
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "overlay build failed; serving parent index");
                None
            }
        }
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

    fn references(&self) -> Result<Vec<Arc<ReferenceIndex>>> {
        // The daemon hands back its LRU-cached `Arc<ReferenceIndex>` snapshots
        // directly — no per-call config load.
        self.get_all_refs()
    }

    fn reference_by_name(&self, name: &str) -> Result<Arc<ReferenceIndex>> {
        // Resolve through the same LRU `references()` uses: `get_ref` primes
        // the cache (with the `is_stale` self-heal that reloads after
        // `cqs ref update`), `borrow_ref` hands back the cached Arc. Keeps the
        // two seam accessors on one config snapshot + one staleness policy,
        // and avoids reloading the reference store + HNSW per request.
        self.get_ref(name)?;
        self.borrow_ref(name).ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{name}' evicted from cache between prime and borrow — retry the query"
            )
        })
    }

    fn overlay(&self) -> Option<Arc<cqs::worktree_overlay::WorktreeOverlay>> {
        // The FIRST production `Some` for this seam (the trait default is
        // `None`; the CLI surface stays `None` in phase 1). Resolved from the
        // per-dispatch `overlay_request` the search handler validated +
        // stamped; `None` here means no overlay was requested for this query.
        self.resolve_overlay()
    }
}
