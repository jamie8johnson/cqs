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
//! ### Background-rebuild peak (#1344 / RM-V1.33-4)
//!
//! When `spawn_hnsw_rebuild` fires, the rebuild thread opens a second
//! read-only `Store` handle to stream chunks for the new HNSW index. As of
//! this fix that handle uses [`Store::open_readonly`] (64 MiB mmap + 4 MiB
//! cache) instead of the prior [`Store::open_readonly_pooled`] (256 + 16) —
//! shaves ~200 MiB off the rebuild-window peak. The build_hnsw_index_owned
//! pipeline streams chunks via the batched embedding API, so the smaller
//! mmap doesn't hurt rebuild throughput. Rebuild-window peak now:
//!
//! ```text
//! main store     :  ~272 MiB (256 mmap + 16 cache)
//! rebuild store  :   ~68 MiB (64 mmap + 4 cache)        ← was 272 MiB
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
    PendingRebuild, MAX_PENDING_REBUILD_DELTA,
};
#[cfg(test)]
use rebuild::{RebuildOutcome, RebuildResult};

mod gc;
use gc::{prune_last_indexed_mtime, run_daemon_periodic_gc, run_daemon_startup_gc};
#[cfg(test)]
use gc::{LAST_INDEXED_PRUNE_AGE_SECS, LAST_INDEXED_PRUNE_SIZE_THRESHOLD_DEFAULT};

mod reconcile;
use reconcile::{reconcile_enabled, run_daemon_reconcile};

mod events;
use events::max_pending_files;
use events::{collect_events, process_file_changes, process_note_changes};

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
/// Does not include `Store` because it is re-opened each cycle (DS-9).
///
/// RM-V1.25-28: `embedder` now points at a shared `Arc<OnceLock<Arc<Embedder>>>`
/// that the daemon thread also holds. First side to populate it wins; the
/// other side's future lazy-init short-circuits to the same instance.
/// Eliminates the ~500 MB duplicate footprint that existed when the outer
/// watch loop and the daemon thread each owned independent OnceLocks.
struct WatchConfig<'a> {
    root: &'a Path,
    cqs_dir: &'a Path,
    notes_path: &'a Path,
    supported_ext: &'a HashSet<&'a str>,
    parser: &'a CqParser,
    embedder: &'a std::sync::OnceLock<std::sync::Arc<Embedder>>,
    quiet: bool,
    model_config: &'a ModelConfig,
    /// #1002: gitignore matcher for the project. `None` if
    /// `CQS_WATCH_RESPECT_GITIGNORE=0`, `--no-ignore` was passed, or the
    /// `.gitignore` file is missing/unreadable. Wrapped in `RwLock` so the
    /// watch loop can hot-swap it on `.gitignore` change without a restart.
    gitignore: &'a std::sync::RwLock<Option<ignore::gitignore::Gitignore>>,
    /// #1004: SPLADE encoder held resident in the daemon so incremental
    /// reindex cycles can encode sparse vectors for new/changed chunks.
    /// `None` when the SPLADE model is absent, fails to load, or
    /// `CQS_WATCH_INCREMENTAL_SPLADE=0`. `Mutex` serializes GPU access
    /// since the encoder holds a CUDA context.
    splade_encoder: Option<&'a std::sync::Mutex<cqs::splade::SpladeEncoder>>,
    /// #1129: project-scoped global embedding cache (per-project, shared
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
    last_indexed_mtime: HashMap<PathBuf, SystemTime>,
    hnsw_index: Option<HnswIndex>,
    incremental_count: usize,
    /// RM-V1.25-23: number of file events dropped this debounce cycle
    /// because pending_files was at cap. Logged once per cycle in
    /// process_file_changes, cleared after.
    dropped_this_cycle: usize,
    /// #1090: when a background HNSW rebuild is running, the watch loop
    /// queues new (chunk_id, embedding) pairs here so they can be replayed
    /// into the rebuilt Owned index before the swap. `None` while no
    /// rebuild is in flight.
    pending_rebuild: Option<PendingRebuild>,
    /// PF-V1.30.1-1: throttle the per-tick `fs::metadata(index_path)` call
    /// in [`publish_watch_snapshot`]. Snapshots fire every ~100 ms, and
    /// `last_synced_at` is whole-second resolution anyway — restating
    /// every tick burns a `stat()` syscall (and on WSL 9P, ~ms latency).
    /// Cache the most-recent reading and only re-stat after this throttle
    /// window elapses.
    last_metadata_check: std::time::Instant,
    /// PF-V1.30.1-1: cached `last_synced_at` value reused between
    /// throttled stat calls. `None` ⇒ index.db missing or mtime
    /// unreadable on the last stat.
    cached_last_synced_at: Option<i64>,
    /// DS-V1.30.1-D4 (#1232): slot name resolved at startup. Cloned
    /// into the watch snapshot every publish so `daemon_status`
    /// callers (specifically `cqs slot remove`) can know which slot
    /// the daemon is serving and refuse to unlink it. Owned String
    /// rather than borrow because `WatchState` outlives the per-tick
    /// `WatchSnapshotInput`.
    active_slot: String,
}

/// PF-V1.30.1-1: how often the watch loop re-stats `index.db` for the
/// `last_synced_at` field on `WatchSnapshot`. 10 s matches the whole-second
/// wire resolution; tighter than this would re-stat without giving
/// observers any new bits.
const LAST_SYNCED_REFRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// #1182: publish a fresh `WatchSnapshot` into the shared `Arc<RwLock<...>>`
/// the daemon thread reads through. Called once per outer watch-loop tick.
///
/// Pure assemble-and-write: pulls the relevant counters off `state`, reads
/// `index.db`'s mtime as a best-effort `last_synced_at`, computes the
/// state-machine value, and replaces the snapshot under a brief write lock.
/// The lock is held only for the move; readers never block on real work.
///
/// PF-V1.30.1-1: takes `&mut WatchState` so the `last_synced_at` stat call
/// can be throttled via the cache. Without throttling this fires every
/// ~100 ms tick, paying a syscall (ms-scale on WSL 9P) for whole-second
/// wire data — wasted budget.
fn publish_watch_snapshot(
    handle: &cqs::watch_status::SharedWatchSnapshot,
    state: &mut WatchState,
    index_path: &std::path::Path,
) {
    // PF-V1.30.1-1: only re-stat when the cache has expired. Snapshots
    // fire every ~100 ms but `last_synced_at` is whole-second resolution
    // — re-stating every tick burns a syscall for no observer-visible
    // change. The cache is invalidated after `LAST_SYNCED_REFRESH`.
    // RB-3: surface overflow as None (treated same as "missing mtime")
    // instead of silently wrapping past `i64::MAX`.
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
        .with_active_slot(&state.active_slot),
    );
    // Poison-recovery: another writer panicking shouldn't silently stop
    // freshness publishing. Recover and overwrite.
    //
    // bundle-watch-status-machine / OB-V1.30.1-3: capture the previous
    // state under the lock so we can emit a transition log line *after*
    // releasing it. Operators see a journal trail for Fresh↔Stale↔
    // Rebuilding flips; the loop wrote 100 ms updates with zero state
    // observability before this. `WatchSnapshot` derives `Clone` and
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
}

/// PB-3: Check if a path is under a WSL DrvFS automount root.
///
/// Default automount root is `/mnt/`, but users can customize it via `automount.root`
/// in `/etc/wsl.conf`. Reads the config once via `OnceLock` and caches the result.
fn is_under_wsl_automount(path: &str) -> bool {
    static AUTOMOUNT_ROOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let root = AUTOMOUNT_ROOT
        .get_or_init(|| parse_wsl_automount_root().unwrap_or_else(|| "/mnt/".to_string()));
    path.starts_with(root.as_str())
}

/// Parse the `automount.root` value from `/etc/wsl.conf`.
/// Returns `None` if the file doesn't exist or doesn't contain the setting.
fn parse_wsl_automount_root() -> Option<String> {
    // RM-V1.33-7: bound the read at 64 KiB. `/etc/wsl.conf` is normally
    // a few hundred bytes; a hostile symlink or bind mount pointing at a
    // multi-GB file would otherwise OOM the watch loop on first event.
    use std::io::Read;
    const MAX_WSL_CONF_BYTES: u64 = 64 * 1024;
    let mut content = String::new();
    std::fs::File::open("/etc/wsl.conf")
        .ok()?
        .take(MAX_WSL_CONF_BYTES)
        .read_to_string(&mut content)
        .ok()?;
    let mut in_automount = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_automount = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("automount");
            continue;
        }
        if in_automount {
            if let Some((key, value)) = trimmed.split_once('=') {
                if key.trim().eq_ignore_ascii_case("root") {
                    let mut root = value.trim().to_string();
                    // Ensure trailing slash for prefix matching
                    if !root.ends_with('/') {
                        root.push('/');
                    }
                    return Some(root);
                }
            }
        }
    }
    None
}

/// #1002: Build a `Gitignore` matcher rooted at the project, combining the
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

    // Root-only .gitignore / .cqsignore in v1. Nested ignore files are not
    // yet discovered — tracked as follow-up. `cqs index` uses the full
    // `ignore` crate walk which supports nesting; the watch loop uses a
    // per-event point query against a pre-built matcher and compile-time
    // nesting would require rebuilding on every subdir change. Root-level
    // covers the worktree-pollution + vendor-bundle motivating cases.

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
/// * `debounce_ms` - Debounce interval in milliseconds for file change events
/// * `no_ignore` - If true, skips `.gitignore` filtering in the watch loop (#1002).
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

    // RM-V1.25-9: install SIGTERM handler *before* spawning the socket
    // thread so both the main loop and the accept loop observe the
    // shutdown flag immediately when systemd stops the unit.
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
    // PB-21: Also detect //wsl.localhost/ and //wsl$/ UNC paths
    // PB-3: Check /etc/wsl.conf for custom automount.root (default is /mnt/)
    let use_poll = poll
        || (cqs::config::is_wsl()
            && root
                .to_str()
                .is_some_and(|p| p.starts_with("//wsl") || is_under_wsl_automount(p)));

    if cqs::config::is_wsl() && !use_poll {
        tracing::warn!("WSL detected: inotify may be unreliable on Windows filesystem mounts. Use --poll or 'cqs index' periodically.");
    }

    // SHL-V1.25-13: the 500ms default is tuned for inotify on native
    // Linux. WSL DrvFS (/mnt/, //wsl$) exposes NTFS which has 1s mtime
    // resolution — anything under ~1000ms risks double-fire for a single
    // save. Poll mode also benefits from a longer window. When the user
    // did not override via flag or env, auto-bump to 1500ms for these
    // paths. `CQS_WATCH_DEBOUNCE_MS` takes precedence over the flag.
    let debounce_ms = if let Some(env_ms) = std::env::var("CQS_WATCH_DEBOUNCE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        env_ms
    } else if debounce_ms == 500 && use_poll {
        tracing::info!(
            "Auto-bumping watch debounce to 1500ms for WSL/poll mode (override via --debounce or CQS_WATCH_DEBOUNCE_MS)"
        );
        1500
    } else {
        debounce_ms
    };

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
            match std::os::unix::net::UnixStream::connect(&sock_path) {
                Ok(_) => {
                    anyhow::bail!(
                        "Another daemon is already listening on {}",
                        sock_path.display()
                    );
                }
                Err(_) => {
                    // SEC-V1.25-15 / PB-V1.25-19: don't blindly unlink whatever
                    // is at sock_path — an attacker (or a stale test artifact)
                    // could leave a symlink or regular file there and trick us
                    // into deleting something we shouldn't. Use symlink_metadata
                    // (no follow) and refuse to remove anything that isn't a
                    // socket or a plain file in the cqs dir.
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
        // SEC-D.6: between `bind()` (creates socket honoring umask) and
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
        // OB-NEW-2 / SEC-V1.30.1-8: Self-maintaining env snapshot —
        // iterate every CQS_* variable instead of a hardcoded whitelist
        // that drifts as new knobs are added. Env vars set on client
        // subprocesses do NOT affect daemon-served queries; only the
        // daemon's own env applies.
        //
        // Redact secrets — any var whose name suffix matches a known
        // secret marker is logged with `<redacted len=N>` instead of
        // the value. With OB-V1.30-1 surfacing info-level to journald,
        // an unredacted log lands in a 30-day journal artifact.
        const SECRET_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_PASSWORD", "_SECRET"];
        let cqs_vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("CQS_"))
            .map(|(k, v)| {
                let is_secret = SECRET_SUFFIXES.iter().any(|suffix| k.ends_with(suffix));
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
    // PB-V1.25-2 / PB-V1.25-18: on non-unix platforms the daemon
    // socket path is #[cfg(unix)]-only, so --serve would otherwise
    // silently no-op. Warn both on stderr (so interactive users notice
    // without --log-level=warn) and via tracing (for systemd-style
    // journals that scrape our output).
    #[cfg(not(unix))]
    if serve {
        eprintln!(
            "Warning: --serve is unix-only (daemon socket uses Unix domain sockets); \
             falling back to plain watch mode"
        );
        tracing::warn!("--serve requested on non-unix platform; daemon disabled");
    }

    // RM-V1.25-28: Allocate the shared embedder slot before spawning the
    // daemon thread so the Arc can be cloned into the thread's closure
    // and adopted by its BatchContext. The slot starts empty; whichever
    // side initializes first (daemon via `ctx.warm()` or watch via
    // `try_init_embedder`) wins and the other reuses the same Arc.
    let shared_embedder: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<Embedder>>> =
        std::sync::Arc::new(std::sync::OnceLock::new());

    // #968: Build ONE tokio runtime and share it across the outer Store
    // (read-write, for reindex writes) and the daemon thread's inner
    // Store (read-only, for queries) plus its EmbeddingCache/QueryCache.
    // Without this each constructor spawned its own 1-4 worker threads
    // that never overlapped usefully. `shared_rt` must be declared before
    // the daemon thread spawn below so we can `Arc::clone` into the
    // closure; it stays alive until this function returns, after the
    // daemon thread is joined.
    let shared_rt = build_shared_runtime()
        .with_context(|| "Failed to build shared tokio runtime for daemon")?;

    // #1182: shared `Arc<RwLock<WatchSnapshot>>` for cross-thread freshness
    // publishing. Allocated *before* the daemon spawn so the Arc can be cloned
    // into both the watch loop (writer) and the daemon thread's BatchContext
    // (reader via `dispatch_status`). Initial value is the `unknown` snapshot;
    // the watch loop overwrites it once per cycle below.
    let watch_snapshot_handle: cqs::watch_status::SharedWatchSnapshot =
        cqs::watch_status::shared_unknown();

    // #1182 — Layer 1: cross-thread one-shot reconcile signal. Allocated
    // alongside `watch_snapshot_handle` so a single Arc clone reaches each
    // side. Watch loop swaps it back to `false` after running the on-demand
    // reconcile pass; daemon's `dispatch_reconcile` flips it to `true`.
    let reconcile_signal_handle: cqs::watch_status::SharedReconcileSignal =
        cqs::watch_status::shared_reconcile_signal();

    // #1182 — Layer 1: pick up a leftover `.cqs/.dirty` marker from a
    // previous session where a git hook fired without a daemon listening.
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
    // RM-V1.25-8: keep the `JoinHandle` in a named `socket_thread` so the
    // main loop can `.take().join()` it on shutdown with a bounded wait.
    // Previously the handle was stashed under `_socket_thread` and dropped
    // on function exit, detaching the thread. In that window the daemon's
    // BatchContext (~500MB+ ONNX sessions, SQLite pool, HNSW Arc, optional
    // CAGRA GPU resources) lived past the main loop's return with no
    // WAL checkpoint and no `Drop` ordering. Under `cargo install` or shell
    // Ctrl+C the orphaned thread could also block stdout writes.
    #[cfg(unix)]
    let mut socket_thread: Option<std::thread::JoinHandle<()>> = if serve {
        if let Some((listener, _)) = socket_listener.take() {
            // RM-V1.25-28: Clone the shared OnceLock into the daemon closure
            // so both the outer watch loop and BatchContext see the same
            // Arc<Embedder>.
            let daemon_embedder = std::sync::Arc::clone(&shared_embedder);
            // Index-aware model resolution for the daemon's embedder. Prefer
            // the model recorded in the store metadata so a wrong-model
            // CQS_EMBEDDING_MODEL doesn't silently produce zero-result queries
            // (the dim mismatch otherwise only surfaces as a tracing::warn!).
            // See ROADMAP.md "Embedder swap workflow" for the longer story.
            let daemon_model_config =
                resolve_index_aware_model_for_watch(&index_path, &root, cli.model.as_deref())?;
            // #968: Clone the shared runtime handle into the daemon closure so
            // its BatchContext opens its Store/EmbeddingCache/QueryCache on
            // the same multi-thread pool as the outer watch loop.
            let daemon_runtime = Arc::clone(&shared_rt);
            // #1182: clone the shared snapshot Arc into the daemon thread.
            let daemon_watch_snapshot = Arc::clone(&watch_snapshot_handle);
            // #1182 — Layer 1: clone the reconcile-signal Arc too.
            let daemon_reconcile_signal = Arc::clone(&reconcile_signal_handle);
            // Stays non-blocking: the accept loop below polls so it can
            // notice SHUTDOWN_REQUESTED on SIGTERM (RM-V1.25-9).
            let thread = daemon::spawn_daemon_thread(
                listener,
                daemon_embedder,
                daemon_model_config,
                daemon_runtime,
                daemon_watch_snapshot,
                daemon_reconcile_signal,
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

    // v1.22.0 audit DS-W2 / OB-22 / PB-NEW-6: watch does not run SPLADE
    // encoding on new chunks. The v20 trigger on `chunks` DELETE ensures
    // sparse correctness (the persisted splade.index.bin gets invalidated
    // when chunks are removed), but newly-added chunks have no sparse
    // vectors until a manual `cqs index` runs. If a user has
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

    // #1091: poll interval is separate from debounce. PollWatcher walks the
    // entire tree on every tick — on WSL DrvFS each entry is a 9P round-trip,
    // so 1500ms (the prior debounce-derived default) burns ~8% of one core
    // continuously on a ~16k-file tree. Default to 5000ms (still fast enough
    // for save → reindex), override with `CQS_WATCH_POLL_MS`. Inotify watchers
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

    // P2.74: warn when the project tree approaches the inotify watch limit.
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

    let debounce = Duration::from_millis(debounce_ms);
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
    // OnceLock can be shared with the daemon thread — see RM-V1.25-28.

    // Open store and reuse across reindex operations within a cycle.
    // Re-opened after each reindex cycle to clear stale OnceLock caches (DS-9).
    // #968: `shared_rt` is declared above the daemon-thread spawn so the
    // closure can `Arc::clone` it; the outer store shares that runtime
    // here so the daemon's inner read-only store and its caches all run
    // on one multi-thread pool instead of three isolated runtimes.
    let mut store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    // v24 / #1221: resolve the vendored-path prefix list once at daemon
    // startup so all subsequent inserts (incremental + reconcile-driven)
    // get correct `vendored` flags. Reads `[index].vendored_paths` from
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

    // DS-W5: Track the database file identity so we detect when `cqs index --force`
    // replaces it. Without this check, watch's Store handle would point at the
    // orphaned (renamed) inode and writes would silently vanish.
    let mut db_id = db_file_identity(&index_path);

    // Persistent HNSW state for incremental updates.
    //
    // The watch loop keeps an *Owned* HnswIndex in memory so `insert_batch`
    // (line ~2480 below) can append new chunks without rebuilding the graph
    // from scratch. After every `hnsw_rebuild_threshold()` incremental inserts
    // we trigger a full rebuild to clean orphan vectors (hnsw_rs has no
    // delete; updated chunks leave their old vectors behind).
    //
    // #1090: at startup we load the persisted index from disk for instant
    // search availability, and *immediately* spawn a background rebuild so
    // we end up with an Owned variant ready before the first file save —
    // without paying a 10-15s cold-start hit. The Loaded variant cannot be
    // mutated (hnsw_rs constraint), so without this swap the first save
    // after restart would fail incremental insert and force a synchronous
    // full rebuild, blocking the editor for 15s. Spawning the rebuild
    // off-thread keeps the daemon responsive throughout.
    //
    // DS-35: starting `incremental_count` at threshold/2 (when we loaded an
    // existing index) means stale orphans from prior sessions get cleaned
    // sooner; the cleanup is now async too via the same pending_rebuild path.
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
                // v1.22.0 audit EH-7: previously `Err(_) => (None, 0)` treated
                // DimensionMismatch, IO errors, and corruption the same as
                // "first run." Now logs so the operator sees why the prior
                // index was discarded.
                tracing::warn!(error = %e, "Existing HNSW index unusable, rebuilding from scratch");
                (None, 0, None)
            }
        };

    // Index-aware model resolution: prefer the model recorded in the open
    // store metadata over CLI flag / env / config / default. Without this,
    // running `cqs watch` with `CQS_EMBEDDING_MODEL=wrong-model` would embed
    // new chunks with a different dim than the index, corrupting
    // incremental reindex.
    let stored_model_for_watch = store.stored_model_name();
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

    // #1002: build the gitignore matcher once at startup. `no_ignore` (CLI)
    // and `CQS_WATCH_RESPECT_GITIGNORE=0` (env) both disable it. Held in
    // `RwLock<Option<_>>` so a future follow-up can hot-swap on
    // `.gitignore` change without restart; v1 builds it once.
    let gitignore = std::sync::RwLock::new(if no_ignore {
        tracing::info!("--no-ignore passed — gitignore filtering disabled");
        None
    } else {
        build_gitignore_matcher(&root)
    });

    // #1024: Daemon startup GC. Two-pass sweep — drop chunks whose origin
    // is gone from disk (Pass 1) and drop chunks whose path is now matched
    // by `.gitignore` (Pass 2, retroactive cleanup of pre-v1.26.0 worktree
    // pollution). Only runs in `--serve` mode (the systemd unit) and is
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
                // EH-V1.29-8: Recover from RwLock poison. A poisoned read
                // usually means a writer panicked mid-update; the previously-
                // written matcher is still valid data. Dropping to "no
                // matcher" silently re-indexes ignored files (including
                // `.env.secret`). `into_inner()` on the `PoisonError` keeps
                // the matcher visible.
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

    // #1004: build the SPLADE encoder once at startup. `None` means
    // incremental SPLADE is disabled for this daemon lifetime — either
    // the model isn't configured, failed to load, or the operator set
    // `CQS_WATCH_INCREMENTAL_SPLADE=0`. Existing sparse vectors in the
    // DB are preserved in all cases.
    let splade_encoder_storage = build_splade_encoder_for_watch().map(std::sync::Mutex::new);
    let splade_encoder_ref: Option<&std::sync::Mutex<cqs::splade::SpladeEncoder>> =
        splade_encoder_storage.as_ref();

    // #1129: open the project-scoped global embedding cache once at daemon
    // startup so reindex cycles can hit it without paying open() per cycle.
    // Mirrors the bulk pipeline's gating on `CQS_CACHE_ENABLED=0`. Open
    // failure is best-effort: log and continue with `None`, identical to
    // the bulk path's degradation.
    //
    // Reuse `shared_rt` so this Cache piggybacks on the same worker pool
    // as the outer Store, daemon Store/Cache, etc. (#968).
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
        // Track last-indexed mtime per file to skip duplicate WSL/NTFS events.
        // On WSL, inotify over 9P delivers repeated events for the same file change.
        // Bounded: pruned when >10k entries or >1k entries on single-file reindex.
        last_indexed_mtime: HashMap::with_capacity(1024),
        hnsw_index,
        incremental_count,
        dropped_this_cycle: 0,
        pending_rebuild,
        // PF-V1.30.1-1: seed throttle so the very first publish tick does
        // re-stat `index.db` (the cache starts empty). After that the
        // cadence is `LAST_SYNCED_REFRESH` between calls. `checked_sub`
        // guards against the (theoretical) freshly-booted-machine case
        // where `Instant::now()` is < `LAST_SYNCED_REFRESH` since boot —
        // falling back to `Instant::now()` just means the *second* tick
        // is the one that reads, not the first.
        last_metadata_check: std::time::Instant::now()
            .checked_sub(LAST_SYNCED_REFRESH)
            .unwrap_or_else(std::time::Instant::now),
        cached_last_synced_at: None,
        active_slot: active_slot.name.clone(),
    };

    let mut cycles_since_clear: u32 = 0;
    // RM-V1.25-5: Track last eviction of the global embedding cache so
    // the reindex path only trims once per hour, keeping the WAL file
    // from churning on every micro-edit.
    let mut last_cache_evict = std::time::Instant::now();

    // #1024: Track last periodic GC tick. Initialised to "now" so the
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

    // #1182 Layer 2: periodic full-tree reconciliation cadence. Same
    // gating model as the GC tick — `--serve` only, opt-out via
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
                // P3-4 (audit v1.33.0): include `kind` and `paths` so
                // operators can distinguish MaxFilesWatch (raise sysctl) from
                // Io(broken pipe) (restart daemon). Display alone collapses
                // the discriminator and drops paths.
                warn!(
                    error = %e,
                    kind = ?e.kind,
                    paths = ?e.paths,
                    "Watch error"
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // #1182 — Layer 1: out-of-band reconcile request. The
                // daemon's `dispatch_reconcile` flips this AtomicBool to
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
                    if queued > 0 {
                        // Reset `last_event` so the synthetic pending
                        // entries are picked up by the very next
                        // `should_process` tick (otherwise the debounce
                        // window would still hold for `last_event`'s
                        // worth of milliseconds).
                        state.last_event = std::time::Instant::now();
                    }
                    // Sliding the periodic-tick clock keeps the two
                    // mechanisms from racing: the next periodic walk
                    // waits a full `daemon_reconcile_interval_secs()`
                    // after this on-demand walk.
                    last_reconcile = std::time::Instant::now();
                }

                let should_process = (!state.pending_files.is_empty() || state.pending_notes)
                    && state.last_event.elapsed() >= debounce;

                if should_process {
                    cycles_since_clear = 0;

                    // DS-1: Acquire index lock before reindexing. If another process
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

                    // DS-W5: Detect if `cqs index --force` replaced the database
                    // while we were waiting. If so, reopen the Store before processing
                    // any changes — otherwise writes go to the orphaned inode.
                    let current_id = db_file_identity(&index_path);
                    if current_id != db_id {
                        info!("index.db replaced (likely cqs index --force), reopening store");
                        drop(store);
                        // #968: reuse the shared runtime on re-open so the
                        // replacement store keeps running on the same
                        // multi-thread worker pool as its predecessor.
                        store = Store::open_with_runtime(&index_path, Arc::clone(&shared_rt))
                            .with_context(|| {
                                format!(
                                    "Failed to re-open store at {} after DB replacement",
                                    index_path.display()
                                )
                            })?;
                        // v24 / #1221: re-stamp the vendored prefix list on
                        // the fresh Store — the OnceLock is per-instance and
                        // a brand-new Store starts empty. Use the cached
                        // resolution from startup so we don't re-read
                        // .cqs.toml mid-watch.
                        store.set_vendored_prefixes(vendored_prefixes_for_store.clone());
                        // db_id updated below in the DS-9 reopen path
                        state.hnsw_index = None;
                        state.incremental_count = 0;
                        // DS-V1.30.1-D1: drop in-flight rebuild whose pending
                        // delta references OLD DB chunk IDs. The rebuild
                        // thread will tx.send(...) into a dropped receiver
                        // (no-op per rebuild.rs:289). Force a fresh rebuild
                        // on the next threshold tick against the new DB.
                        if state.pending_rebuild.take().is_some() {
                            tracing::info!(
                                "discarded in-flight HNSW rebuild after DB replacement; \
                                 next threshold tick will rebuild against new DB",
                            );
                        }
                    }

                    if !state.pending_files.is_empty() {
                        process_file_changes(&watch_cfg, &store, &mut state);
                    }

                    if state.pending_notes {
                        state.pending_notes = false;
                        process_note_changes(&root, &store, cli.quiet);
                    }

                    // DS-9: Re-open Store to clear stale OnceLock caches
                    // (call_graph_cache, test_chunks_cache). The documented contract
                    // in store/mod.rs requires re-opening after index changes.
                    // DS-9 / RM-6: Clear caches instead of full re-open.
                    // Avoids pool teardown + runtime creation + PRAGMA setup
                    // on every reindex cycle over 24/7 systemd lifetime.
                    store.clear_caches();
                    db_id = db_file_identity(&index_path);

                    // RM-V1.25-5: Periodically evict the global embedding
                    // cache so long-running watch sessions don't let the
                    // shared ~/.cache/cqs/embeddings.db grow past its
                    // CQS_CACHE_MAX_SIZE cap (default 10GB). Gated by
                    // `last_cache_evict.elapsed()` so we don't churn the
                    // SQLite file on every single reindex cycle.
                    //
                    // #968: reuse the shared runtime so this one-shot eviction
                    // piggybacks on the existing worker pool rather than
                    // spinning up a fresh current_thread runtime.
                    if last_cache_evict.elapsed() >= Duration::from_secs(3600) {
                        let project_cqs_dir = cqs::resolve_index_dir(&root);
                        let cache_path =
                            cqs::cache::EmbeddingCache::project_default_path(&project_cqs_dir);
                        super::batch::evict_embeddings_cache_with_runtime(
                            &cache_path,
                            "watch reindex cycle",
                            Some(Arc::clone(&shared_rt)),
                        );
                        last_cache_evict = std::time::Instant::now();
                    }

                    // DS-1: Release lock after all reindex work (including HNSW rebuild)
                    drop(lock);
                } else {
                    cycles_since_clear += 1;
                    // Clear embedder session and HNSW index after ~5 minutes idle
                    // (3000 cycles at 100ms). Frees GPU/memory when watch is idle.
                    //
                    // RM-V1.25-28: the shared Arc<Embedder> is also held by
                    // the daemon thread's BatchContext. clear_session is
                    // safe either way: the ONNX session is behind a Mutex
                    // and the tokenizer is Mutex<Option<Arc<…>>>.
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = shared_embedder.get() {
                            emb.clear_session();
                        }
                        // AC-V1.30.1-10: do NOT reset incremental_count on
                        // idle-clear. The counter's contract is "incremental
                        // inserts since last full rebuild"; a 5-minute idle
                        // hasn't changed the on-disk delta. Resetting here
                        // means the next file event starts the threshold
                        // timer from scratch and understates delta size,
                        // delaying the rebuild that should fire on
                        // accumulated drift.
                        state.hnsw_index = None;
                        cycles_since_clear = 0;
                    }

                    // #1024: Idle-time periodic GC. Only fires when
                    //   (a) `--serve` is on AND `CQS_DAEMON_PERIODIC_GC` != "0",
                    //   (b) the last actual file event was more than
                    //       `daemon_periodic_gc_idle_secs()` ago (so a long
                    //       burst of edits never triggers GC mid-burst), AND
                    //   (c) the previous tick was more than
                    //       `daemon_periodic_gc_interval_secs()` ago.
                    // The bounded sweep (cap = daemon_periodic_gc_cap()) keeps
                    // each tick's write transaction short.
                    //
                    // PF-V1.30.1-3 (#1226): when both gc and reconcile gates
                    // fire on the same idle tick, do one disk walk and pass
                    // it to both consumers. The two intervals share the
                    // idle gate (`daemon_periodic_gc_idle_secs`), so on
                    // long-quiet ticks at the gc cadence boundary, both
                    // would otherwise walk the tree back-to-back.
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
                                // EH-V1.29-8: Same poison-recovery as startup
                                // GC above — silently dropping to "no matcher"
                                // would let periodic GC re-index gitignored
                                // files (the very ones the matcher was built
                                // to exclude).
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

                    // #1182 Layer 2: Periodic full-tree reconciliation.
                    // Sibling of the GC tick; same idle-gating model:
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
                        // #1231: detect a `cqs index --force` rotation that
                        // happened between idle ticks — the inotify branch
                        // at line 1191 only fires on actual filesystem
                        // events, and a long quiet period followed by a
                        // forced reindex would land us here with `store`
                        // pointing at the orphaned inode. Reopen on
                        // mismatch and skip this tick; the next interval
                        // will reconcile against the fresh DB.
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
                            // v24 / #1221: re-stamp vendored prefixes on
                            // the fresh Store — OnceLock is per-instance.
                            store.set_vendored_prefixes(vendored_prefixes_for_store.clone());
                            db_id = current_id;
                            state.hnsw_index = None;
                            state.incremental_count = 0;
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
                            if queued > 0 {
                                // Reset `last_event` so `process_file_changes`
                                // observes the synthetic pending entries on
                                // the very next debounce tick (otherwise the
                                // idle threshold would still hold).
                                state.last_event = std::time::Instant::now();
                            }
                            last_reconcile = std::time::Instant::now();
                        }
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

        // #1182: publish freshness snapshot once per outer iteration.
        // Cheap — counter reads, one optional `metadata()` on `index.db`,
        // and a brief write-lock acquire. Runs every ~100 ms cycle so the
        // daemon's `dispatch_status` always sees a snapshot less than one
        // tick old. The `RwLock` on `watch_snapshot_handle` is acquired
        // for the duration of a struct-move; readers (daemon clients)
        // never wait more than that.
        publish_watch_snapshot(&watch_snapshot_handle, &mut state, &index_path);

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

    // RM-V1.25-8: bounded join of the daemon socket thread. The thread
    // already observes `daemon_should_exit()` at the top of its accept
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
            match handle_opt.as_ref() {
                Some(h) if h.is_finished() => {
                    if let Err(e) = handle_opt.take().unwrap().join() {
                        tracing::warn!(?e, "Daemon socket thread panicked during shutdown");
                    } else {
                        tracing::info!("Daemon socket thread joined cleanly");
                    }
                    break;
                }
                Some(_) => std::thread::sleep(poll),
                None => break,
            }
        }
        if handle_opt.is_some() {
            // P3.22: log audit verified — the warn fires whenever the deadline
            // expires before `is_finished()` returns true, so journal output
            // reflects reality (no silent detach masquerading as "joined
            // cleanly"). The "joined cleanly" line above is reachable only
            // from the `is_finished` arm, which already calls `.join()`.
            tracing::warn!(
                deadline_secs = 5,
                "Daemon socket thread did not exit within shutdown window — detaching (BatchContext Drop may race with process exit; in-flight embedder inference is the usual culprit)"
            );
            // Intentionally drop `handle_opt` to detach — preserved as the
            // pre-fix behaviour, only when the 5 s budget is exhausted.
        }
    }

    // P2.71: bounded join of the in-flight HNSW rebuild thread (if any).
    // Without this, the rebuild thread is detached on daemon shutdown — a
    // long rebuild keeps writing to disk after the process is "done" and may
    // race the next startup. The rebuild thread doesn't observe a shutdown
    // flag yet (audit calls cancellation a follow-on issue), so we bound the
    // wait at 30s — the common case is a near-finished rebuild that completes
    // in <1s, and a stalled rebuild gets detached with a loud warning.
    if let Some(mut pending) = state.pending_rebuild.take() {
        if let Some(handle) = pending.handle.take() {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let poll = Duration::from_millis(100);
            let mut handle_opt = Some(handle);
            while std::time::Instant::now() < deadline {
                match handle_opt.as_ref() {
                    Some(h) if h.is_finished() => {
                        if let Err(e) = handle_opt.take().unwrap().join() {
                            tracing::warn!(
                                ?e,
                                "Background HNSW rebuild thread panicked during shutdown"
                            );
                        } else {
                            tracing::info!("Background HNSW rebuild thread joined cleanly");
                        }
                        break;
                    }
                    Some(_) => std::thread::sleep(poll),
                    None => break,
                }
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
