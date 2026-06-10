//! Slot-parallel reindex: delta propagation to sibling slots.
//!
//! A save event reindexes the active slot synchronously in the debounce
//! cycle. Without propagation, every other slot under `.cqs/slots/`
//! drifts until a manual `cqs index --slot X`. This module keeps sibling
//! slots converged by re-using the active cycle's changed-file set:
//!
//! 1. **One event pipeline.** The active reindex computes the changed
//!    files; [`SiblingSet::enqueue`] copies that delta into a per-slot
//!    in-memory queue. Siblings never re-scan the tree on their own.
//! 2. **Active slot keeps absolute priority.** Sibling drains run from
//!    the watch loop's idle (recv-timeout) arm, strictly after the
//!    active flush, one slot per tick — a continuous active event
//!    stream starves sibling drains by construction, which is the
//!    intended ordering.
//! 3. **Same-model siblings are pure cache hits by construction.** The
//!    active reindex embeds changed chunks and writes them back to the
//!    project-scoped global cache keyed `(canonical_hash,
//!    model_fingerprint)`. A sibling sharing the active fingerprint then
//!    drains through `resolve_reuse` (inside [`reindex_files`]) at 100%
//!    hit rate — SQLite writes only, no GPU inference. Same-model
//!    siblings are therefore drain-eligible as soon as their queue is
//!    non-empty.
//! 4. **Foreign-model slots batch with hysteresis and are opt-in.** A
//!    different-fingerprint slot costs real inference plus an embedder
//!    session load. Deltas accumulate until `CQS_WATCH_FOREIGN_BATCH_FILES`
//!    files or `CQS_WATCH_FOREIGN_BATCH_SECS` seconds, then drain with
//!    exactly one embedder load (built at drain start, dropped at drain
//!    end). Entirely inert unless `CQS_WATCH_ALL_SLOTS=1`. Embedding
//!    work is serialized by construction — all drains run on the watch
//!    loop thread.
//! 5. **Durability via reconcile, not new machinery.** The queues are
//!    in-memory; [`SiblingSet::reconcile_siblings`] runs the same
//!    fingerprint reconciliation as the active slot against every
//!    propagated sibling on the periodic reconcile tick, so a daemon
//!    killed mid-drain converges within the reconcile interval.
//! 6. **Failure isolation.** A slot that fails (missing index, stale
//!    schema, locked DB) is marked errored and skipped; its queue is
//!    retained and its `last_error` surfaces in `cqs status --watch`.
//!    The reconcile tick clears the errored mark so drains retry on the
//!    reconcile cadence rather than hot-looping.
//!
//! Sibling drains do NOT maintain sibling HNSW graphs: the drain marks
//! the sibling's HNSW dirty flags and leaves them set, so search against
//! that slot uses its existing dirty-flag fallback until the next
//! rebuild against that slot. Chunk/FTS/call-graph state is what
//! converges here.
//!
//! Slots created after daemon startup are not discovered until restart —
//! same contract as slot promotion.

use super::*;
use cqs::watch_status::{FreshnessState, ReindexLatency, SlotWatchStatus, WatchErrorInfo};

/// Kill-switch for sibling propagation as a whole.
/// `CQS_WATCH_SIBLING_SLOTS=0` disables it; default on.
pub(super) fn sibling_propagation_enabled() -> bool {
    std::env::var("CQS_WATCH_SIBLING_SLOTS").as_deref() != Ok("0")
}

/// Tunables for sibling propagation, resolved from env once at daemon
/// startup. Kept as plain data so the drain-due decision is a pure
/// function unit tests can drive without touching the process env.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SiblingPolicy {
    /// Propagate to foreign-model slots (`CQS_WATCH_ALL_SLOTS=1`).
    /// Default false — foreign slots are inert.
    pub foreign_enabled: bool,
    /// Foreign-slot hysteresis: drain when the queue reaches this many
    /// files (`CQS_WATCH_FOREIGN_BATCH_FILES`, default 32).
    pub batch_files: usize,
    /// Foreign-slot hysteresis: drain when the oldest queued delta has
    /// waited this many seconds (`CQS_WATCH_FOREIGN_BATCH_SECS`,
    /// default 300).
    pub batch_secs: u64,
}

impl SiblingPolicy {
    pub(super) fn from_env() -> Self {
        Self {
            foreign_enabled: std::env::var("CQS_WATCH_ALL_SLOTS").as_deref() == Ok("1"),
            batch_files: cqs::limits::parse_env_usize_clamped(
                "CQS_WATCH_FOREIGN_BATCH_FILES",
                32,
                1,
                100_000,
            ),
            batch_secs: std::env::var("CQS_WATCH_FOREIGN_BATCH_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&s| s > 0)
                .unwrap_or(300),
        }
    }
}

/// Model relationship between a sibling slot and the active slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SiblingKind {
    /// Same embedding model as the active slot — drains are pure cache
    /// hits (the active cycle wrote the embeddings back to the global
    /// cache before the sibling drain runs).
    SameModel,
    /// Different model. Requires its own embedder session per drain;
    /// batched with hysteresis and gated on `CQS_WATCH_ALL_SLOTS`.
    Foreign { model: String },
}

/// One tracked sibling slot plus its in-memory delta queue.
pub(super) struct SiblingSlot {
    pub(super) name: String,
    /// `.cqs/slots/<name>/` — lock scope for drains.
    slot_dir: PathBuf,
    /// `.cqs/slots/<name>/index.db`.
    index_path: PathBuf,
    kind: SiblingKind,
    /// Tracked but not propagated to (foreign model without the
    /// opt-in). Inert slots never accumulate queue entries and report
    /// `unknown` freshness.
    inert: bool,
    /// Relative paths pending propagation to this slot.
    queue: HashSet<PathBuf>,
    /// When the oldest still-queued delta arrived — drives the
    /// foreign-slot time hysteresis. Cleared on drain.
    first_enqueued: Option<std::time::Instant>,
    /// Marked on drain failure; skipped until the next reconcile pass
    /// clears it (the reconcile cadence is the retry cadence).
    errored: bool,
    /// Sticky most-recent error for `cqs status --watch`.
    last_error: Option<WatchErrorInfo>,
    last_reindex: Option<ReindexLatency>,
    last_synced_at: Option<i64>,
    /// True once a drain or reconcile pass has run against this slot —
    /// before that, an empty queue means "unverified", not "fresh".
    verified: bool,
}

impl SiblingSlot {
    fn record_error(&mut self, message: String) {
        tracing::warn!(slot = %self.name, error = %message, "sibling slot error");
        self.last_error = Some(WatchErrorInfo {
            at_unix_secs: cqs::unix_secs_i64().unwrap_or(0),
            message,
        });
    }

    /// Put a drained-but-unprocessed file set back on the queue so a
    /// transient failure (lock held, embedder unavailable) retries later.
    fn requeue(&mut self, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        self.queue.extend(files);
        if self.first_enqueued.is_none() {
            self.first_enqueued = Some(std::time::Instant::now());
        }
    }

    fn status_state(&self) -> FreshnessState {
        if self.inert {
            FreshnessState::Unknown
        } else if self.errored || !self.queue.is_empty() {
            FreshnessState::Stale
        } else if self.verified {
            FreshnessState::Fresh
        } else {
            FreshnessState::Unknown
        }
    }
}

/// Outcome of one sibling drain, returned for logging and test
/// accounting. `embedded` counts chunks that required a fresh embedder
/// inference — zero for a same-model sibling whose delta was fully
/// served by the global cache (the gate property of the design).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DrainOutcome {
    pub(super) slot: String,
    pub(super) files: usize,
    pub(super) chunks: usize,
    pub(super) embedded: usize,
}

/// All sibling slots tracked by this daemon, discovered once at startup.
pub(super) struct SiblingSet {
    slots: Vec<SiblingSlot>,
    policy: SiblingPolicy,
    /// Round-robin cursor so one perpetually-due slot can't starve the
    /// others when several are due across consecutive idle ticks.
    cursor: usize,
}

/// Pure foreign-slot hysteresis decision: drain when the queue is large
/// enough OR the oldest entry has waited long enough.
pub(super) fn foreign_drain_due(
    queue_len: usize,
    first_enqueued: Option<std::time::Instant>,
    policy: &SiblingPolicy,
) -> bool {
    if queue_len == 0 {
        return false;
    }
    queue_len >= policy.batch_files
        || first_enqueued.is_some_and(|t| t.elapsed().as_secs() >= policy.batch_secs)
}

impl SiblingSet {
    /// A set that tracks nothing — used when propagation is disabled or
    /// the project has a single slot.
    pub(super) fn empty() -> Self {
        Self {
            slots: Vec::new(),
            policy: SiblingPolicy::from_env(),
            cursor: 0,
        }
    }

    /// Discover sibling slots under `.cqs/slots/`, classify each by
    /// model relative to the active slot's resolved config, and build
    /// the tracking set. Slots without an index, or whose model cannot
    /// be determined, are tracked as errored so `cqs status --watch`
    /// surfaces them rather than silently skipping.
    pub(super) fn discover(
        project_cqs_dir: &Path,
        active_slot: &str,
        active_model: &ModelConfig,
        policy: SiblingPolicy,
    ) -> Self {
        let _span = tracing::info_span!("sibling_slots_discover", active = active_slot).entered();

        if !sibling_propagation_enabled() {
            tracing::info!("CQS_WATCH_SIBLING_SLOTS=0 — sibling slot propagation disabled");
            return Self {
                slots: Vec::new(),
                policy,
                cursor: 0,
            };
        }

        let names = match cqs::slot::list_slots(project_cqs_dir) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "sibling discovery: list_slots failed — propagation disabled this session");
                return Self {
                    slots: Vec::new(),
                    policy,
                    cursor: 0,
                };
            }
        };

        let mut slots = Vec::new();
        for name in names {
            if name == active_slot {
                continue;
            }
            let slot_dir = cqs::resolve_slot_dir(project_cqs_dir, &name);
            let index_path = slot_dir.join(cqs::INDEX_DB_FILENAME);
            let mut slot = SiblingSlot {
                name: name.clone(),
                slot_dir,
                index_path,
                kind: SiblingKind::SameModel,
                inert: false,
                queue: HashSet::new(),
                first_enqueued: None,
                errored: false,
                last_error: None,
                last_reindex: None,
                last_synced_at: None,
                verified: false,
            };
            if !slot.index_path.exists() {
                slot.errored = true;
                slot.record_error(format!(
                    "slot has no index.db — run `cqs index --slot {name}` to initialize it"
                ));
                slots.push(slot);
                continue;
            }
            match classify_slot_model(&slot.index_path, active_model) {
                Ok(kind) => {
                    let foreign = matches!(kind, SiblingKind::Foreign { .. });
                    slot.kind = kind;
                    if foreign && !policy.foreign_enabled {
                        slot.inert = true;
                        tracing::info!(
                            slot = %slot.name,
                            "sibling slot uses a foreign model and CQS_WATCH_ALL_SLOTS is unset — inert (not propagated)"
                        );
                    }
                }
                Err(msg) => {
                    slot.errored = true;
                    slot.record_error(msg);
                }
            }
            slot.last_synced_at = index_mtime_unix_secs(&slot.index_path);
            slots.push(slot);
        }

        tracing::info!(
            siblings = slots.len(),
            propagated = slots.iter().filter(|s| !s.inert && !s.errored).count(),
            "sibling slot propagation initialized"
        );
        Self {
            slots,
            policy,
            cursor: 0,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Copy the active cycle's drained file set into every propagated
    /// sibling's queue. Called from `process_file_changes` on the
    /// success arm — propagation strictly follows the active reindex so
    /// the global cache already holds the fresh embeddings.
    ///
    /// Queues share the inotify path's `CQS_WATCH_MAX_PENDING` cap;
    /// entries dropped at the cap are recovered by the slot-aware
    /// reconcile pass (the walk is idempotent).
    pub(super) fn enqueue(&mut self, files: &[PathBuf]) {
        if files.is_empty() {
            return;
        }
        let cap = super::events::max_pending_files();
        for slot in &mut self.slots {
            if slot.inert {
                continue;
            }
            let mut dropped = 0usize;
            for f in files {
                if slot.queue.len() >= cap {
                    dropped += 1;
                    continue;
                }
                slot.queue.insert(f.clone());
            }
            if dropped > 0 {
                tracing::warn!(
                    slot = %slot.name,
                    dropped,
                    cap,
                    "sibling delta queue at cap — dropped entries recover via reconcile"
                );
            }
            if !slot.queue.is_empty() && slot.first_enqueued.is_none() {
                slot.first_enqueued = Some(std::time::Instant::now());
            }
        }
    }

    /// Whether slot `i` should drain this tick.
    fn slot_due(&self, i: usize, rebuild_in_flight: bool) -> bool {
        let slot = &self.slots[i];
        if slot.inert || slot.errored || slot.queue.is_empty() {
            return false;
        }
        match slot.kind {
            SiblingKind::SameModel => true,
            // Foreign drains load a second model — defer while a
            // background HNSW rebuild may be holding GPU memory.
            SiblingKind::Foreign { .. } => {
                !rebuild_in_flight
                    && foreign_drain_due(slot.queue.len(), slot.first_enqueued, &self.policy)
            }
        }
    }

    /// Drain at most ONE due sibling slot. Called from the watch loop's
    /// idle arm so active-slot work always preempts sibling drains; one
    /// slot per tick keeps each idle tick bounded and yields to fresh
    /// events between slots.
    pub(super) fn drain_one(
        &mut self,
        cfg: &WatchConfig,
        backoff: &mut EmbedderBackoff,
        shared_rt: &Arc<tokio::runtime::Runtime>,
        rebuild_in_flight: bool,
    ) -> Option<DrainOutcome> {
        let n = self.slots.len();
        if n == 0 {
            return None;
        }
        let mut pick = None;
        for off in 0..n {
            let i = (self.cursor + off) % n;
            if self.slot_due(i, rebuild_in_flight) {
                pick = Some(i);
                break;
            }
        }
        let i = pick?;
        self.cursor = (i + 1) % n;
        // Copy the policy out so the `&mut self.slots[i]` borrow below
        // doesn't conflict (`SiblingPolicy` is `Copy`).
        let policy = self.policy;
        drain_slot(cfg, &mut self.slots[i], &policy, backoff, shared_rt)
    }

    /// Slot-aware reconcile: run the same fingerprint reconciliation the
    /// active slot gets against every propagated sibling, queueing
    /// divergent files into the sibling's delta queue. This is the
    /// durability layer — the in-memory queues don't survive a daemon
    /// restart, but the next reconcile tick re-derives them from disk.
    ///
    /// Also the retry cadence for errored slots: the errored mark is
    /// cleared here when the slot's store opens cleanly, so the next
    /// idle tick's drain retries.
    pub(super) fn reconcile_siblings(
        &mut self,
        root: &Path,
        parser: &CqParser,
        no_ignore: bool,
        max_pending: usize,
        disk_files: Option<&HashSet<PathBuf>>,
        shared_rt: &Arc<tokio::runtime::Runtime>,
    ) -> usize {
        let _span = tracing::info_span!("reconcile_siblings", slots = self.slots.len()).entered();
        let mut total = 0usize;
        for slot in &mut self.slots {
            if slot.inert {
                continue;
            }
            let store = match Store::open_with_runtime(&slot.index_path, Arc::clone(shared_rt)) {
                Ok(s) => s,
                Err(e) => {
                    slot.errored = true;
                    slot.record_error(format!("reconcile: failed to open slot store: {e}"));
                    continue;
                }
            };
            // The store opened — clear the errored mark so drains retry.
            // `last_error` stays sticky for status visibility.
            slot.errored = false;
            let queued = super::reconcile::run_daemon_reconcile_with_walk(
                &store,
                root,
                parser,
                no_ignore,
                &mut slot.queue,
                max_pending,
                disk_files,
            );
            if queued > 0 && slot.first_enqueued.is_none() {
                slot.first_enqueued = Some(std::time::Instant::now());
            }
            slot.verified = true;
            if queued > 0 {
                tracing::info!(slot = %slot.name, queued, "sibling reconcile queued divergent files");
            }
            total += queued;
        }
        total
    }

    /// Build the per-slot status entries the snapshot publisher appends
    /// after the active slot. Cheap — small clones, no filesystem work.
    pub(super) fn status_entries(&self) -> Vec<SlotWatchStatus> {
        self.slots
            .iter()
            .map(|s| SlotWatchStatus {
                name: s.name.clone(),
                state: s.status_state(),
                last_synced_at: s.last_synced_at,
                last_reindex: s.last_reindex.clone(),
                queue_depth: u64::try_from(s.queue.len()).unwrap_or(u64::MAX),
                last_error: s.last_error.clone(),
            })
            .collect()
    }
}

/// Read the slot's recorded model from its store metadata and classify
/// it against the active slot's resolved config. `Err(message)` when the
/// model can't be determined or isn't a known preset (we couldn't build
/// an embedder for it, so propagation would be wrong-dim corruption).
fn classify_slot_model(
    index_path: &Path,
    active_model: &ModelConfig,
) -> Result<SiblingKind, String> {
    let store =
        Store::open_readonly(index_path).map_err(|e| format!("failed to open slot store: {e}"))?;
    let stored = store
        .try_stored_model_name()
        .map_err(|e| format!("failed to read slot model metadata: {e}"))?;
    drop(store);
    let stored = match stored {
        Some(s) => s,
        None => return Err("slot store has no recorded embedding model".to_string()),
    };
    if stored == active_model.repo || stored == active_model.name {
        return Ok(SiblingKind::SameModel);
    }
    match ModelConfig::from_preset(&stored) {
        Some(cfg) if cfg.name == active_model.name => Ok(SiblingKind::SameModel),
        Some(_) => Ok(SiblingKind::Foreign { model: stored }),
        None => Err(format!(
            "slot model {stored:?} is not a known preset — cannot build an embedder for propagation"
        )),
    }
}

/// Best-effort `index.db` mtime as unix seconds, for the status entry.
fn index_mtime_unix_secs(index_path: &Path) -> Option<i64> {
    std::fs::metadata(index_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Drain one sibling slot's queue through the SAME reindex entry point
/// the active slot uses (`reindex_files`, which resolves reuse through
/// the shared `resolve_reuse`). The store handle is the slot parameter —
/// it points at the sibling's `index.db`.
///
/// Returns `None` when the drain was deferred (lock held, embedder
/// unavailable) or failed; failures mark the slot errored and the files
/// are requeued either way.
fn drain_slot(
    cfg: &WatchConfig,
    slot: &mut SiblingSlot,
    policy: &SiblingPolicy,
    backoff: &mut EmbedderBackoff,
    shared_rt: &Arc<tokio::runtime::Runtime>,
) -> Option<DrainOutcome> {
    let files: Vec<PathBuf> = slot.queue.drain().collect();
    slot.first_enqueued = None;
    let _span = tracing::info_span!(
        "drain_sibling_slot",
        slot = %slot.name,
        files = files.len(),
    )
    .entered();

    // Per-slot index lock — same scope `cqs index --slot X` takes.
    let lock = match try_acquire_index_lock(&slot.slot_dir) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            tracing::debug!(slot = %slot.name, "sibling drain deferred: slot index lock held");
            slot.requeue(files);
            return None;
        }
        Err(e) => {
            slot.errored = true;
            slot.record_error(format!("failed to create slot index lock: {e}"));
            slot.requeue(files);
            return None;
        }
    };

    let store = match Store::open_with_runtime(&slot.index_path, Arc::clone(shared_rt)) {
        Ok(s) => s,
        Err(e) => {
            slot.errored = true;
            slot.record_error(format!("failed to open slot store: {e}"));
            slot.requeue(files);
            return None;
        }
    };

    // Re-verify the slot's model each drain — `cqs index --slot X
    // --model Y` can re-model a slot mid-daemon.
    match classify_slot_model(&slot.index_path, cfg.model_config) {
        Ok(kind) => {
            if matches!(kind, SiblingKind::Foreign { .. }) && !policy.foreign_enabled {
                slot.kind = kind;
                slot.inert = true;
                slot.queue.clear();
                tracing::info!(
                    slot = %slot.name,
                    "sibling slot re-modeled to a foreign model without CQS_WATCH_ALL_SLOTS — now inert"
                );
                return None;
            }
            slot.kind = kind;
        }
        Err(msg) => {
            slot.errored = true;
            slot.record_error(msg);
            slot.requeue(files);
            return None;
        }
    }

    // Resolve the embedder. Same-model drains share the active
    // embedder; with the active-first ordering its inference path is
    // expected to stay cold (100% cache hits). Foreign drains build a
    // session for the slot's model — exactly one load per drain,
    // dropped at the end of this function.
    let foreign_embedder: Option<Embedder> = match &slot.kind {
        SiblingKind::SameModel => None,
        SiblingKind::Foreign { model } => {
            let model_cfg = match ModelConfig::from_preset(model) {
                Some(c) => c,
                None => {
                    slot.errored = true;
                    slot.record_error(format!("slot model {model:?} is not a known preset"));
                    slot.requeue(files);
                    return None;
                }
            };
            match Embedder::new(model_cfg) {
                Ok(e) => Some(e),
                Err(e) => {
                    slot.errored = true;
                    slot.record_error(format!("failed to load embedder for sibling drain: {e}"));
                    slot.requeue(files);
                    return None;
                }
            }
        }
    };
    let embedder: &Embedder = match &foreign_embedder {
        Some(e) => e,
        None => match try_init_embedder(cfg.embedder, backoff, cfg.model_config) {
            Some(e) => e,
            None => {
                tracing::debug!(
                    slot = %slot.name,
                    "sibling drain deferred: active embedder unavailable (backoff)"
                );
                slot.requeue(files);
                return None;
            }
        },
    };

    // Mark the sibling's HNSW dirty BEFORE writing chunks — and leave it
    // set after: drains don't maintain sibling HNSW graphs, so the
    // on-disk graph genuinely is stale once new chunks land. Search
    // against the slot uses the existing dirty-flag fallback.
    if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Enriched, true) {
        slot.errored = true;
        slot.record_error(format!("cannot set sibling HNSW dirty flag: {e}"));
        slot.requeue(files);
        return None;
    }
    if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Base, true) {
        slot.errored = true;
        slot.record_error(format!("cannot set sibling base HNSW dirty flag: {e}"));
        slot.requeue(files);
        return None;
    }

    let started = std::time::Instant::now();
    let result = reindex_files(
        cfg.root,
        &store,
        &files,
        cfg.parser,
        embedder,
        cfg.global_cache,
        true,
    );
    drop(store);
    drop(lock);

    match result {
        Ok((chunks, embedded_hashes)) => {
            let outcome = DrainOutcome {
                slot: slot.name.clone(),
                files: files.len(),
                chunks,
                embedded: embedded_hashes.len(),
            };
            slot.last_reindex = Some(ReindexLatency {
                at_unix_secs: cqs::unix_secs_i64().unwrap_or(0),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                files: u64::try_from(files.len()).unwrap_or(u64::MAX),
            });
            slot.last_synced_at = index_mtime_unix_secs(&slot.index_path);
            slot.errored = false;
            slot.verified = true;
            tracing::info!(
                slot = %slot.name,
                files = outcome.files,
                chunks = outcome.chunks,
                embedded = outcome.embedded,
                same_model = matches!(slot.kind, SiblingKind::SameModel),
                "sibling slot drained"
            );
            Some(outcome)
        }
        Err(e) => {
            slot.errored = true;
            slot.record_error(format!("sibling reindex failed: {e}"));
            slot.requeue(files);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ModelInfo;

    fn policy(foreign_enabled: bool, batch_files: usize, batch_secs: u64) -> SiblingPolicy {
        SiblingPolicy {
            foreign_enabled,
            batch_files,
            batch_secs,
        }
    }

    fn test_slot(name: &str, dir: &Path, kind: SiblingKind) -> SiblingSlot {
        SiblingSlot {
            name: name.to_string(),
            slot_dir: dir.to_path_buf(),
            index_path: dir.join(cqs::INDEX_DB_FILENAME),
            kind,
            inert: false,
            queue: HashSet::new(),
            first_enqueued: None,
            errored: false,
            last_error: None,
            last_reindex: None,
            last_synced_at: None,
            verified: false,
        }
    }

    fn set_with(slots: Vec<SiblingSlot>, policy: SiblingPolicy) -> SiblingSet {
        SiblingSet {
            slots,
            policy,
            cursor: 0,
        }
    }

    /// Create `.cqs/slots/<name>/index.db` initialized to `model` so
    /// discovery/classification can read real metadata.
    fn init_slot_store(project_cqs_dir: &Path, name: &str, model: &str, dim: usize) {
        let dir = cqs::resolve_slot_dir(project_cqs_dir, name);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join(cqs::INDEX_DB_FILENAME)).unwrap();
        store.init(&ModelInfo::new(model, dim)).unwrap();
    }

    // ===== foreign hysteresis =====

    #[test]
    fn foreign_drain_due_fires_on_file_count() {
        let p = policy(true, 4, 3600);
        assert!(!foreign_drain_due(3, Some(std::time::Instant::now()), &p));
        assert!(foreign_drain_due(4, Some(std::time::Instant::now()), &p));
    }

    #[test]
    fn foreign_drain_due_fires_on_age() {
        let p = policy(true, 1000, 1);
        let old = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(foreign_drain_due(1, Some(old), &p));
        assert!(!foreign_drain_due(1, Some(std::time::Instant::now()), &p));
    }

    #[test]
    fn foreign_drain_due_empty_queue_never_fires() {
        let p = policy(true, 1, 0);
        assert!(!foreign_drain_due(0, None, &p));
    }

    // ===== enqueue + due decision =====

    #[test]
    fn enqueue_skips_inert_slots_and_arms_hysteresis_clock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut active = test_slot("same", tmp.path(), SiblingKind::SameModel);
        active.verified = true;
        let mut inert = test_slot(
            "foreign",
            tmp.path(),
            SiblingKind::Foreign {
                model: "other".to_string(),
            },
        );
        inert.inert = true;
        let mut set = set_with(vec![active, inert], policy(false, 32, 300));

        set.enqueue(&[PathBuf::from("a.rs"), PathBuf::from("b.rs")]);

        assert_eq!(set.slots[0].queue.len(), 2);
        assert!(set.slots[0].first_enqueued.is_some());
        assert!(set.slots[1].queue.is_empty(), "inert slot must stay empty");
        assert!(set.slots[1].first_enqueued.is_none());
    }

    #[test]
    fn same_model_slot_is_due_immediately_foreign_waits_for_hysteresis() {
        let tmp = tempfile::TempDir::new().unwrap();
        let same = test_slot("same", tmp.path(), SiblingKind::SameModel);
        let foreign = test_slot(
            "foreign",
            tmp.path(),
            SiblingKind::Foreign {
                model: "other".to_string(),
            },
        );
        let mut set = set_with(vec![same, foreign], policy(true, 32, 3600));
        set.enqueue(&[PathBuf::from("a.rs")]);

        assert!(set.slot_due(0, false), "same-model due as soon as queued");
        assert!(
            !set.slot_due(1, false),
            "foreign below both thresholds is not due"
        );

        // Push the foreign queue over the file threshold.
        let many: Vec<PathBuf> = (0..32).map(|i| PathBuf::from(format!("f{i}.rs"))).collect();
        set.enqueue(&many);
        assert!(set.slot_due(1, false), "foreign due at file threshold");
        assert!(
            !set.slot_due(1, true),
            "foreign drains defer while a rebuild holds the GPU"
        );
    }

    #[test]
    fn errored_slot_is_never_due() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut slot = test_slot("bad", tmp.path(), SiblingKind::SameModel);
        slot.errored = true;
        slot.queue.insert(PathBuf::from("a.rs"));
        let set = set_with(vec![slot], policy(true, 1, 0));
        assert!(!set.slot_due(0, false));
    }

    // ===== status entries =====

    #[test]
    fn status_entries_state_machine() {
        let tmp = tempfile::TempDir::new().unwrap();

        let mut fresh = test_slot("fresh", tmp.path(), SiblingKind::SameModel);
        fresh.verified = true;

        let mut queued = test_slot("queued", tmp.path(), SiblingKind::SameModel);
        queued.verified = true;
        queued.queue.insert(PathBuf::from("a.rs"));

        let mut errored = test_slot("errored", tmp.path(), SiblingKind::SameModel);
        errored.errored = true;
        errored.record_error("synthetic".to_string());

        let mut inert = test_slot(
            "inert",
            tmp.path(),
            SiblingKind::Foreign {
                model: "other".to_string(),
            },
        );
        inert.inert = true;

        let unverified = test_slot("new", tmp.path(), SiblingKind::SameModel);

        let set = set_with(
            vec![fresh, queued, errored, inert, unverified],
            policy(false, 32, 300),
        );
        let entries = set.status_entries();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].state, FreshnessState::Fresh);
        assert_eq!(entries[1].state, FreshnessState::Stale);
        assert_eq!(entries[1].queue_depth, 1);
        assert_eq!(entries[2].state, FreshnessState::Stale);
        assert!(entries[2].last_error.is_some());
        assert_eq!(entries[3].state, FreshnessState::Unknown);
        assert_eq!(
            entries[4].state,
            FreshnessState::Unknown,
            "empty queue before first verify is unverified, not fresh"
        );
    }

    // ===== discovery =====

    #[test]
    fn discover_marks_missing_index_slot_errored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_cqs_dir = tmp.path().join(".cqs");
        // Active slot + one sibling without an index.db.
        std::fs::create_dir_all(cqs::resolve_slot_dir(&project_cqs_dir, "default")).unwrap();
        std::fs::create_dir_all(cqs::resolve_slot_dir(&project_cqs_dir, "empty-sib")).unwrap();

        let active_model = ModelConfig::default_model();
        let set = SiblingSet::discover(
            &project_cqs_dir,
            "default",
            &active_model,
            policy(false, 32, 300),
        );
        assert_eq!(set.slots.len(), 1);
        let entry = &set.status_entries()[0];
        assert_eq!(entry.name, "empty-sib");
        assert_eq!(entry.state, FreshnessState::Stale);
        assert!(entry
            .last_error
            .as_ref()
            .is_some_and(|e| e.message.contains("no index.db")));
    }

    #[test]
    fn discover_classifies_same_and_foreign_models() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_cqs_dir = tmp.path().join(".cqs");
        let active_model = ModelConfig::default_model();
        init_slot_store(&project_cqs_dir, "default", &active_model.repo, 8);
        init_slot_store(&project_cqs_dir, "same-sib", &active_model.repo, 8);
        // BGE-large is a known preset distinct from the default model.
        init_slot_store(&project_cqs_dir, "foreign-sib", "BAAI/bge-large-en-v1.5", 8);

        let set = SiblingSet::discover(
            &project_cqs_dir,
            "default",
            &active_model,
            policy(false, 32, 300),
        );
        assert_eq!(set.slots.len(), 2);
        let same = set.slots.iter().find(|s| s.name == "same-sib").unwrap();
        assert_eq!(same.kind, SiblingKind::SameModel);
        assert!(!same.inert);
        let foreign = set.slots.iter().find(|s| s.name == "foreign-sib").unwrap();
        assert!(matches!(foreign.kind, SiblingKind::Foreign { .. }));
        assert!(
            foreign.inert,
            "foreign slot must be inert without CQS_WATCH_ALL_SLOTS"
        );
    }

    #[test]
    fn discover_with_foreign_opt_in_propagates_foreign_slot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_cqs_dir = tmp.path().join(".cqs");
        let active_model = ModelConfig::default_model();
        init_slot_store(&project_cqs_dir, "default", &active_model.repo, 8);
        init_slot_store(&project_cqs_dir, "foreign-sib", "BAAI/bge-large-en-v1.5", 8);

        let set = SiblingSet::discover(
            &project_cqs_dir,
            "default",
            &active_model,
            policy(true, 32, 300),
        );
        assert_eq!(set.slots.len(), 1);
        assert!(!set.slots[0].inert, "opted-in foreign slot is propagated");
    }

    // ===== reconcile (durability net) =====

    /// Daemon-killed-mid-drain recovery: a sibling whose store has never
    /// seen a file that exists on disk gets that file queued by the
    /// slot-aware reconcile pass — without any inotify event.
    #[test]
    fn reconcile_siblings_queues_divergent_files_and_clears_errored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let project_cqs_dir = root.join(".cqs");
        let active_model = ModelConfig::default_model();
        init_slot_store(&project_cqs_dir, "sib", &active_model.repo, 8);

        // A real source file on disk that the sibling store has no
        // chunks for → reconcile classifies it as Added and queues it.
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("orphan.rs"), b"fn orphan() {}").unwrap();

        let sib_dir = cqs::resolve_slot_dir(&project_cqs_dir, "sib");
        let mut slot = test_slot("sib", &sib_dir, SiblingKind::SameModel);
        // Simulate a prior drain failure — reconcile is the retry path.
        slot.errored = true;
        let mut set = set_with(vec![slot], policy(false, 32, 300));

        let parser = CqParser::new().unwrap();
        let rt = build_shared_runtime().unwrap();
        let queued = set.reconcile_siblings(root, &parser, false, 10_000, None, &rt);

        assert_eq!(queued, 1, "divergent file must be queued");
        assert!(set.slots[0].queue.contains(&PathBuf::from("src/orphan.rs")));
        assert!(!set.slots[0].errored, "reconcile clears the errored mark");
        assert!(set.slots[0].verified);
        assert!(set.slots[0].first_enqueued.is_some());
    }

    #[test]
    fn reconcile_siblings_skips_inert_slots() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.rs"), b"fn a() {}").unwrap();

        let mut slot = test_slot(
            "inert",
            &root.join(".cqs/slots/inert"),
            SiblingKind::Foreign {
                model: "other".to_string(),
            },
        );
        slot.inert = true;
        let mut set = set_with(vec![slot], policy(false, 32, 300));

        let parser = CqParser::new().unwrap();
        let rt = build_shared_runtime().unwrap();
        let queued = set.reconcile_siblings(root, &parser, false, 10_000, None, &rt);
        assert_eq!(queued, 0);
        assert!(set.slots[0].queue.is_empty());
        assert!(!set.slots[0].verified, "inert slots are not reconciled");
    }

    // ===== drain scheduling =====

    #[test]
    fn drain_one_returns_none_when_nothing_due() {
        let tmp = tempfile::TempDir::new().unwrap();
        let slot = test_slot("idle", tmp.path(), SiblingKind::SameModel);
        let mut set = set_with(vec![slot], policy(true, 32, 300));

        let cqs_dir = tmp.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let notes_path = tmp.path().join("docs/notes.toml");
        let supported: HashSet<&str> = HashSet::new();
        let embedder_slot = std::sync::OnceLock::new();
        let model_cfg = ModelConfig::default_model();
        let parser = CqParser::new().unwrap();
        let gitignore = std::sync::RwLock::new(None);
        let cfg = WatchConfig {
            root: tmp.path(),
            cqs_dir: &cqs_dir,
            notes_path: &notes_path,
            supported_ext: &supported,
            parser: &parser,
            embedder: &embedder_slot,
            quiet: true,
            model_config: &model_cfg,
            gitignore: &gitignore,
            splade_encoder: None,
            global_cache: None,
        };
        let mut backoff = EmbedderBackoff::new();
        let rt = build_shared_runtime().unwrap();
        assert!(set.drain_one(&cfg, &mut backoff, &rt, false).is_none());
    }

    /// Same-model sibling convergence with ZERO embedder inference: the
    /// active slot reindexes a file (writing its embeddings back to the
    /// global cache), then the sibling drain resolves the entire delta
    /// from cache through `resolve_reuse`. `DrainOutcome::embedded` is
    /// the accounting — it counts chunks that fell through to
    /// `embed_documents`, and must be 0.
    ///
    /// `#[ignore]` because it loads a real CPU embedder (ONNX weights),
    /// matching the precedent of the reindex_files cache tests in
    /// `watch::tests`.
    #[test]
    #[ignore = "Requires loading a real CPU embedder (heavy)"]
    fn same_model_sibling_drain_converges_with_zero_embedder_calls() {
        use cqs::cache::EmbeddingCache;

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let project_cqs_dir = root.join(".cqs");

        let rs_file = root.join("hit.rs");
        std::fs::write(&rs_file, b"pub fn sibling_cache_hit() { let _ = 42; }").unwrap();

        // Active embedder + active store.
        let model_cfg = ModelConfig::resolve(None, None);
        let embedder = Embedder::new_cpu(model_cfg).expect("init CPU embedder");
        let dim = embedder.embedding_dim();
        let repo = embedder.model_config().repo.clone();

        let active_dir = cqs::resolve_slot_dir(&project_cqs_dir, "default");
        std::fs::create_dir_all(&active_dir).unwrap();
        let mut active_store = Store::open(&active_dir.join(cqs::INDEX_DB_FILENAME)).unwrap();
        active_store.init(&ModelInfo::new(&repo, dim)).unwrap();
        active_store.set_dim(dim);

        // Sibling store on the same model.
        init_slot_store(&project_cqs_dir, "sib", &repo, dim);
        let sib_dir = cqs::resolve_slot_dir(&project_cqs_dir, "sib");

        // Global cache, shared across slots.
        let cache_path = EmbeddingCache::project_default_path(&project_cqs_dir);
        let cache = EmbeddingCache::open(&cache_path).expect("open cache");

        let parser = CqParser::new().unwrap();
        let files = vec![PathBuf::from("hit.rs")];

        // 1) ACTIVE reindex — embeds fresh and writes back to the cache.
        let (active_chunks, active_embedded) = reindex_files(
            root,
            &active_store,
            &files,
            &parser,
            &embedder,
            Some(&cache),
            true,
        )
        .expect("active reindex");
        assert!(active_chunks >= 1);
        assert!(
            !active_embedded.is_empty(),
            "active pass must have embedded fresh chunks"
        );

        // 2) SIBLING drain — must be a 100% cache hit.
        let embedder_slot = std::sync::OnceLock::new();
        let _ = embedder_slot.set(std::sync::Arc::new(embedder));
        let model_cfg2 = ModelConfig::resolve(None, None);
        let gitignore = std::sync::RwLock::new(None);
        let supported: HashSet<&str> = HashSet::new();
        let notes_path = root.join("docs/notes.toml");
        let cfg = WatchConfig {
            root,
            cqs_dir: &active_dir,
            notes_path: &notes_path,
            supported_ext: &supported,
            parser: &parser,
            embedder: &embedder_slot,
            quiet: true,
            model_config: &model_cfg2,
            gitignore: &gitignore,
            splade_encoder: None,
            global_cache: Some(&cache),
        };

        let mut slot = test_slot("sib", &sib_dir, SiblingKind::SameModel);
        slot.queue.insert(PathBuf::from("hit.rs"));
        slot.first_enqueued = Some(std::time::Instant::now());
        let mut set = set_with(vec![slot], policy(false, 32, 300));

        let mut backoff = EmbedderBackoff::new();
        let rt = build_shared_runtime().unwrap();
        let outcome = set
            .drain_one(&cfg, &mut backoff, &rt, false)
            .expect("sibling drain must run");

        assert_eq!(outcome.slot, "sib");
        assert!(outcome.chunks >= 1, "sibling received the chunks");
        assert_eq!(
            outcome.embedded, 0,
            "same-model sibling drain must be served entirely from the global cache"
        );

        // The sibling store actually holds the file's chunks now.
        let sib_store = Store::open(&sib_dir.join(cqs::INDEX_DB_FILENAME)).unwrap();
        let chunks = sib_store.get_chunks_by_origin("hit.rs").unwrap();
        assert!(
            !chunks.is_empty(),
            "sibling store must contain the propagated chunks"
        );
        assert_eq!(set.slots[0].status_state(), FreshnessState::Fresh);
    }
}
