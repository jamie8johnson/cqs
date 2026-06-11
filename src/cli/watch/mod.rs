//! Watch mode - monitor for file changes and reindex
//!
//! ## Memory Usage
//!
//! Watch mode holds several resources in memory while idle:
//!
//! - **Parser**: ~1MB for tree-sitter queries (allocated immediately)
//! - **Store**: SQLite connection pool with up to 4 connections + 256 MiB mmap
//!   + 16 MiB page cache (allocated immediately)
//! - **Embedder**: ~500MB for ONNX model (lazy-loaded on first file change)
//!
//! ### Background-rebuild peak
//!
//! When `spawn_hnsw_rebuild` fires, the rebuild thread opens a second
//! read-only `Store` handle to stream chunks for the new HNSW index. That
//! handle uses [`Store::open_readonly`] (64 MiB mmap + 4 MiB cache) to keep
//! the rebuild-window peak low. The build_hnsw_index_owned pipeline streams
//! chunks via the batched embedding API, so the smaller mmap doesn't hurt
//! rebuild throughput. Rebuild-window peak:
//!
//! ```text
//! main store     :  ~272 MiB (256 mmap + 16 cache)
//! rebuild store  :   ~68 MiB (64 mmap + 4 cache)
//! enriched HNSW  :  ~50–200 MiB (live, in main process)
//! rebuild HNSW   :  ~50–200 MiB (under construction, in rebuild thread)
//! ```
//!
//! `incremental_count`-driven backpressure (`watch::events`) is the only
//! guard against multiple concurrent rebuilds; in practice that holds, but
//! a future stress on the trigger thresholds would benefit from a
//! `Mutex<()>`-style "only one rebuild at a time" gate.
//!
//! The Embedder is the largest resource and is only loaded when files actually change.
//! Once loaded, it remains in memory for fast subsequent reindexing. This tradeoff
//! favors responsiveness over memory efficiency for long-running watch sessions.
//!
//! For memory-constrained environments, consider running `cqs index` manually instead
//! of using watch mode.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use notify::{Config, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{info, info_span, warn};

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::generate_nl_description_with_seq_len;
use cqs::hnsw::HnswIndex;
use cqs::note::parse_notes;
use cqs::parser::{ChunkTypeRefs, Parser as CqParser};
use cqs::store::Store;

use super::{check_interrupted, find_project_root, try_acquire_index_lock, Cli};

#[cfg(unix)]
mod socket;
#[cfg(unix)]
use socket::{
    handle_socket_client, max_concurrent_daemon_clients, write_daemon_error, SocketCleanupGuard,
};

mod runtime;
use runtime::build_shared_runtime;
#[cfg(unix)]
use runtime::{daemon_should_exit, install_sigterm_handler, is_shutdown_requested};

mod rebuild;
use rebuild::{
    clear_hnsw_dirty_with_retry, drain_pending_rebuild, hnsw_rebuild_threshold,
    resolve_index_aware_model_for_watch, spawn_hnsw_rebuild, try_init_embedder, EmbedderBackoff,
    PendingRebuild,
};
#[cfg(test)]
use rebuild::{RebuildOutcome, RebuildResult};

mod gc;
use gc::{
    prune_last_indexed_mtime, prune_last_indexed_mtime_by_age, run_daemon_periodic_gc,
    run_daemon_startup_gc,
};
#[cfg(test)]
use gc::{LAST_INDEXED_PRUNE_AGE_SECS, LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT};

mod reconcile;
use reconcile::{reconcile_enabled, run_daemon_reconcile};

mod events;
use events::max_pending_files;
use events::{collect_events, process_file_changes, process_note_changes};

mod siblings;
use siblings::{SiblingPolicy, SiblingSet};

mod reindex;
#[cfg(target_os = "linux")]
use reindex::count_watchable_dirs;
#[cfg(test)]
use reindex::splade_batch_size;
use reindex::{
    build_splade_encoder_for_watch, db_file_identity, encode_splade_for_changed_files,
    reindex_files, reindex_notes,
};

#[cfg(unix)]
mod daemon;

/// Immutable references shared across the watch loop.
///
/// Does not include `Store` because it is re-opened each cycle.
///
/// `embedder` points at a shared `Arc<OnceLock<Arc<Embedder>>>` that the
/// daemon thread also holds. First side to populate it wins; the other
/// side's lazy-init short-circuits to the same instance. This keeps the
/// outer watch loop and the daemon thread from each owning a ~500 MB
/// duplicate embedder.
struct WatchConfig<'a> {
    root: &'a Path,
    cqs_dir: &'a Path,
    notes_path: &'a Path,
    supported_ext: &'a HashSet<&'a str>,
    parser: &'a CqParser,
    embedder: &'a std::sync::OnceLock<std::sync::Arc<Embedder>>,
    quiet: bool,
    model_config: &'a ModelConfig,
    /// gitignore matcher for the project. `None` if
    /// `CQS_WATCH_RESPECT_GITIGNORE=0`, `--no-ignore` was passed, or the
    /// `.gitignore` file is missing/unreadable. Wrapped in `RwLock` so the
    /// watch loop can hot-swap it on `.gitignore` change without a restart.
    gitignore: &'a std::sync::RwLock<Option<ignore::gitignore::Gitignore>>,
    /// SPLADE encoder held resident in the daemon so incremental
    /// reindex cycles can encode sparse vectors for new/changed chunks.
    /// `None` when the SPLADE model is absent, fails to load, or
    /// `CQS_WATCH_INCREMENTAL_SPLADE=0`. `Mutex` serializes GPU access
    /// since the encoder holds a CUDA context.
    splade_encoder: Option<&'a std::sync::Mutex<cqs::splade::SpladeEncoder>>,
    /// Project-scoped global embedding cache (per-project, shared
    /// across slots). `Some` when the cache opened cleanly at daemon
    /// startup; `None` when `CQS_CACHE_ENABLED=0` is set or the open
    /// failed. `reindex_files` consults this cache before the store's
    /// per-slot `chunks.embedding` lookup so a chunk hashed in one slot
    /// (or under a previous model) doesn't pay GPU cost on every save.
    /// Mirrors the bulk pipeline's `prepare_for_embedding` shape.
    global_cache: Option<&'a cqs::cache::EmbeddingCache>,
}

/// Mutable session state that evolves across watch cycles.
struct WatchState {
    embedder_backoff: EmbedderBackoff,
    pending_files: HashSet<PathBuf>,
    pending_notes: bool,
    last_event: std::time::Instant,
    /// When the oldest still-pending event arrived. Armed by
    /// `collect_events` (and the reconcile queue paths) on the first
    /// event after a flush; cleared by the flush block once the pending
    /// sets drain. Drives the max-latency cap in [`flush_due`]: a
    /// never-quiet event stream still flushes within
    /// `DebounceConfig::max_latency` of this instant.
    first_pending_event: Option<std::time::Instant>,
    last_indexed_mtime: HashMap<PathBuf, SystemTime>,
    hnsw_index: Option<HnswIndex>,
    incremental_count: usize,
    /// Number of file events dropped this debounce cycle because
    /// pending_files was at cap. Logged once per cycle in
    /// process_file_changes, cleared after.
    dropped_this_cycle: usize,
    /// When a background HNSW rebuild is running, the watch loop
    /// queues new (chunk_id, embedding) pairs here so they can be replayed
    /// into the rebuilt Owned index before the swap. `None` while no
    /// rebuild is in flight.
    pending_rebuild: Option<PendingRebuild>,
    /// Throttle the per-tick `fs::metadata(index_path)` call in
    /// [`publish_watch_snapshot`]. Snapshots fire every ~100 ms, and
    /// `last_synced_at` is whole-second resolution anyway — restating
    /// every tick burns a `stat()` syscall (and on WSL 9P, ~ms latency).
    /// Cache the most-recent reading and only re-stat after this throttle
    /// window elapses.
    last_metadata_check: std::time::Instant,
    /// Cached `last_synced_at` value reused between throttled stat calls.
    /// `None` ⇒ index.db missing or mtime unreadable on the last stat.
    cached_last_synced_at: Option<i64>,
    /// Slot name resolved at startup. Cloned into the watch snapshot every
    /// publish so `daemon_status` callers (specifically `cqs slot remove`)
    /// can know which slot the daemon is serving and refuse to unlink it.
    /// Owned String rather than borrow because `WatchState` outlives the
    /// per-tick `WatchSnapshotInput`.
    active_slot: String,
    /// Latency of the most recent completed reindex pass.
    /// Recorded in `process_file_changes` where `reindex_files` returns
    /// Ok; published every snapshot tick for `cqs status --watch`.
    last_reindex: Option<cqs::watch_status::ReindexLatency>,
    /// Most recent reindex/notes-reindex error. Sticky across
    /// subsequent successes — the timestamp disambiguates. Recorded
    /// where the watch loop currently logs `Reindex error` /
    /// `Notes reindex error`.
    last_error: Option<cqs::watch_status::WatchErrorInfo>,
    /// Store-state stamp captured after the watch loop's own most recent
    /// write cycle (chunk reindex + incremental SPLADE encode). Every HNSW
    /// save this process performs passes it as the snapshot to
    /// `save_stamped`: if the live stamp differs at save time, a writer this
    /// loop never observed (a concurrent `cqs index`) committed chunks the
    /// in-memory index does not contain, and the save is discarded instead
    /// of overwriting the newer on-disk index. `None` until the first write
    /// cycle (saves then fall back to the live stamp).
    observed_stamp: Option<cqs::hnsw::StoreStamp>,
}

/// How often the watch loop re-stats `index.db` for the `last_synced_at`
/// field on `WatchSnapshot`. 10 s matches the whole-second wire resolution;
/// tighter than this would re-stat without giving observers any new bits.
const LAST_SYNCED_REFRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// Publish a fresh `WatchSnapshot` into the shared `Arc<RwLock<...>>`
/// the daemon thread reads through. Called once per outer watch-loop tick.
///
/// Pure assemble-and-write: pulls the relevant counters off `state`, reads
/// `index.db`'s mtime as a best-effort `last_synced_at`, computes the
/// state-machine value, and replaces the snapshot under a brief write lock.
/// The lock is held only for the move; readers never block on real work.
///
/// Takes `&mut WatchState` so the `last_synced_at` stat call can be
/// throttled via the cache. Without throttling this fires every ~100 ms
/// tick, paying a syscall (ms-scale on WSL 9P) for whole-second wire data.
///
/// `in_flight_clients` is the daemon accept loop's shared counter
/// (always 0 without `--serve`); `reconcile_signal` is sampled with a
/// plain `load` (never `swap` — draining it is the loop body's job).
/// Both feed the `cqs status --watch` ops block.
fn publish_watch_snapshot(
    handle: &cqs::watch_status::SharedWatchSnapshot,
    fresh_notifier: &cqs::watch_status::SharedFreshNotifier,
    state: &mut WatchState,
    index_path: &std::path::Path,
    in_flight_clients: &std::sync::atomic::AtomicUsize,
    reconcile_signal: &std::sync::atomic::AtomicBool,
    siblings: &SiblingSet,
) {
    // Only re-stat when the cache has expired. Snapshots fire every ~100 ms
    // but `last_synced_at` is whole-second resolution — re-stating every
    // tick burns a syscall for no observer-visible change. The cache is
    // invalidated after `LAST_SYNCED_REFRESH`. Overflow surfaces as None
    // (treated same as "missing mtime") instead of wrapping past `i64::MAX`.
    let last_synced_at = if state.last_metadata_check.elapsed() >= LAST_SYNCED_REFRESH {
        let fresh = std::fs::metadata(index_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok());
        state.cached_last_synced_at = fresh;
        state.last_metadata_check = std::time::Instant::now();
        fresh
    } else {
        state.cached_last_synced_at
    };
    let delta_saturated = state
        .pending_rebuild
        .as_ref()
        .map(|p| p.delta_saturated)
        .unwrap_or(false);
    let snap = cqs::watch_status::WatchSnapshot::compute(
        cqs::watch_status::WatchSnapshotInput::new(
            state.pending_files.len(),
            state.pending_notes,
            state.pending_rebuild.is_some(),
            delta_saturated,
            state.incremental_count,
            state.dropped_this_cycle,
            state.last_event,
            last_synced_at,
        )
        .with_active_slot(&state.active_slot)
        .with_ops(
            in_flight_clients.load(std::sync::atomic::Ordering::Acquire),
            reconcile_signal.load(std::sync::atomic::Ordering::Acquire),
            state.last_reindex.as_ref(),
            state.last_error.as_ref(),
        )
        .with_sibling_slots(siblings.status_entries()),
    );
    // Poison-recovery: another writer panicking shouldn't silently stop
    // freshness publishing. Recover and overwrite.
    //
    // Capture the previous state under the lock so we can emit a transition
    // log line *after* releasing it. Operators see a journal trail for
    // Fresh↔Stale↔Rebuilding flips. `WatchSnapshot` derives `Clone` and
    // `FreshnessState` is `Copy`, so the clone-for-log cost is negligible.
    let prev_state;
    let next_snap = snap.clone();
    match handle.write() {
        Ok(mut guard) => {
            prev_state = guard.state;
            *guard = snap;
        }
        Err(poisoned) => {
            tracing::warn!("watch_snapshot RwLock poisoned — recovering and continuing to publish");
            let mut guard = poisoned.into_inner();
            prev_state = guard.state;
            *guard = snap;
        }
    }

    if prev_state != next_snap.state {
        tracing::info!(
            prev = prev_state.as_str(),
            next = next_snap.state.as_str(),
            modified_files = next_snap.modified_files,
            rebuild_in_flight = next_snap.rebuild_in_flight,
            dropped_this_cycle = next_snap.dropped_this_cycle,
            "watch state transition",
        );
    }

    // Event-driven freshness wake-up. The notifier dedupes on idempotent
    // `false → false` and `true → true` calls (cheap mutex acquire +
    // boolean compare), so calling it every 100 ms tick is fine — only
    // `false → true` transitions issue a `notify_all` to parked
    // `wait_fresh` clients.
    fresh_notifier.set_fresh(next_snap.is_fresh());
}

/// Timing knobs for the watch loop's idle-flush debounce.
///
/// The pending event set flushes when EITHER condition holds:
///
/// - **quiet gap**: no accepted event for `quiet_gap` — the timer
///   restarts on every event, so a `git checkout` burst coalesces into
///   exactly one reindex cycle fired ~`quiet_gap` after the burst's
///   *last* event, and a single save flushes at the same latency as a
///   fixed window would.
/// - **max latency**: the oldest pending event has waited
///   `max_latency` — bounds total delay when the stream never goes
///   quiet (e.g. a generator continuously rewriting files), which
///   would otherwise keep restarting the quiet-gap timer forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DebounceConfig {
    quiet_gap: Duration,
    max_latency: Duration,
}

/// Multiplier applied to the quiet gap to derive the default
/// max-latency cap when `CQS_WATCH_MAX_DEBOUNCE_MS` is unset.
/// 6× = 3 s at the 500 ms inotify default, 9 s at the 1500 ms
/// WSL/poll auto-bump — long enough that a normal burst (git
/// checkout, branch switch) ends well inside it, short enough that a
/// never-quiet stream still reindexes within seconds.
const MAX_DEBOUNCE_FACTOR: u32 = 6;

/// Resolve the idle-flush debounce config from the `--debounce` flag,
/// poll-mode detection, and the two env overrides (passed in as already
/// parsed values so this stays pure and unit-testable).
///
/// Quiet gap precedence: `CQS_WATCH_DEBOUNCE_MS` env > `--debounce`
/// flag, with the WSL/poll auto-bump (500 → 1500 ms) applied only when
/// the user overrode neither. The poll watcher delivers events in scan
/// batches, so the quiet gap there must exceed NTFS's 1 s mtime
/// resolution or a single save risks double-firing.
///
/// Max latency: `CQS_WATCH_MAX_DEBOUNCE_MS` env, defaulting to
/// [`MAX_DEBOUNCE_FACTOR`] × the resolved quiet gap, and clamped to at
/// least the quiet gap (a cap below the gap would turn the idle-flush
/// into a fixed window at the cap, which is never what the two knobs
/// together are asking for).
fn resolve_debounce(
    flag_ms: u64,
    use_poll: bool,
    env_gap_ms: Option<u64>,
    env_max_ms: Option<u64>,
) -> DebounceConfig {
    let quiet_gap_ms = if let Some(env_ms) = env_gap_ms {
        env_ms
    } else if flag_ms == 500 && use_poll {
        tracing::info!(
            "Auto-bumping watch quiet-gap to 1500ms for WSL/poll mode (override via --debounce or CQS_WATCH_DEBOUNCE_MS)"
        );
        1500
    } else {
        flag_ms
    };
    let max_latency_ms = env_max_ms
        .unwrap_or_else(|| quiet_gap_ms.saturating_mul(u64::from(MAX_DEBOUNCE_FACTOR)))
        .max(quiet_gap_ms);
    DebounceConfig {
        quiet_gap: Duration::from_millis(quiet_gap_ms),
        max_latency: Duration::from_millis(max_latency_ms),
    }
}

/// Pre-drain decision: should the watch loop flush the pending sets
/// into a reindex cycle right now?
///
/// Evaluated on every loop iteration — event arrivals included, not
/// just recv timeouts — because a continuous stream of events arriving
/// faster than the recv timeout never reaches the timeout arm at all;
/// only the max-latency condition can fire there.
fn flush_due(state: &WatchState, debounce: &DebounceConfig) -> bool {
    if state.pending_files.is_empty() && !state.pending_notes {
        return false;
    }
    if state.last_event.elapsed() >= debounce.quiet_gap {
        return true;
    }
    state
        .first_pending_event
        .is_some_and(|first| first.elapsed() >= debounce.max_latency)
}

/// Check if a path is under a WSL DrvFS automount root.
///
/// Default automount root is `/mnt/`, but users can customize it via `automount.root`
/// in `/etc/wsl.conf`.
///
/// Delegates to `cqs::config::wsl_automount_root_or_default` so this helper
/// and `is_wsl_drvfs_path` share a single OnceLock-cached source of truth.
fn is_under_wsl_automount(path: &str) -> bool {
    path.starts_with(cqs::config::wsl_automount_root_or_default())
}

/// Build a `Gitignore` matcher rooted at the project, combining the
/// root `.gitignore` with any nested `.gitignore` files discovered by a
/// single shallow walk. Returns `None` under any of:
///
/// - `--no-ignore` is set (caller responsibility to pass `false`)
/// - `CQS_WATCH_RESPECT_GITIGNORE=0` (feature flag kill-switch)
/// - No `.gitignore` at project root (treated as "index everything")
/// - `.gitignore` is unreadable or malformed (logged as warn, fall through)
///
/// When `Some`, the matcher is queried per-event in `collect_events`. The
/// hardcoded `.cqs/` skip in `collect_events` remains in place as
/// belt-and-suspenders so the system's own files are never indexed
/// regardless of what `.gitignore` contains.
fn build_gitignore_matcher(root: &Path) -> Option<ignore::gitignore::Gitignore> {
    let _span = tracing::info_span!("build_gitignore_matcher").entered();

    if std::env::var("CQS_WATCH_RESPECT_GITIGNORE").as_deref() == Ok("0") {
        tracing::info!("CQS_WATCH_RESPECT_GITIGNORE=0 — gitignore filtering disabled");
        return None;
    }

    let root_gitignore = root.join(".gitignore");
    let root_cqsignore = root.join(".cqsignore");
    if !root_gitignore.exists() && !root_cqsignore.exists() {
        tracing::info!(
            root = %root.display(),
            "no .gitignore or .cqsignore at project root — watch will not filter"
        );
        return None;
    }

    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);

    // Order matters for negation: later `add()` calls win on conflict.
    // .gitignore first, then .cqsignore so cqs-specific overrides apply last.
    if root_gitignore.exists() {
        if let Some(err) = builder.add(&root_gitignore) {
            tracing::warn!(
                path = %root_gitignore.display(),
                error = %err,
                "root .gitignore unreadable or malformed — falling back to empty matcher"
            );
            return None;
        }
    }
    if root_cqsignore.exists() {
        if let Some(err) = builder.add(&root_cqsignore) {
            tracing::warn!(
                path = %root_cqsignore.display(),
                error = %err,
                "root .cqsignore unreadable or malformed — skipping it"
            );
        }
    }

    // Root-only .gitignore / .cqsignore. Nested ignore files are not
    // discovered: `cqs index` uses the full `ignore` crate walk which
    // supports nesting; the watch loop uses a per-event point query against
    // a pre-built matcher, and nesting would require rebuilding on every
    // subdir change. Root-level covers the worktree-pollution + vendor-bundle
    // cases.

    match builder.build() {
        Ok(gi) => {
            tracing::info!(
                n_files = gi.num_ignores(),
                "gitignore matcher loaded for watch loop"
            );
            Some(gi)
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "gitignore matcher build failed — watch will not filter by gitignore"
            );
            None
        }
    }
}

/// Watches the project for file changes and updates the code search index incrementally.
///
/// # Arguments
///
/// * `cli` - Command-line interface context
/// * `debounce_ms` - Quiet gap in milliseconds for the idle-flush debounce:
///   pending changes flush after this much event silence (see
///   [`DebounceConfig`] for the full semantics including the max-latency cap)
/// * `no_ignore` - If true, skips `.gitignore` filtering in the watch loop.
///   Mirrors the `cqs index --no-ignore` flag. When false (default), the watch
///   loop queries the project's `.gitignore` for every event and ignores matches.
///   Also overridable at runtime via `CQS_WATCH_RESPECT_GITIGNORE=0`.
/// * `poll` - If true, uses polling instead of inotify for file system monitoring
///
/// # Returns
///
/// Returns `Ok(())` on successful completion, or an error if the index doesn't exist or watch setup fails.
///
/// # Errors
///
/// * If the project index is not found (user should run `cqs index` first)
/// * If setting up file system watching fails
pub fn cmd_watch(
    cli: &Cli,
    debounce_ms: u64,
    no_ignore: bool,
    poll: bool,
    serve: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_watch", debounce_ms, poll, serve, no_ignore).entered();

    // Install SIGTERM handler *before* spawning the socket thread so both
    // the main loop and the accept loop observe the shutdown flag
    // immediately when systemd stops the unit.
    #[cfg(unix)]
    install_sigterm_handler();

    let root = find_project_root();

    // Auto-detect when polling is needed: WSL + DrvFS mount path.
    //
    // Detection is prefix-based rather than filesystem-based (statfs NTFS/FAT magic)
    // because that's pragmatic: paths under DrvFS mounts in WSL are Windows filesystems
    // (NTFS, FAT32, exFAT), none of which support inotify. A statfs check would give
    // the same answer with more syscalls and less portability across WSL versions.
    // If the project root is on a Linux filesystem inside WSL (e.g. /home/...), inotify works
    // fine and we leave use_poll false.
    // Also detect //wsl.localhost/ and //wsl$/ UNC paths.
    // Check /etc/wsl.conf for custom automount.root (default is /mnt/).
    let use_poll = poll
        || (cqs::config::is_wsl()
            && root
                .to_str()
                .is_some_and(|p| p.starts_with("//wsl") || is_under_wsl_automount(p)));

    if cqs::config::is_wsl() && !use_poll {
        tracing::warn!("WSL detected: inotify may be unreliable on Windows filesystem mounts. Use --poll or 'cqs index' periodically.");
    }

    // Idle-flush debounce resolution. `CQS_WATCH_DEBOUNCE_MS` is the
    // quiet gap (takes precedence over --debounce; WSL/poll auto-bump
    // 500 → 1500 ms because NTFS mtime resolution is 1 s and the poll
    // watcher delivers scan batches); `CQS_WATCH_MAX_DEBOUNCE_MS` is
    // the max-latency cap, defaulting to 6× the quiet gap. See
    // `DebounceConfig` for the flush semantics.
    let debounce = resolve_debounce(
        debounce_ms,
        use_poll,
        std::env::var("CQS_WATCH_DEBOUNCE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok()),
        std::env::var("CQS_WATCH_MAX_DEBOUNCE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok()),
    );
    tracing::info!(
        quiet_gap_ms = debounce.quiet_gap.as_millis() as u64,
        max_latency_ms = debounce.max_latency.as_millis() as u64,
        "watch debounce resolved (idle-flush)"
    );

    let project_cqs_dir = cqs::resolve_index_dir(&root);

    // Migration: ensure legacy `.cqs/index.db` (if present) is moved to
    // `.cqs/slots/default/` before watch hooks the index file. This is
    // idempotent — the migration runs at top of `dispatch::run_with`
    // already, so this is a belt-and-braces guard for daemon-only paths
    // (cqs-watch systemd service launched directly via `cqs watch --serve`
    // before any other CLI invocation triggered the migration).
    if project_cqs_dir.exists() {
        if let Err(e) = cqs::slot::migrate_legacy_index_to_default_slot(&project_cqs_dir) {
            tracing::warn!(error = %e, "slot migration failed inside watch boot; continuing without it");
        }
    }

    // Resolve active slot at daemon startup. The daemon binds to whichever
    // slot is active at this moment; promotion afterwards requires a daemon
    // restart per spec §Daemon.
    let active_slot = cqs::slot::resolve_slot_name(cli.slot.as_deref(), &project_cqs_dir)
        .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!(
        slot = %active_slot.name,
        source = active_slot.source.as_str(),
        "daemon bound to slot"
    );

    let cqs_dir = if cqs::slot::slots_root(&project_cqs_dir).exists() {
        cqs::resolve_slot_dir(&project_cqs_dir, &active_slot.name)
    } else {
        project_cqs_dir.clone()
    };
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    if !index_path.exists() {
        bail!("No index found at {}. Run 'cqs index' first (or 'cqs index --slot {}' if the slot exists but is empty).", index_path.display(), active_slot.name);
    }

    // Socket listener BEFORE watcher scan — daemon is immediately queryable
    // while the (potentially slow) poll watcher initializes.
    // Unix domain sockets are not available on Windows.
    #[cfg(unix)]
    let mut socket_listener = if serve {
        // Daemon socket is keyed by the project-level `.cqs/` dir so all
        // slots share one socket — the daemon serves whichever slot was
        // active at startup, but the socket is per-project not per-slot.
        let sock_path = super::daemon_socket_path(&project_cqs_dir);
        if sock_path.exists() {
            // Connect to the existing socket with a bounded timeout. A wedged
            // peer (mid-shutdown / paused) that never accepts would otherwise
            // hang `cqs watch --serve` startup indefinitely.
            let probe_timeout = std::time::Duration::from_millis(2_000);
            let probe_result = (|| -> std::io::Result<std::os::unix::net::UnixStream> {
                let s = std::os::unix::net::UnixStream::connect(&sock_path)?;
                let _ = s.set_read_timeout(Some(probe_timeout));
                let _ = s.set_write_timeout(Some(probe_timeout));
                Ok(s)
            })();
            match probe_result {
                Ok(_) => {
                    anyhow::bail!(
                        "Another daemon is already listening on {}",
                        sock_path.display()
                    );
                }
                Err(_) => {
                    // Don't blindly unlink whatever is at sock_path — an
                    // attacker (or a stale test artifact) could leave a
                    // symlink or regular file there and trick us into deleting
                    // something we shouldn't. Use symlink_metadata (no follow)
                    // and refuse to remove anything that isn't a socket or a
                    // plain file in the cqs dir.
                    //
                    // Ensure the socket's parent directory is 0o700 BEFORE the
                    // cleanup-then-bind sequence. With a private parent dir, a
                    // hostile local user can't plant their own socket at
                    // sock_path during the TOCTOU gap between `remove_file` and
                    // `bind`. Failure to secure the parent (symlink, mode
                    // tightening fails, etc.) is fatal — better to refuse to
                    // start than serve from a world-writable dir.
                    if let Err(e) = cqs::daemon_translate::ensure_socket_parent_dir(&sock_path) {
                        anyhow::bail!(
                            "Failed to secure daemon socket parent directory: {} (SEC-V1.36-10)",
                            e
                        );
                    }
                    use std::os::unix::fs::FileTypeExt;
                    match std::fs::symlink_metadata(&sock_path) {
                        Ok(md) => {
                            let ft = md.file_type();
                            if ft.is_symlink() || ft.is_dir() {
                                anyhow::bail!(
                                    "Refusing to remove non-socket path {} (symlink/dir); resolve manually before starting daemon",
                                    sock_path.display()
                                );
                            }
                            if !(ft.is_socket() || ft.is_file()) {
                                anyhow::bail!(
                                    "Refusing to remove non-socket path {} (unexpected file type); resolve manually before starting daemon",
                                    sock_path.display()
                                );
                            }
                            if let Err(e) = std::fs::remove_file(&sock_path) {
                                tracing::warn!(
                                    error = %e,
                                    path = %sock_path.display(),
                                    "Failed to remove stale socket file"
                                );
                            } else {
                                tracing::debug!(path = %sock_path.display(), "Removed stale socket file");
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Raced with another cleanup — nothing to do.
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                path = %sock_path.display(),
                                "Failed to stat socket path before cleanup"
                            );
                        }
                    }
                }
            }
        }
        // Between `bind()` (creates socket honoring umask) and
        // `set_permissions(0o600)`, the socket inode is world-creatable
        // for ~ms. On `/tmp` fallback (`XDG_RUNTIME_DIR` unset) any local
        // user could connect during that window. Set umask to 0o077
        // immediately before bind so the socket is born private, then
        // restore. Keep the explicit chmod as belt-and-suspenders in case
        // a future refactor drops the umask wrap.
        //
        // SAFETY: `libc::umask` is process-global. We do this on the daemon
        // startup path before any concurrent file-creating code runs.
        #[cfg(unix)]
        let prev_umask = unsafe { libc::umask(0o077) };
        let listener = std::os::unix::net::UnixListener::bind(&sock_path)
            .with_context(|| format!("Failed to bind socket at {}", sock_path.display()))?;
        #[cfg(unix)]
        unsafe {
            libc::umask(prev_umask);
        }
        listener.set_nonblocking(true)?;
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::warn!(
                    error = %e,
                    path = %sock_path.display(),
                    "Failed to set socket permissions to 0o600"
                );
            }
        }
        tracing::info!(
            socket = %sock_path.display(),
            pid = std::process::id(),
            "Daemon listening"
        );
        if !cli.quiet {
            println!("Daemon listening on {}", sock_path.display());
        }
        // Self-maintaining env snapshot — iterate every CQS_* variable
        // instead of a hardcoded whitelist that drifts as new knobs are
        // added. Env vars set on client subprocesses do NOT affect
        // daemon-served queries; only the daemon's own env applies.
        //
        // Redact secrets — any var whose name *contains* a known secret
        // marker is logged with `<redacted len=N>` instead of the value.
        // A value-shape check also redacts any URL with embedded userinfo
        // (`scheme://user:pass@host`), e.g. creds in CQS_LLM_API_BASE.
        const SECRET_MARKERS: &[&str] = &[
            "KEY", "TOKEN", "SECRET", "PASSWORD", "BEARER", "AUTH", "CRED", "PASS",
        ];
        let value_has_userinfo = |v: &str| -> bool {
            // Match "scheme://user:pass@host..." — the `:` between user and
            // pass plus the `@` separator. Cheap heuristic, no URL parser.
            if let Some(after_scheme) = v.split_once("://").map(|(_, rest)| rest) {
                if let Some(at_pos) = after_scheme.find('@') {
                    return after_scheme[..at_pos].contains(':');
                }
            }
            false
        };
        let cqs_vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("CQS_"))
            .map(|(k, v)| {
                let upper = k.to_ascii_uppercase();
                let is_secret =
                    SECRET_MARKERS.iter().any(|m| upper.contains(m)) || value_has_userinfo(&v);
                let value = if is_secret {
                    format!("<redacted len={}>", v.len())
                } else {
                    v
                };
                (k, value)
            })
            .collect();
        tracing::info!(cqs_vars = ?cqs_vars, "Daemon env snapshot");
        Some((listener, sock_path))
    } else {
        None
    };
    #[cfg(unix)]
    let _socket_guard = socket_listener
        .as_ref()
        .map(|(_, path)| SocketCleanupGuard(path.clone()));
    // On non-unix platforms the daemon socket path is #[cfg(unix)]-only, so
    // --serve would otherwise silently no-op. Warn both on stderr (so
    // interactive users notice without --log-level=warn) and via tracing
    // (for systemd-style journals that scrape our output).
    #[cfg(not(unix))]
    if serve {
        eprintln!(
            "Warning: --serve is unix-only (daemon socket uses Unix domain sockets); \
             falling back to plain watch mode"
        );
        tracing::warn!("--serve requested on non-unix platform; daemon disabled");
    }

    // Allocate the shared embedder slot before spawning the daemon thread so
    // the Arc can be cloned into the thread's closure and adopted by its
    // BatchContext. The slot starts empty; whichever side initializes first
    // (daemon via `ctx.warm()` or watch via `try_init_embedder`) wins and the
    // other reuses the same Arc.
    let shared_embedder: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<Embedder>>> =
        std::sync::Arc::new(std::sync::OnceLock::new());

    // Build ONE tokio runtime and share it across the outer Store
    // (read-write, for reindex writes) and the daemon thread's inner Store
    // (read-only, for queries) plus its EmbeddingCache/QueryCache. Otherwise
    // each constructor spawns its own 1-4 worker threads that never overlap
    // usefully. `shared_rt` must be declared before the daemon thread spawn
    // below so we can `Arc::clone` into the closure; it stays alive until
    // this function returns, after the daemon thread is joined.
    let shared_rt = build_shared_runtime()
        .with_context(|| "Failed to build shared tokio runtime for daemon")?;

    // Shared `Arc<RwLock<WatchSnapshot>>` for cross-thread freshness
    // publishing. Allocated *before* the daemon spawn so the Arc can be cloned
    // into both the watch loop (writer) and the daemon thread's BatchContext
    // (reader via `dispatch_status`). Initial value is the `unknown` snapshot;
    // the watch loop overwrites it once per cycle below.
    let watch_snapshot_handle: cqs::watch_status::SharedWatchSnapshot =
        cqs::watch_status::shared_unknown();

    // Cross-thread one-shot reconcile signal. Allocated alongside
    // `watch_snapshot_handle` so a single Arc clone reaches each side. Watch
    // loop swaps it back to `false` after running the on-demand reconcile
    // pass; daemon's `dispatch_reconcile` flips it to `true`.
    let reconcile_signal_handle: cqs::watch_status::SharedReconcileSignal =
        cqs::watch_status::shared_reconcile_signal();

    // Cross-thread event-driven freshness notifier. Watch loop's
    // `publish_watch_snapshot` calls `set_fresh` every cycle; the daemon's
    // `wait_fresh` handler parks on `wait_until_fresh`. Single round-trip,
    // zero busy-poll.
    let fresh_notifier_handle: cqs::watch_status::SharedFreshNotifier =
        cqs::watch_status::shared_fresh_notifier();

    // Shared in-flight client counter. The daemon accept loop
    // increments/decrements it per connection; the watch loop samples it
    // every snapshot publish so `cqs status --watch` can report it.
    // Stays at 0 when `--serve` is off (no daemon thread to count).
    let in_flight_clients_handle: Arc<std::sync::atomic::AtomicUsize> =
        Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Pick up a leftover `.cqs/.dirty` marker from a previous session where
    // a git hook fired without a daemon listening.
    // The hook touches this file as a fallback; on next daemon start we
    // promote it into a one-shot reconcile request and remove the marker.
    let dirty_marker_path = cqs_dir.join(".dirty");
    if dirty_marker_path.exists() {
        reconcile_signal_handle.store(true, std::sync::atomic::Ordering::Release);
        if let Err(e) = std::fs::remove_file(&dirty_marker_path) {
            tracing::warn!(
                error = %e,
                path = %dirty_marker_path.display(),
                ".cqs/.dirty present but could not be removed"
            );
        } else {
            tracing::info!(
                path = %dirty_marker_path.display(),
                "Promoted .cqs/.dirty into a one-shot reconcile request"
            );
        }
    }

    // Spawn dedicated socket handler thread — runs independently of the file
    // watcher so queries are served immediately, even during the slow poll scan.
    //
    // Keep the `JoinHandle` in a named `socket_thread` so the main loop can
    // `.take().join()` it on shutdown with a bounded wait. A detached thread
    // would let the daemon's BatchContext (~500MB+ ONNX sessions, SQLite
    // pool, HNSW Arc, optional CAGRA GPU resources) live past the main loop's
    // return with no WAL checkpoint and no `Drop` ordering — and under
    // `cargo install` or shell Ctrl+C the orphaned thread could block stdout
    // writes.
    #[cfg(unix)]
    let mut socket_thread: Option<std::thread::JoinHandle<()>> = if serve {
        if let Some((listener, _)) = socket_listener.take() {
            // Clone the shared OnceLock into the daemon closure so both the
            // outer watch loop and BatchContext see the same Arc<Embedder>.
            let daemon_embedder = std::sync::Arc::clone(&shared_embedder);
            // Index-aware model resolution for the daemon's embedder. Prefer
            // the model recorded in the store metadata so a wrong-model
            // CQS_EMBEDDING_MODEL doesn't silently produce zero-result queries
            // (the dim mismatch otherwise only surfaces as a tracing::warn!).
            // See ROADMAP.md "Embedder swap workflow" for the longer story.
            let daemon_model_config =
                resolve_index_aware_model_for_watch(&index_path, &root, cli.model.as_deref())?;
            // Clone the shared runtime handle into the daemon closure so its
            // BatchContext opens its Store/EmbeddingCache/QueryCache on the
            // same multi-thread pool as the outer watch loop.
            let daemon_runtime = Arc::clone(&shared_rt);
            // Clone the shared snapshot Arc into the daemon thread.
            let daemon_watch_snapshot = Arc::clone(&watch_snapshot_handle);
            // Clone the reconcile-signal Arc too.
            let daemon_reconcile_signal = Arc::clone(&reconcile_signal_handle);
            // Clone the freshness notifier so the daemon's `wait_fresh`
            // handler shares the same notifier the watch loop publishes
            // through.
            let daemon_fresh_notifier = Arc::clone(&fresh_notifier_handle);
            // Clone the in-flight counter so the accept loop's
            // per-connection bookkeeping is visible to the watch loop's
            // snapshot publisher.
            let daemon_in_flight = Arc::clone(&in_flight_clients_handle);
            // Stays non-blocking: the accept loop below polls so it can
            // notice SHUTDOWN_REQUESTED on SIGTERM.
            let thread = daemon::spawn_daemon_thread(
                listener,
                daemon_embedder,
                daemon_model_config,
                daemon_runtime,
                daemon_watch_snapshot,
                daemon_reconcile_signal,
                daemon_fresh_notifier,
                daemon_in_flight,
            );
            Some(thread)
        } else {
            None
        }
    } else {
        None
    };

    let parser = CqParser::new()?;
    let supported_ext: HashSet<_> = parser.supported_extensions().iter().cloned().collect();

    println!(
        "Watching {} for changes (Ctrl+C to stop)...",
        root.display()
    );
    println!(
        "Code extensions: {}",
        supported_ext.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    println!("Also watching: docs/notes.toml");

    // Watch does not run SPLADE encoding on new chunks. The v20 trigger on
    // `chunks` DELETE ensures sparse correctness (the persisted
    // splade.index.bin gets invalidated when chunks are removed), but
    // newly-added chunks have no sparse vectors until a manual `cqs index`
    // runs. If a user has
    // CQS_SPLADE_MODEL set expecting full SPLADE coverage to be
    // maintained live, tell them up front that they still need to rerun
    // `cqs index` for fresh coverage on new chunks.
    if cqs::splade::resolve_splade_model_dir().is_some() {
        println!(
            "⚠ SPLADE model configured but watch mode does not refresh sparse vectors for \
             newly-added chunks. Run 'cqs index' after a stable edit session to restore \
             full SPLADE coverage. Sparse correctness for removed chunks is maintained \
             automatically via the v20 schema trigger."
        );
        tracing::warn!(
            "Watch mode does not re-run SPLADE encoding — new chunks will have no sparse \
             vectors until manual 'cqs index'. Removals are handled via the v20 chunks-delete \
             trigger."
        );
    }

    let (tx, rx) = mpsc::channel();

    // Poll interval is separate from debounce. PollWatcher walks the entire
    // tree on every tick — on WSL DrvFS each entry is a 9P round-trip, so a
    // short interval burns ~8% of one core continuously on a ~16k-file tree.
    // Default to 5000ms (still fast enough for save → reindex), override with
    // `CQS_WATCH_POLL_MS`. Inotify watchers
    // ignore the value but the field exists in `Config`, so we set it
    // unconditionally and let the watcher type decide.
    let poll_ms = std::env::var("CQS_WATCH_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&ms| ms >= 100)
        .unwrap_or(5000);
    let config = Config::default().with_poll_interval(Duration::from_millis(poll_ms));

    // Box<dyn Watcher> so both watcher types work with the same variable
    let mut watcher: Box<dyn Watcher> = if use_poll {
        println!("Using poll watcher (interval: {}ms)", poll_ms);
        Box::new(PollWatcher::new(tx, config)?)
    } else {
        Box::new(RecommendedWatcher::new(tx, config)?)
    };

    // Warn when the project tree approaches the inotify watch limit.
    // notify::watch(Recursive) registers a watch per directory; on distros
    // with the old default of 8192 a moderately-deep monorepo exhausts the
    // limit and per-subdir registration failures are silent. We don't fail
    // here — the watch still works for whatever directories were registered
    // — but we emit a loud warning with the recommended fix so operators
    // know why some saves stopped triggering reindex.
    #[cfg(target_os = "linux")]
    if !use_poll {
        if let Ok(limit_str) = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches") {
            if let Ok(limit) = limit_str.trim().parse::<usize>() {
                let dir_count = count_watchable_dirs(&root);
                if dir_count * 10 > limit * 9 {
                    tracing::warn!(
                        dir_count,
                        limit,
                        "inotify watch limit nearly exhausted — saves in some subdirectories \
                         may not trigger reindex. Either run `cqs watch --poll` or raise the \
                         limit with: sudo sysctl -w fs.inotify.max_user_watches={}",
                        limit * 4
                    );
                    if !cli.quiet {
                        eprintln!(
                            "[warn] inotify watch limit ({}) nearly exhausted by {} dirs in this tree.\n\
                             [warn]   Either run `cqs watch --poll` or raise the limit:\n\
                             [warn]     sudo sysctl -w fs.inotify.max_user_watches={}",
                            limit, dir_count, limit * 4
                        );
                    }
                }
            }
        }
    }

    watcher.watch(&root, RecursiveMode::Recursive)?;

    let notes_path = root.join("docs/notes.toml");
    let cqs_dir = dunce::canonicalize(&cqs_dir).unwrap_or_else(|e| {
        tracing::debug!(path = %cqs_dir.display(), error = %e, "canonicalize failed, using original");
        cqs_dir
    });
    let notes_path = dunce::canonicalize(&notes_path).unwrap_or_else(|e| {
        tracing::debug!(path = %notes_path.display(), error = %e, "canonicalize failed, using original");
        notes_path
    });

    // Embedder is declared above (before daemon thread spawn) so its
    // OnceLock can be shared with the daemon thread.

    // Open store and reuse across reindex operations within a cycle.
    // Caches are cleared after each reindex cycle to drop stale OnceLock
    // caches. The outer store shares `shared_rt` so the daemon's inner
    // read-only store and its caches all run on one multi-thread pool
    // instead of three isolated runtimes.
    let mut store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    // Resolve the vendored-path prefix list once at daemon startup so all
    // subsequent inserts (incremental + reconcile-driven) get correct
    // `vendored` flags. Reads `[index].vendored_paths` from
    // `.cqs.toml`; falls back to the built-in default list when absent.
    // Captured into a local `Vec<String>` so the DB-replaced reopen
    // paths below can re-stamp the freshly-opened Store without
    // re-reading the config file.
    let vendored_prefixes_for_store: Vec<String> = {
        let cfg = cqs::config::Config::load(&root);
        let vendored_override = cfg
            .index
            .as_ref()
            .and_then(|ic| ic.vendored_paths.as_deref());
        cqs::vendored::effective_prefixes(vendored_override)
    };
    store.set_vendored_prefixes(vendored_prefixes_for_store.clone());

    // Track the database file identity so we detect when `cqs index --force`
    // replaces it. Without this check, watch's Store handle would point at the
    // orphaned (renamed) inode and writes would silently vanish.
    let mut db_id = db_file_identity(&index_path);

    // Persistent HNSW state for incremental updates.
    //
    // The watch loop keeps an *Owned* HnswIndex in memory so `insert_batch`
    // can append new chunks without rebuilding the graph from scratch. After
    // every `hnsw_rebuild_threshold()` incremental inserts
    // we trigger a full rebuild to clean orphan vectors (hnsw_rs has no
    // delete; updated chunks leave their old vectors behind).
    //
    // At startup we load the persisted index from disk for instant search
    // availability, and *immediately* spawn a background rebuild so we end up
    // with an Owned variant ready before the first file save — without paying
    // a 10-15s cold-start hit. The Loaded variant cannot be mutated (hnsw_rs
    // constraint), so without this swap the first save after restart would
    // fail incremental insert and force a synchronous full rebuild, blocking
    // the editor for 15s. Spawning the rebuild off-thread keeps the daemon
    // responsive throughout.
    //
    // Starting `incremental_count` at threshold/2 (when we loaded an existing
    // index) means stale orphans from prior sessions get cleaned sooner; the
    // cleanup is async via the same pending_rebuild path.
    let (hnsw_index, incremental_count, pending_rebuild) =
        match HnswIndex::load_with_dim(cqs_dir.as_ref(), "index", store.dim()) {
            Ok(index) => {
                let n = index.len();
                info!(vectors = n, "Loaded existing HNSW index from disk");
                // Spawn background rebuild so we get an Owned variant ASAP
                // (incremental insert needs Owned, Loaded is immutable).
                let pending = spawn_hnsw_rebuild(
                    cqs_dir.clone(),
                    index_path.clone(),
                    store.dim(),
                    "startup_owned_swap",
                );
                (Some(index), hnsw_rebuild_threshold() / 2, Some(pending))
            }
            Err(ref e) if matches!(e, cqs::hnsw::HnswError::NotFound(_)) => {
                tracing::debug!("No prior HNSW index, starting fresh");
                (None, 0, None)
            }
            Err(e) => {
                // Log so the operator sees why the prior index was discarded
                // (DimensionMismatch, IO error, corruption) rather than
                // treating it silently as a first run.
                tracing::warn!(error = %e, "Existing HNSW index unusable, rebuilding from scratch");
                (None, 0, None)
            }
        };

    // Index-aware model resolution: prefer the model recorded in the open
    // store metadata over CLI flag / env / config / default. Without this,
    // running `cqs watch` with `CQS_EMBEDDING_MODEL=wrong-model` would embed
    // new chunks with a different dim than the index, corrupting
    // incremental reindex.
    //
    // The lossy `stored_model_name()` returns `None` on real SQL errors
    // (corrupt metadata, schema skew, sqlite I/O), not just on fresh DBs.
    // Without surfacing the error the watch loop falls back to
    // CLI/env/config resolution and silently writes wrong-dim embeddings
    // into the live store. Use the strict variant and warn loudly on read
    // failure so journald shows the cause.
    let stored_model_for_watch = match store.try_stored_model_name() {
        Ok(opt) => opt,
        Err(e) => {
            tracing::error!(
                error = %e,
                index_path = %index_path.display(),
                "Watch loop failed to read stored_model_name from metadata — \
                 falling back to CLI/env/config resolution. If the index \
                 actually has a recorded model, the fallback may produce a \
                 wrong-dim embedder and corrupt the incremental reindex; \
                 stop the daemon and run `cqs index --force` to repair."
            );
            None
        }
    };
    let project_config_for_watch = cqs::config::Config::load(&root);
    let model_config_owned = ModelConfig::resolve_for_query(
        stored_model_for_watch.as_deref(),
        cli.model.as_deref(),
        project_config_for_watch.embedding.as_ref(),
    )
    .apply_env_overrides();
    tracing::info!(
        stored = stored_model_for_watch.as_deref().unwrap_or("<none>"),
        resolved = %model_config_owned.name,
        dim = model_config_owned.dim,
        "Watch loop resolved index-aware model config"
    );
    let model_config = &model_config_owned;

    // Discover sibling slots for slot-parallel delta propagation. The
    // set is fixed for the daemon's lifetime (slots created later need a
    // restart, same contract as slot promotion). Same-model siblings
    // ride the active cycle's cache write-backs; foreign-model siblings
    // are inert unless CQS_WATCH_ALL_SLOTS=1. Disable entirely with
    // CQS_WATCH_SIBLING_SLOTS=0.
    let mut sibling_slots = if cqs::slot::slots_root(&project_cqs_dir).exists() {
        SiblingSet::discover(
            &project_cqs_dir,
            &active_slot.name,
            model_config,
            SiblingPolicy::from_env(),
        )
    } else {
        SiblingSet::empty()
    };

    // Build the gitignore matcher once at startup. `no_ignore` (CLI)
    // and `CQS_WATCH_RESPECT_GITIGNORE=0` (env) both disable it. Held in
    // `RwLock<Option<_>>` so a `.gitignore` change can be hot-swapped
    // without restart.
    let gitignore = std::sync::RwLock::new(if no_ignore {
        tracing::info!("--no-ignore passed — gitignore filtering disabled");
        None
    } else {
        build_gitignore_matcher(&root)
    });

    // Daemon startup GC. Two-pass sweep — drop chunks whose origin is gone
    // from disk (Pass 1) and drop chunks whose path is now matched by
    // `.gitignore` (Pass 2, retroactive cleanup of worktree pollution).
    // Only runs in `--serve` mode (the systemd unit) and is
    // disabled by `CQS_DAEMON_STARTUP_GC=0`. Synchronous on the main
    // thread so the daemon socket sees a clean index from the first
    // accepted connection.
    //
    // Acquires the index lock non-blockingly via `try_acquire_index_lock`
    // — if a concurrent `cqs index` already holds the lock, we skip the
    // startup pass and let the next periodic-GC tick catch up. Blocking
    // here would defeat `cqs index`'s expectation that the daemon
    // releases the lock between reindex cycles.
    if serve {
        match try_acquire_index_lock(&cqs_dir) {
            Ok(Some(gc_lock)) => {
                // Recover from RwLock poison. A poisoned read usually means a
                // writer panicked mid-update; the written matcher is still
                // valid data. Dropping to "no matcher" would silently
                // re-index ignored files (including `.env.secret`).
                // `into_inner()` on the `PoisonError` keeps the matcher
                // visible.
                let matcher_guard = match gitignore.read() {
                    Ok(g) => Some(g),
                    Err(poisoned) => {
                        tracing::error!(
                            "Gitignore RwLock poisoned — recovering. Previous matcher is still valid; indexing continues with it."
                        );
                        Some(poisoned.into_inner())
                    }
                };
                let matcher_ref = matcher_guard.as_ref().and_then(|g| g.as_ref());
                run_daemon_startup_gc(&store, &root, &parser, matcher_ref);
                // Explicit drop so the read lock is released before the watch
                // loop starts taking it on every event.
                drop(matcher_guard);
                drop(gc_lock);
                // Clear caches so subsequent queries observe the pruned rows.
                store.clear_caches();
                db_id = db_file_identity(&index_path);
            }
            Ok(None) => {
                tracing::info!(
                    "Daemon startup GC: index lock held by another process — skipping (periodic GC will catch up)"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Daemon startup GC: failed to acquire index lock — skipping"
                );
            }
        }
    }

    // Build the SPLADE encoder once at startup. `None` means
    // incremental SPLADE is disabled for this daemon lifetime — either
    // the model isn't configured, failed to load, or the operator set
    // `CQS_WATCH_INCREMENTAL_SPLADE=0`. Existing sparse vectors in the
    // DB are preserved in all cases.
    let splade_encoder_storage = build_splade_encoder_for_watch().map(std::sync::Mutex::new);
    let splade_encoder_ref: Option<&std::sync::Mutex<cqs::splade::SpladeEncoder>> =
        splade_encoder_storage.as_ref();

    // Open the project-scoped global embedding cache once at daemon startup
    // so reindex cycles can hit it without paying open() per cycle. Mirrors
    // the bulk pipeline's gating on `CQS_CACHE_ENABLED=0`. Open failure is
    // best-effort: log and continue with `None`, identical to the bulk
    // path's degradation. Reuses `shared_rt` so this Cache piggybacks on the
    // same worker pool as the outer Store, daemon Store/Cache, etc.
    let global_cache_storage: Option<cqs::cache::EmbeddingCache> = {
        if std::env::var("CQS_CACHE_ENABLED").as_deref() == Ok("0") {
            tracing::info!(
                "CQS_CACHE_ENABLED=0 — global embedding cache disabled for watch reindex"
            );
            None
        } else {
            let cache_path = cqs::cache::EmbeddingCache::project_default_path(&project_cqs_dir);
            match cqs::cache::EmbeddingCache::open_with_runtime(
                &cache_path,
                Some(Arc::clone(&shared_rt)),
            ) {
                Ok(c) => {
                    tracing::info!(
                        path = %cache_path.display(),
                        "Watch reindex global embedding cache opened"
                    );
                    Some(c)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %cache_path.display(),
                        "Watch reindex global cache unavailable; proceeding without it"
                    );
                    None
                }
            }
        }
    };
    let global_cache_ref: Option<&cqs::cache::EmbeddingCache> = global_cache_storage.as_ref();

    let watch_cfg = WatchConfig {
        root: &root,
        cqs_dir: &cqs_dir,
        notes_path: &notes_path,
        supported_ext: &supported_ext,
        parser: &parser,
        embedder: shared_embedder.as_ref(),
        quiet: cli.quiet,
        model_config,
        gitignore: &gitignore,
        splade_encoder: splade_encoder_ref,
        global_cache: global_cache_ref,
    };

    let mut state = WatchState {
        embedder_backoff: EmbedderBackoff::new(),
        pending_files: HashSet::new(),
        pending_notes: false,
        last_event: std::time::Instant::now(),
        first_pending_event: None,
        // Track last-indexed mtime per file to skip duplicate WSL/NTFS events.
        // On WSL, inotify over 9P delivers repeated events for the same file change.
        // Bounded: pruned when >10k entries or >1k entries on single-file reindex.
        last_indexed_mtime: HashMap::with_capacity(1024),
        hnsw_index,
        incremental_count,
        dropped_this_cycle: 0,
        pending_rebuild,
        // Seed throttle so the very first publish tick does re-stat
        // `index.db` (the cache starts empty). After that the cadence is
        // `LAST_SYNCED_REFRESH` between calls. `checked_sub`
        // guards against the (theoretical) freshly-booted-machine case
        // where `Instant::now()` is < `LAST_SYNCED_REFRESH` since boot —
        // falling back to `Instant::now()` just means the *second* tick
        // is the one that reads, not the first.
        last_metadata_check: std::time::Instant::now()
            .checked_sub(LAST_SYNCED_REFRESH)
            .unwrap_or_else(std::time::Instant::now),
        cached_last_synced_at: None,
        active_slot: active_slot.name.clone(),
        last_reindex: None,
        last_error: None,
        // Seed with the store state as of daemon startup so the very first
        // HNSW save already detects foreign writers. Read failure → None;
        // the first save falls back to the live stamp.
        observed_stamp: cqs::hnsw::StoreStamp::read(&store).ok(),
    };

    let mut cycles_since_clear: u32 = 0;
    // Track last eviction of the global embedding cache so the reindex path
    // only trims once per hour, keeping the WAL file from churning on every
    // micro-edit.
    let mut last_cache_evict = std::time::Instant::now();

    // Track last periodic GC tick. Initialised to "now" so the
    // first periodic sweep doesn't fire until the full interval
    // (`daemon_periodic_gc_interval_secs()`) has elapsed after startup —
    // the startup pass already covered the initial state.
    // Disabled when --serve is off (this is a daemon-only feature) or
    // when CQS_DAEMON_PERIODIC_GC=0.
    let periodic_gc_enabled =
        serve && std::env::var("CQS_DAEMON_PERIODIC_GC").as_deref() != Ok("0");
    if !periodic_gc_enabled && serve {
        tracing::info!("CQS_DAEMON_PERIODIC_GC=0 — periodic idle-time GC disabled");
    }
    let mut last_periodic_gc = std::time::Instant::now();

    // Cadence for the age-only prune of `last_indexed_mtime`. The size-gated
    // prune only fires when the map crosses
    // `CQS_WATCH_PRUNE_SIZE_THRESHOLD`; this age-only tick fires once per
    // hour from the idle branch so daemons that stay below the threshold
    // still shed stale entries.
    const LAST_INDEXED_AGE_PRUNE_INTERVAL_SECS: u64 = 3600;
    let mut last_age_prune = std::time::Instant::now();

    // Periodic full-tree reconciliation cadence. Same gating model as the
    // GC tick — `--serve` only, opt-out via
    // `CQS_WATCH_RECONCILE=0`. Initialised to "now" so the first tick
    // fires on the same `daemon_reconcile_interval_secs()` cadence as
    // every subsequent one (no startup walk; the inotify watcher already
    // sees fresh changes for the time window between daemon start and
    // first interval).
    let reconcile_enabled_flag = serve && reconcile_enabled();
    if !reconcile_enabled_flag && serve {
        tracing::info!("CQS_WATCH_RECONCILE=0 — periodic full-tree reconciliation disabled");
    }
    let mut last_reconcile = std::time::Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                collect_events(&event, &watch_cfg, &mut state);
            }
            Ok(Err(e)) => {
                // Include `kind` and `paths` so operators can distinguish
                // MaxFilesWatch (raise sysctl) from Io(broken pipe) (restart
                // daemon). Display alone collapses the discriminator and drops
                // paths.
                warn!(
                    error = %e,
                    kind = ?e.kind,
                    paths = ?e.paths,
                    "Watch error"
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Out-of-band reconcile request. The daemon's
                // `dispatch_reconcile` flips this AtomicBool to
                // `true` when a `cqs hook fire` client posts a
                // `reconcile` socket message. Bypasses the periodic
                // tick's idle gating: a git operation is itself the
                // user signal, so reconciling immediately is correct
                // even when `pending_files` is empty (which is exactly
                // the case `cqs watch` was missing — bulk git ops with
                // no inotify events).
                //
                // Coalesces repeated requests into a single walk: a
                // `git rebase -i` firing post-rewrite once per replayed
                // commit only triggers one walk on the next tick.
                let on_demand_reconcile_requested =
                    reconcile_signal_handle.swap(false, std::sync::atomic::Ordering::AcqRel);
                if on_demand_reconcile_requested && reconcile_enabled_flag {
                    let queued = run_daemon_reconcile(
                        &store,
                        &root,
                        &parser,
                        no_ignore,
                        &mut state.pending_files,
                        max_pending_files(),
                    );
                    tracing::info!(
                        queued,
                        pending_total = state.pending_files.len(),
                        "On-demand reconcile (#1182 Layer 1) drained"
                    );
                    // Slot-aware pass: the same git-operation signal that
                    // triggered the active reconcile applies to every
                    // propagated sibling. Without this, repeated hook
                    // fires (which slide `last_reconcile`) could starve
                    // sibling reconciliation indefinitely.
                    if !sibling_slots.is_empty() {
                        sibling_slots.reconcile_siblings(
                            &root,
                            &parser,
                            no_ignore,
                            max_pending_files(),
                            None,
                            &shared_rt,
                        );
                    }
                    if queued > 0 {
                        // Reset `last_event` so the synthetic pending
                        // entries flush one quiet-gap from queue time
                        // (otherwise a stale `last_event` could fire
                        // the flush mid-queue on a later tick). Arm the
                        // max-latency clock too so a continuous event
                        // stream can't starve the queued entries.
                        state.last_event = std::time::Instant::now();
                        if state.first_pending_event.is_none() {
                            state.first_pending_event = Some(state.last_event);
                        }
                    }
                    // Sliding the periodic-tick clock keeps the two
                    // mechanisms from racing: the next periodic walk
                    // waits a full `daemon_reconcile_interval_secs()`
                    // after this on-demand walk.
                    last_reconcile = std::time::Instant::now();
                }

                // Flush decision lives after the match (see the
                // `flush_due` block below) so it is evaluated on event
                // arrivals too, not just on quiet ticks. The idle
                // housekeeping here only runs when no flush is due.
                if !flush_due(&state, &debounce) {
                    // Age-only prune fires hourly on idle ticks regardless of
                    // map size. The size-gated `prune_last_indexed_mtime`
                    // still runs in the event path; this idle-tick variant
                    // catches daemons that sit below the threshold but
                    // accumulate stale entries from deleted/moved files.
                    if last_age_prune.elapsed()
                        >= Duration::from_secs(LAST_INDEXED_AGE_PRUNE_INTERVAL_SECS)
                    {
                        let removed =
                            prune_last_indexed_mtime_by_age(&mut state.last_indexed_mtime);
                        if removed > 0 {
                            tracing::debug!(
                                removed,
                                remaining = state.last_indexed_mtime.len(),
                                "Age-only prune of last_indexed_mtime fired (idle tick)"
                            );
                        }
                        last_age_prune = std::time::Instant::now();
                    }

                    cycles_since_clear += 1;
                    // Clear embedder session and HNSW index after ~5 minutes idle
                    // (3000 cycles at 100ms). Frees GPU/memory when watch is idle.
                    //
                    // The shared Arc<Embedder> is also held by the daemon
                    // thread's BatchContext. clear_session is safe either way:
                    // the ONNX session is behind a Mutex and the tokenizer is
                    // Mutex<Option<Arc<…>>>.
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = shared_embedder.get() {
                            emb.clear_session();
                        }
                        // Do NOT reset incremental_count on idle-clear. The
                        // counter's contract is "incremental inserts since
                        // last full rebuild"; a 5-minute idle hasn't changed
                        // the on-disk delta. Resetting here would make the
                        // next file event start the threshold timer from
                        // scratch and understate delta size, delaying the
                        // rebuild that should fire on accumulated drift.
                        state.hnsw_index = None;
                        cycles_since_clear = 0;
                    }

                    // Idle-time periodic GC. Only fires when
                    //   (a) `--serve` is on AND `CQS_DAEMON_PERIODIC_GC` != "0",
                    //   (b) the last actual file event was more than
                    //       `daemon_periodic_gc_idle_secs()` ago (so a long
                    //       burst of edits never triggers GC mid-burst), AND
                    //   (c) the previous tick was more than
                    //       `daemon_periodic_gc_interval_secs()` ago.
                    // The bounded sweep (cap = daemon_periodic_gc_cap()) keeps
                    // each tick's write transaction short.
                    //
                    // When both gc and reconcile gates fire on the same idle
                    // tick, do one disk walk and pass it to both consumers.
                    // The two intervals share the idle gate
                    // (`daemon_periodic_gc_idle_secs`), so on long-quiet ticks
                    // at the gc cadence boundary, both would otherwise walk
                    // the tree back-to-back.
                    let gc_due = periodic_gc_enabled
                        && state.last_event.elapsed()
                            >= Duration::from_secs(super::limits::daemon_periodic_gc_idle_secs())
                        && last_periodic_gc.elapsed()
                            >= Duration::from_secs(
                                super::limits::daemon_periodic_gc_interval_secs(),
                            );
                    let reconcile_due = reconcile_enabled_flag
                        && state.last_event.elapsed()
                            >= Duration::from_secs(super::limits::daemon_periodic_gc_idle_secs())
                        && last_reconcile.elapsed()
                            >= Duration::from_secs(super::limits::daemon_reconcile_interval_secs());
                    let shared_disk_files: Option<HashSet<PathBuf>> = if gc_due && reconcile_due {
                        let exts = parser.supported_extensions();
                        match cqs::enumerate_files(&root, &exts, no_ignore) {
                            Ok(v) => Some(v.into_iter().collect()),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Shared idle-tick walk failed; gc and reconcile fall back to per-callsite walks",
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };

                    // Acquires the same `acquire_index_lock` semantics by
                    // calling `try_acquire_index_lock` — if `cqs index` or
                    // `cqs gc` is running, the GC tick skips and tries again
                    // on the next interval.
                    if gc_due {
                        match try_acquire_index_lock(&cqs_dir) {
                            Ok(Some(gc_lock)) => {
                                // Same poison-recovery as startup GC above —
                                // dropping to "no matcher" would let periodic
                                // GC re-index gitignored files (the very ones
                                // the matcher was built to exclude).
                                let matcher_guard = match gitignore.read() {
                                    Ok(g) => Some(g),
                                    Err(poisoned) => {
                                        tracing::error!(
                                            "Gitignore RwLock poisoned — recovering. Previous matcher is still valid; periodic GC continues with it."
                                        );
                                        Some(poisoned.into_inner())
                                    }
                                };
                                let matcher_ref = matcher_guard.as_ref().and_then(|g| g.as_ref());
                                run_daemon_periodic_gc(
                                    &store,
                                    &root,
                                    &parser,
                                    matcher_ref,
                                    shared_disk_files.as_ref(),
                                );
                                drop(matcher_guard);
                                drop(gc_lock);
                                // Clear caches so the next query observes the pruned rows.
                                store.clear_caches();
                                db_id = db_file_identity(&index_path);
                                // GC prunes chunks (bumping the write
                                // generation); refresh the observed stamp so
                                // the next HNSW save doesn't mistake our own
                                // GC for a foreign writer and discard itself.
                                state.observed_stamp = cqs::hnsw::StoreStamp::read(&store).ok();
                            }
                            Ok(None) => {
                                tracing::debug!("Periodic GC: index lock held, skipping this tick");
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Periodic GC: failed to acquire index lock — skipping tick"
                                );
                            }
                        }
                        // Always advance the timer so a wedged lock or
                        // failed enumerate doesn't make us retry every loop.
                        last_periodic_gc = std::time::Instant::now();
                    }

                    // Periodic full-tree reconciliation. Sibling of the GC
                    // tick; same idle-gating model:
                    //   (a) `--serve` on AND `CQS_WATCH_RECONCILE` != "0",
                    //   (b) `last_event.elapsed() >= daemon_periodic_gc_idle_secs()`
                    //       — reuses the same idle threshold so a burst of
                    //       edits doesn't trigger reconcile mid-burst,
                    //   (c) `last_reconcile.elapsed() >= daemon_reconcile_interval_secs()`
                    //       (default 30 s — much faster cadence than GC).
                    //
                    // Reconcile only *queues* divergent files into
                    // `state.pending_files`; the next debounce tick drains
                    // the queue through the existing `process_file_changes`
                    // path. No parallel reindex code branch — every queued
                    // file gets the same correctness guarantees as inotify
                    // events.
                    //
                    // Reads only — no write transaction, no index lock
                    // needed. The `process_file_changes` drain on the next
                    // tick takes its own lock per its existing contract.
                    if reconcile_due {
                        // Detect a `cqs index --force` rotation that happened
                        // between idle ticks — the inotify branch only fires
                        // on actual filesystem events, and a long quiet period
                        // followed by a forced reindex would land us here with
                        // `store` pointing at the orphaned inode. Reopen on
                        // mismatch and skip this tick; the next interval will
                        // reconcile against the fresh DB.
                        let current_id = db_file_identity(&index_path);
                        if current_id != db_id {
                            info!(
                                "index.db replaced before reconcile tick — reopening store and \
                                 skipping this pass; next interval will fire against fresh DB"
                            );
                            drop(store);
                            store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
                                .with_context(|| {
                                format!(
                                    "Failed to re-open store at {} after DB replacement",
                                    index_path.display()
                                )
                            })?;
                            // Re-stamp vendored prefixes on the fresh Store —
                            // OnceLock is per-instance.
                            store.set_vendored_prefixes(vendored_prefixes_for_store.clone());
                            db_id = current_id;
                            state.hnsw_index = None;
                            state.incremental_count = 0;
                            // Fresh DB, fresh write history — re-seed the
                            // observed stamp from the replacement store.
                            state.observed_stamp = cqs::hnsw::StoreStamp::read(&store).ok();
                            if state.pending_rebuild.take().is_some() {
                                tracing::info!(
                                    "discarded in-flight HNSW rebuild after DB replacement \
                                     observed at reconcile tick"
                                );
                            }
                            last_reconcile = std::time::Instant::now();
                        } else {
                            let queued = reconcile::run_daemon_reconcile_with_walk(
                                &store,
                                &root,
                                &parser,
                                no_ignore,
                                &mut state.pending_files,
                                max_pending_files(),
                                shared_disk_files.as_ref(),
                            );
                            // Slot-aware pass (durability net for the
                            // in-memory sibling delta queues): every
                            // propagated sibling gets the same
                            // fingerprint reconciliation, sharing the
                            // pre-walked disk set when available.
                            if !sibling_slots.is_empty() {
                                sibling_slots.reconcile_siblings(
                                    &root,
                                    &parser,
                                    no_ignore,
                                    max_pending_files(),
                                    shared_disk_files.as_ref(),
                                    &shared_rt,
                                );
                            }
                            if queued > 0 {
                                // Reset `last_event` so the synthetic
                                // pending entries flush one quiet-gap
                                // from queue time; arm the max-latency
                                // clock so a continuous event stream
                                // can't starve them past the cap.
                                state.last_event = std::time::Instant::now();
                                if state.first_pending_event.is_none() {
                                    state.first_pending_event = Some(state.last_event);
                                }
                            }
                            last_reconcile = std::time::Instant::now();
                        }
                    }

                    // Sibling slot drains run strictly on idle ticks —
                    // active-slot work (the flush block below) always
                    // preempts them, and at most ONE slot drains per
                    // tick so fresh events get a look-in between slots.
                    // Same-model drains are SQLite-only (pure cache
                    // hits by construction); foreign drains carry their
                    // own embedder load and respect the hysteresis
                    // thresholds.
                    if !sibling_slots.is_empty() {
                        let _ = sibling_slots.drain_one(
                            &watch_cfg,
                            &mut state.embedder_backoff,
                            &shared_rt,
                            state.pending_rebuild.is_some(),
                        );
                    }
                }

                // Socket queries handled by dedicated thread (see _socket_thread above).
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!(
                    "File watcher disconnected unexpectedly. \
                     Hint: Restart 'cqs watch' to resume monitoring."
                );
            }
        }

        // Pre-drain decision. Evaluated on every loop iteration —
        // event arrivals included — because a continuous stream of
        // events arriving faster than the 100 ms recv timeout never
        // reaches the timeout arm at all; without this placement the
        // max-latency cap could never fire under exactly the load it
        // exists for.
        if flush_due(&state, &debounce) {
            cycles_since_clear = 0;

            // Acquire index lock before reindexing. If another process
            // (cqs index, cqs gc) holds it, skip this cycle.
            let lock = match try_acquire_index_lock(&cqs_dir) {
                Ok(Some(lock)) => lock,
                Ok(None) => {
                    info!("Index lock held by another process, skipping reindex cycle");
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create index lock file");
                    continue;
                }
            };

            // Detect if `cqs index --force` replaced the database
            // while we were waiting. If so, reopen the Store before processing
            // any changes — otherwise writes go to the orphaned inode.
            let current_id = db_file_identity(&index_path);
            if current_id != db_id {
                info!("index.db replaced (likely cqs index --force), reopening store");
                drop(store);
                // Reuse the shared runtime on re-open so the
                // replacement store keeps running on the same
                // multi-thread worker pool as its predecessor.
                store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
                    .with_context(|| {
                        format!(
                            "Failed to re-open store at {} after DB replacement",
                            index_path.display()
                        )
                    })?;
                // Re-stamp the vendored prefix list on the fresh Store
                // — the OnceLock is per-instance and a brand-new Store
                // starts empty. Use the cached resolution from startup
                // so we don't re-read .cqs.toml mid-watch.
                store.set_vendored_prefixes(vendored_prefixes_for_store.clone());
                // db_id updated below in the cache-clear path.
                state.hnsw_index = None;
                state.incremental_count = 0;
                // Fresh DB, fresh write history — re-seed the observed
                // stamp from the replacement store.
                state.observed_stamp = cqs::hnsw::StoreStamp::read(&store).ok();
                // Drop in-flight rebuild whose pending delta references
                // OLD DB chunk IDs. The rebuild thread sends into a
                // dropped receiver (no-op). Force a fresh rebuild on
                // the next threshold tick against the new DB.
                if state.pending_rebuild.take().is_some() {
                    tracing::info!(
                        "discarded in-flight HNSW rebuild after DB replacement; \
                         next threshold tick will rebuild against new DB",
                    );
                }
            }

            if !state.pending_files.is_empty() {
                process_file_changes(&watch_cfg, &store, &mut state, &mut sibling_slots);
            }

            if state.pending_notes {
                state.pending_notes = false;
                process_note_changes(&root, &store, cli.quiet, &mut state);
            }

            // Clear stale OnceLock caches (call_graph_cache,
            // test_chunks_cache) after index changes. Clear caches
            // instead of a full re-open to avoid pool teardown +
            // runtime creation + PRAGMA setup on every reindex cycle
            // over a 24/7 systemd lifetime.
            store.clear_caches();
            db_id = db_file_identity(&index_path);

            // Periodically evict the global embedding cache so
            // long-running watch sessions don't let the shared
            // ~/.cache/cqs/embeddings.db grow past its
            // CQS_CACHE_MAX_SIZE cap (default 10GB). Gated by
            // `last_cache_evict.elapsed()` so we don't churn the
            // SQLite file on every single reindex cycle. Reuses the
            // shared runtime so this one-shot eviction piggybacks on
            // the existing worker pool.
            if last_cache_evict.elapsed() >= Duration::from_secs(3600) {
                let project_cqs_dir = cqs::resolve_index_dir(&root);
                let cache_path = cqs::cache::EmbeddingCache::project_default_path(&project_cqs_dir);
                super::batch::evict_embeddings_cache_with_runtime(
                    &cache_path,
                    "watch reindex cycle",
                    Some(Arc::clone(&shared_rt)),
                );
                last_cache_evict = std::time::Instant::now();
            }

            // Release lock after all reindex work (including HNSW rebuild).
            drop(lock);

            // Pending sets drained — disarm the max-latency clock so
            // the next accepted event starts a fresh burst.
            state.first_pending_event = None;
        }
        // Publish freshness snapshot once per outer iteration.
        // Cheap — counter reads, one optional `metadata()` on `index.db`,
        // and a brief write-lock acquire. Runs every ~100 ms cycle so the
        // daemon's `dispatch_status` always sees a snapshot less than one
        // tick old. The `RwLock` on `watch_snapshot_handle` is acquired
        // for the duration of a struct-move; readers (daemon clients)
        // never wait more than that.
        publish_watch_snapshot(
            &watch_snapshot_handle,
            &fresh_notifier_handle,
            &mut state,
            &index_path,
            &in_flight_clients_handle,
            &reconcile_signal_handle,
            &sibling_slots,
        );

        if check_interrupted() {
            println!("\nStopping watch...");
            break;
        }

        #[cfg(unix)]
        if is_shutdown_requested() {
            tracing::info!("SIGTERM received, draining watch loop");
            if !cli.quiet {
                println!("\nSIGTERM received, stopping watch...");
            }
            break;
        }
    }

    // Bounded join of the daemon socket thread. The thread already observes
    // `daemon_should_exit()` at the top of its accept
    // loop (Ctrl+C and SIGTERM both satisfy it), so in the common case
    // this returns within one poll cycle (~100ms). Enforce an outer
    // timeout so a wedged handler (e.g. waiting on a long embedder
    // inference) can't keep the process alive past ~5 s after the
    // user asked it to stop.
    #[cfg(unix)]
    if let Some(handle) = socket_thread.take() {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let poll = Duration::from_millis(50);
        let mut handle_opt = Some(handle);
        while std::time::Instant::now() < deadline {
            // Drain via `as_ref` view + `take` so a refactor adding an
            // intervening take site can't turn an `.unwrap()` into a
            // daemon-shutdown panic.
            let finished = handle_opt
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(false);
            if finished {
                if let Some(h) = handle_opt.take() {
                    if let Err(e) = h.join() {
                        tracing::warn!(?e, "Daemon socket thread panicked during shutdown");
                    } else {
                        tracing::info!("Daemon socket thread joined cleanly");
                    }
                }
                break;
            }
            if handle_opt.is_none() {
                break;
            }
            std::thread::sleep(poll);
        }
        if handle_opt.is_some() {
            // The warn fires whenever the deadline expires before
            // `is_finished()` returns true, so journal output reflects
            // reality (no silent detach masquerading as "joined cleanly").
            // The "joined cleanly" line above is reachable only from the
            // `is_finished` arm, which already calls `.join()`.
            tracing::warn!(
                deadline_secs = 5,
                "Daemon socket thread did not exit within shutdown window — detaching (BatchContext Drop may race with process exit; in-flight embedder inference is the usual culprit)"
            );
            // Intentionally drop `handle_opt` to detach when the 5 s budget
            // is exhausted.
        }
    }

    // Bounded join of the in-flight HNSW rebuild thread (if any). Otherwise
    // the rebuild thread is detached on daemon shutdown — a long rebuild
    // keeps writing to disk after the process is "done" and may race the next
    // startup. Signal shutdown FIRST: the thread checks the flag before
    // entering each sidecar save, so a build that finishes after the 30s
    // window skips its save instead of being killed mid-promote by process
    // exit (the dirty flag stays set; the next start rebuilds). The bounded
    // wait covers the common case of a near-finished rebuild completing in
    // <1s; a stalled one gets detached with a loud warning.
    if let Some(mut pending) = state.pending_rebuild.take() {
        pending
            .shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = pending.handle.take() {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let poll = Duration::from_millis(100);
            let mut handle_opt = Some(handle);
            while std::time::Instant::now() < deadline {
                // Same drain shape as the daemon socket-thread join above:
                // `as_ref` view + `take` so an intervening `take()` can't
                // turn this into a panic.
                let finished = handle_opt
                    .as_ref()
                    .map(|h| h.is_finished())
                    .unwrap_or(false);
                if finished {
                    if let Some(h) = handle_opt.take() {
                        if let Err(e) = h.join() {
                            tracing::warn!(
                                ?e,
                                "Background HNSW rebuild thread panicked during shutdown"
                            );
                        } else {
                            tracing::info!("Background HNSW rebuild thread joined cleanly");
                        }
                    }
                    break;
                }
                if handle_opt.is_none() {
                    break;
                }
                std::thread::sleep(poll);
            }
            if handle_opt.is_some() {
                tracing::warn!(
                    "Background HNSW rebuild thread did not finish within 30s shutdown window — detaching"
                );
                // Drop to detach; rebuild thread keeps running until the
                // process exits, but at least we logged it.
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod adversarial_socket_tests;
