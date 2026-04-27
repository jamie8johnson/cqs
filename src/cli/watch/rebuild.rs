//! HNSW rebuild orchestration: in-flight rebuild state, backoff for
//! embedder init, model resolution, threshold helpers, and the
//! foreground/background rebuild paths.
//!
//! Carved out of `watch.rs`. The watch loop calls into this module to
//! kick off a background rebuild (`spawn_hnsw_rebuild`), drain a
//! completed one (`drain_pending_rebuild`), or fall back to retrying a
//! flaky embedder (`try_init_embedder` + `EmbedderBackoff`). The model
//! resolution helper lives here because it feeds the rebuild thread
//! and the watch loop's main `Embedder` from the same source of truth.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::hnsw::HnswIndex;
use cqs::store::Store;

use super::{WatchConfig, WatchState};

/// Full HNSW rebuild after this many incremental inserts to clean orphaned vectors.
/// Override with CQS_WATCH_REBUILD_THRESHOLD env var.
pub(super) fn hnsw_rebuild_threshold() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_WATCH_REBUILD_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    })
}
/// #1090: handle to an in-flight background HNSW rebuild.
///
/// The rebuild thread streams embeddings from a read-only Store opened on
/// the same `index.db`, builds a fresh `Owned` `HnswIndex`, and saves it to
/// disk before sending the index back through the channel. While the
/// thread runs, the watch loop continues to commit new chunks to SQLite —
/// any (id, embedding) pair indexed during the rebuild window is captured
/// in `delta` and replayed into the new index just before the atomic swap,
/// closing the TOCTOU between the rebuild thread's snapshot and `recv`.
pub(super) struct PendingRebuild {
    pub(super) rx: std::sync::mpsc::Receiver<RebuildOutcome>,
    /// P1.17 / #1124: each entry carries the chunk's `content_hash` alongside
    /// (id, embedding) so the swap-time drain can compare against the
    /// rebuild thread's snapshot. An id-only dedup would silently drop the
    /// fresh embedding for any chunk that was re-embedded mid-rebuild
    /// (snapshot has the OLD vector under the same id; delta has the NEW
    /// one) — the HNSW would carry the stale vector until the next
    /// threshold rebuild.
    pub(super) delta: Vec<(String, Embedding, String)>,
    pub(super) started_at: std::time::Instant,
    /// P2.71: held so daemon shutdown can `join` (or detect the thread is
    /// finished) instead of leaking a detached worker. `None` if the spawn
    /// itself failed — the channel disconnect path then handles cleanup.
    pub(super) handle: Option<std::thread::JoinHandle<()>>,
    /// P2.72: latched once `delta` exceeds `MAX_PENDING_REBUILD_DELTA`. When
    /// set, the drain path discards the rebuilt index instead of swapping
    /// (the missed embeddings would silently disappear); the next threshold
    /// rebuild reads fresh state from SQLite and recovers cleanly.
    pub(super) delta_saturated: bool,
}

/// P1.17 / #1124: the rebuild thread reports both the freshly-built
/// `HnswIndex` AND the per-id `content_hash` snapshot the build consumed.
/// The drain path needs the snapshot map to detect mid-rebuild
/// re-embeddings — without it, a hash-aware dedup would have to issue a
/// second SQL query (and lose snapshot consistency under concurrent
/// writers).
pub(crate) struct RebuildResult {
    pub index: HnswIndex,
    pub snapshot_hashes: std::collections::HashMap<String, String>,
}

/// P2.72: cap on per-rebuild delta size. A stale-rebuild that runs longer
/// than expected (very large index, slow disk) accumulates one entry per
/// chunk re-embedded by the watch loop. Without a cap a multi-GB embedding
/// vector backlog is possible — every entry is `Vec<f32>` of `dim` floats.
/// 5,000 entries × 1024 dim × 4 bytes ≈ 20 MB worst case, recoverable by the
/// next threshold rebuild's fresh SQLite scan.
pub(super) const MAX_PENDING_REBUILD_DELTA: usize = 5_000;

pub(super) type RebuildOutcome = Result<Option<RebuildResult>, anyhow::Error>;

/// Track exponential backoff state for embedder initialization retries.
///
/// On repeated failures, backs off from 0s to max 5 minutes between attempts
/// to avoid burning CPU retrying a broken ONNX model load every ~2s cycle.
pub(super) struct EmbedderBackoff {
    /// Number of consecutive failures
    pub(super) failures: u32,
    /// Instant when the next retry is allowed
    next_retry: std::time::Instant,
}

impl EmbedderBackoff {
    pub(super) fn new() -> Self {
        Self {
            failures: 0,
            next_retry: std::time::Instant::now(),
        }
    }

    /// Record a failure and compute the next retry time with exponential backoff.
    /// Backoff: 2^failures seconds, capped at 300s (5 min).
    pub(super) fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let delay_secs = 2u64.saturating_pow(self.failures).min(300);
        self.next_retry = std::time::Instant::now() + Duration::from_secs(delay_secs);
        warn!(
            failures = self.failures,
            next_retry_secs = delay_secs,
            "Embedder init failed, backing off"
        );
    }

    /// Reset backoff on success.
    pub(super) fn reset(&mut self) {
        self.failures = 0;
        self.next_retry = std::time::Instant::now();
    }

    /// Whether we should attempt initialization (backoff expired).
    pub(super) fn should_retry(&self) -> bool {
        std::time::Instant::now() >= self.next_retry
    }
}

/// Try to initialize the shared embedder, returning a reference from the
/// Arc-backed OnceLock. Deduplicates the 7-line pattern that appeared
/// twice in cmd_watch. Uses `backoff` to apply exponential backoff on
/// repeated failures (RM-24).
///
/// RM-V1.25-28: the OnceLock is shared with the daemon thread; whichever
/// side initializes first wins, and the other reuses the same Arc.
pub(super) fn try_init_embedder<'a>(
    embedder: &'a std::sync::OnceLock<std::sync::Arc<Embedder>>,
    backoff: &mut EmbedderBackoff,
    model_config: &ModelConfig,
) -> Option<&'a Embedder> {
    match embedder.get() {
        Some(e) => Some(e.as_ref()),
        None => {
            if !backoff.should_retry() {
                return None;
            }
            match Embedder::new(model_config.clone()) {
                Ok(e) => {
                    backoff.reset();
                    Some(embedder.get_or_init(|| std::sync::Arc::new(e)).as_ref())
                }
                Err(e) => {
                    warn!(error = %e, "Failed to initialize embedder");
                    backoff.record_failure();
                    None
                }
            }
        }
    }
}

/// Resolve the embedding model for the watch / daemon path, preferring the
/// model recorded in the open store's metadata over CLI flag / env / config.
///
/// Quick standalone read of `Store::stored_model_name()` — opened in
/// read-only mode and dropped before the caller's main store handle is built,
/// so this does not interfere with the watch loop's read-write store. The
/// extra open is cheap (a single SELECT against the `metadata` table) and
/// runs at most twice per `cmd_watch` invocation (once for the daemon
/// thread's `daemon_model_config`, once for the watch loop's
/// `cli.try_model_config()` override below).
///
/// See [`cqs::embedder::ModelConfig::resolve_for_query`] for the resolution
/// chain when no stored name is present (CLI > env > config > default).
pub(super) fn resolve_index_aware_model_for_watch(
    index_path: &std::path::Path,
    root: &std::path::Path,
    cli_model: Option<&str>,
) -> Result<ModelConfig> {
    let stored = match cqs::Store::open_readonly(index_path) {
        Ok(s) => s.stored_model_name(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Quick read-only open for stored_model_name() failed; \
                 falling back to CLI/env/config resolution"
            );
            None
        }
    };
    let project_config = cqs::config::Config::load(root);
    let resolved = ModelConfig::resolve_for_query(
        stored.as_deref(),
        cli_model,
        project_config.embedding.as_ref(),
    )
    .apply_env_overrides();
    tracing::info!(
        stored = stored.as_deref().unwrap_or("<none>"),
        resolved = %resolved.name,
        dim = resolved.dim,
        "Watch resolved index-aware model config"
    );
    Ok(resolved)
}
/// #1090: Spawn a background thread to rebuild the enriched HNSW from the
/// store and save it to disk. Returns a `PendingRebuild` whose `rx` will
/// receive the new `Owned` index (or an error) when the build completes.
///
/// The thread opens its own read-only Store on the same `index.db` so the
/// main watch loop's `&Store` isn't borrowed across thread boundaries.
/// SQLite WAL gives the thread a consistent read snapshot; new commits made
/// by the watch loop while the rebuild is in flight are tracked in the
/// returned `PendingRebuild::delta` (filled by the caller) and replayed
/// into the new index just before the swap, closing the TOCTOU.
///
/// `context` is logged at info on completion to help operators distinguish
/// startup-owned-swap rebuilds from threshold-triggered rebuilds.
pub(super) fn spawn_hnsw_rebuild(
    cqs_dir: PathBuf,
    index_path: PathBuf,
    expected_dim: usize,
    context: &'static str,
) -> PendingRebuild {
    let (tx, rx) = std::sync::mpsc::channel();
    let started_at = std::time::Instant::now();
    let span = tracing::info_span!(
        "hnsw_rebuild_bg",
        context,
        cqs_dir = %cqs_dir.display(),
    );
    let thread_result = std::thread::Builder::new()
        .name(format!("cqs-hnsw-rebuild-{}", context))
        .spawn(move || {
            let _enter = span.entered();
            let result: RebuildOutcome = (|| -> RebuildOutcome {
                let store =
                    cqs::Store::open_readonly_pooled(&index_path).map_err(anyhow::Error::from)?;
                if store.dim() != expected_dim {
                    anyhow::bail!(
                        "store dim ({}) does not match expected ({}); refusing rebuild",
                        store.dim(),
                        expected_dim
                    );
                }
                let enriched = crate::cli::commands::build_hnsw_index_owned(&store, &cqs_dir)?;
                // Phase 5: also rebuild the base (non-enriched) HNSW so the
                // dual-index router stays in sync. The base index is loaded
                // fresh from disk by search processes — no in-memory swap
                // needed. Best-effort: a base rebuild failure shouldn't block
                // the enriched swap, so log + continue.
                match crate::cli::commands::build_hnsw_base_index(&store, &cqs_dir) {
                    Ok(Some(n)) => tracing::info!(vectors = n, "base HNSW rebuilt in background"),
                    Ok(None) => tracing::debug!("base HNSW skipped (no embedding_base rows yet)"),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "base HNSW rebuild failed in background; router falls back to enriched-only"
                    ),
                }
                // P1.17 / #1124: package the (index, snapshot_hashes) pair
                // so the drain can detect mid-rebuild re-embeddings.
                Ok(enriched.map(|(index, snapshot_hashes)| RebuildResult {
                    index,
                    snapshot_hashes,
                }))
            })();
            let elapsed_ms = started_at.elapsed().as_millis();
            match &result {
                Ok(Some(r)) => tracing::info!(
                    vectors = r.index.len(),
                    elapsed_ms,
                    context,
                    "background HNSW rebuild complete"
                ),
                Ok(None) => tracing::info!(
                    elapsed_ms,
                    context,
                    "background HNSW rebuild: store empty, nothing to build"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    elapsed_ms,
                    context,
                    "background HNSW rebuild failed"
                ),
            }
            // Receiver may have been dropped if the daemon shut down — that's fine.
            let _ = tx.send(result);
        });
    let handle = match thread_result {
        Ok(h) => Some(h),
        Err(e) => {
            // Spawn failed (rare — only on resource exhaustion). Log and
            // return a PendingRebuild whose channel will hang up on first
            // poll, which the caller treats as "no rebuild in flight."
            tracing::warn!(error = %e, context, "Failed to spawn HNSW rebuild thread");
            None
        }
    };
    PendingRebuild {
        rx,
        delta: Vec::new(),
        started_at,
        handle,
        delta_saturated: false,
    }
}

/// #1090: Try to consume a completed background HNSW rebuild and swap it
/// into `state.hnsw_index`. Replays any chunks captured in
/// `pending.delta` into the new index before saving + swapping so chunks
/// committed during the rebuild window aren't dropped.
///
/// Behaviour:
/// - Channel ready, `Ok(Some(idx))`: replay delta → save → swap, clear pending.
/// - Channel ready, `Ok(None)`: store was empty when the thread ran; clear
///   pending without swapping. Next reindex cycle will spawn a fresh one.
/// - Channel ready, `Err(_)`: thread reported an error; clear pending so
///   the next threshold trigger can retry.
/// - Channel empty: rebuild still in flight; leave pending alone so the
///   caller continues to capture delta entries.
/// - Channel disconnected: spawn failed earlier or thread panicked; clear.
pub(super) fn drain_pending_rebuild(cfg: &WatchConfig, store: &Store, state: &mut WatchState) {
    let Some(pending) = state.pending_rebuild.as_mut() else {
        return;
    };
    let outcome = match pending.rx.try_recv() {
        Ok(o) => o,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            tracing::warn!("Background rebuild thread channel disconnected; clearing pending");
            state.pending_rebuild = None;
            return;
        }
    };
    let pending = state
        .pending_rebuild
        .take()
        .expect("pending_rebuild was Some when we held a borrow");

    match outcome {
        Ok(Some(RebuildResult {
            index: mut new_index,
            snapshot_hashes,
        })) => {
            // P2.72: if the delta saturated, the rebuilt index is missing
            // events that we dropped on the floor; swapping it in would
            // silently lose those chunks. Discard instead — the next
            // threshold rebuild reads fresh state from SQLite and recovers.
            if pending.delta_saturated {
                let elapsed_ms = pending.started_at.elapsed().as_millis();
                tracing::warn!(
                    elapsed_ms,
                    discarded_delta = pending.delta.len(),
                    "Discarding rebuilt HNSW: delta saturated during rebuild — \
                     next threshold rebuild will pick up changes from SQLite"
                );
                if !cfg.quiet {
                    println!(
                        "  HNSW index: rebuild discarded (delta saturated; {}ms wasted, will retry)",
                        elapsed_ms
                    );
                }
                return;
            }
            // P1.17 / #1124: replay captured delta — skip only entries
            // whose (id, content_hash) match the snapshot. An id that was
            // re-embedded mid-rebuild has a NEW hash in delta but the
            // snapshot baked in the OLD vector; the entry must replay so
            // the fresh embedding lands in the swapped HNSW. Pure id-only
            // dedup (the pre-fix shape) silently dropped these and left
            // the HNSW serving stale vectors until the next threshold
            // rebuild.
            //
            // Trade-off when an id IS re-embedded: `insert_batch` on
            // hnsw_rs adds a duplicate node (no deletion API). That's the
            // same orphan situation as the existing fast-incremental path
            // — search post-filters by SQLite, so the orphan is invisible
            // to callers. The next threshold rebuild cleans it up.
            let to_replay: Vec<(String, Embedding)> = pending
                .delta
                .into_iter()
                .filter(|(id, _, hash)| {
                    snapshot_hashes.get(id.as_str()).is_none_or(|sh| sh != hash)
                })
                .map(|(id, emb, _)| (id, emb))
                .collect();
            if !to_replay.is_empty() {
                let items: Vec<(String, &[f32])> = to_replay
                    .iter()
                    .map(|(id, emb)| (id.clone(), emb.as_slice()))
                    .collect();
                match new_index.insert_batch(&items) {
                    Ok(n) => {
                        tracing::info!(replayed = n, "Replayed delta into rebuilt HNSW before swap")
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        replayed_attempt = items.len(),
                        "Failed to replay delta into rebuilt HNSW; new chunks will surface on next rebuild"
                    ),
                }
            }
            if let Err(e) = new_index.save(cfg.cqs_dir, "index") {
                tracing::warn!(
                    error = %e,
                    "Failed to save rebuilt HNSW after delta replay; in-memory swap proceeds anyway"
                );
            } else {
                clear_hnsw_dirty_with_retry(
                    store,
                    cqs::HnswKind::Enriched,
                    "background_rebuild_swap",
                );
            }
            let elapsed_ms = pending.started_at.elapsed().as_millis();
            let n = new_index.len();
            state.hnsw_index = Some(new_index);
            state.incremental_count = 0;
            info!(
                vectors = n,
                elapsed_ms, "Background HNSW rebuild swapped in"
            );
            if !cfg.quiet {
                println!(
                    "  HNSW index: {} vectors (background rebuild swapped in, {}ms)",
                    n, elapsed_ms
                );
            }
        }
        Ok(None) => {
            tracing::debug!("Background rebuild reported empty store; cleared pending");
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Background HNSW rebuild failed; will retry on next threshold trigger"
            );
        }
    }
}

/// DS2-7: Clear the HNSW-dirty flag for `kind`, retrying once on a
/// transient error (e.g. `SQLITE_BUSY` when a concurrent writer is
/// finishing a transaction). If both attempts fail, emits a warn and
/// returns — the caller keeps the in-memory HNSW but the persistent
/// dirty flag stays `1`, forcing a full rebuild on the next daemon
/// start. The single retry is enough to absorb the common lock-window
/// race without extending the write-side lock hold time.
pub(super) fn clear_hnsw_dirty_with_retry(store: &Store, kind: cqs::HnswKind, context: &str) {
    if let Err(e1) = store.set_hnsw_dirty(kind, false) {
        tracing::debug!(
            error = %e1,
            kind = ?kind,
            context,
            "First clear-dirty attempt failed, retrying once"
        );
        if let Err(e2) = store.set_hnsw_dirty(kind, false) {
            tracing::warn!(
                first_error = %e1,
                retry_error = %e2,
                kind = ?kind,
                context,
                "Failed to clear HNSW dirty flag after retry — unnecessary rebuild on next daemon start"
            );
        }
    }
}
