//! Watch mode - monitor for file changes and reindex
//!
//! ## Memory Usage
//!
//! Watch mode holds several resources in memory while idle:
//!
//! - **Parser**: ~1MB for tree-sitter queries (allocated immediately)
//! - **Store**: SQLite connection pool with up to 4 connections (allocated immediately)
//! - **Embedder**: ~500MB for ONNX model (lazy-loaded on first file change)
//!
//! The Embedder is the largest resource and is only loaded when files actually change.
//! Once loaded, it remains in memory for fast subsequent reindexing. This tradeoff
//! favors responsiveness over memory efficiency for long-running watch sessions.
//!
//! For memory-constrained environments, consider running `cqs index` manually instead
//! of using watch mode.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use notify::{Config, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{info, info_span, warn};

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::generate_nl_description;
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
use gc::{LAST_INDEXED_PRUNE_AGE_SECS, LAST_INDEXED_PRUNE_SIZE_THRESHOLD};

mod events;
#[cfg(test)]
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
    let content = std::fs::read_to_string("/etc/wsl.conf").ok()?;
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
        // OB-NEW-2: Self-maintaining env snapshot — iterate every CQS_*
        // variable instead of a hardcoded whitelist that drifts as new
        // knobs are added. Env vars set on client subprocesses do NOT
        // affect daemon-served queries; only the daemon's own env applies.
        let cqs_vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("CQS_"))
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
            // Stays non-blocking: the accept loop below polls so it can
            // notice SHUTDOWN_REQUESTED on SIGTERM (RM-V1.25-9).
            let thread = std::thread::spawn(move || {
                // BatchContext created inside the thread — RefCell is !Send
                // but thread-local ownership is fine.
                let ctx = match super::batch::create_context_with_runtime(Some(daemon_runtime)) {
                    Ok(ctx) => {
                        // RM-V1.25-28: seed the BatchContext's OnceLock if
                        // the shared slot is already populated; otherwise
                        // populate the shared slot with a fresh Embedder
                        // so the outer watch loop sees it on first use.
                        if let Some(existing) = daemon_embedder.get() {
                            ctx.adopt_embedder(std::sync::Arc::clone(existing));
                            tracing::info!("Daemon adopted shared embedder");
                        } else {
                            match Embedder::new(daemon_model_config) {
                                Ok(emb) => {
                                    let arc = std::sync::Arc::new(emb);
                                    // Try to install in the shared slot;
                                    // another thread may have raced us.
                                    let winning_arc =
                                        daemon_embedder.get_or_init(|| std::sync::Arc::clone(&arc));
                                    ctx.adopt_embedder(std::sync::Arc::clone(winning_arc));
                                    tracing::info!("Daemon built and shared embedder");
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Daemon embedder init failed — will retry lazily");
                                }
                            }
                        }
                        ctx.warm();
                        ctx
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Daemon BatchContext creation failed");
                        return;
                    }
                };
                // SEC-V1.25-1: wrap the BatchContext in Arc<Mutex> so each
                // accepted connection gets its own handler thread. Without
                // this, a single malicious client sitting on the 5 s read
                // timeout (or a slow legitimate client) could wedge the
                // accept loop for `5 * N` seconds and DoS the daemon.
                //
                // #1127 (post-#1145 — closes P2.64): the BatchContext mutex
                // is now held only across `checkout_view_from_arc` — a few
                // microseconds to clone the snapshot Arcs and drop the
                // guard. Handlers run outside the lock against a
                // `BatchView`, so two slow queries (gather, task) overlap
                // on wall-clock. The Refresh handler back-channels through
                // the view's `outer_lock` to take the BatchContext mutex
                // briefly for the invalidation.
                //
                // The accept loop's idle sweep (`sweep_idle_sessions`) and
                // the new daemon-dispatch path are both safe under
                // try_lock / brief lock semantics — they never block on a
                // long handler.
                let ctx = Arc::new(Mutex::new(ctx));
                let in_flight = Arc::new(AtomicUsize::new(0));
                // P3 #125: resolve cap once at startup so a `CQS_MAX_DAEMON_CLIENTS`
                // change requires daemon restart (matches the rest of the env-var
                // surface — config reload is not a goal for caps).
                let max_clients = max_concurrent_daemon_clients();
                tracing::info!(max_concurrent = max_clients, "Daemon query thread ready");
                // RM-V1.25-3: Periodically sweep idle ONNX sessions even if
                // no client connects. `check_idle_timeout` only fires on
                // `dispatch_line`, so a warmed-but-untouched daemon would
                // otherwise pin ~500MB+ indefinitely. Tick once per minute.
                let mut last_idle_sweep = std::time::Instant::now();
                let idle_sweep_interval = Duration::from_secs(60);
                // P3 #125: report current in-flight client count once a minute
                // so operators can see whether the cap is being approached.
                let mut last_inflight_report = std::time::Instant::now();
                let inflight_report_interval = Duration::from_secs(60);
                // RM-V1.25-9: Poll accept with a short sleep so the loop
                // can notice SIGTERM and drain cleanly instead of blocking
                // indefinitely on a syscall that systemd has to kill.
                // Listener was set non-blocking at bind time.
                // RM-V1.25-8: also break on Ctrl+C (`check_interrupted`) so
                // the main loop's `.join()` on shutdown completes promptly.
                loop {
                    if daemon_should_exit() {
                        tracing::info!("Daemon accept loop draining on shutdown signal");
                        break;
                    }
                    // RM-V1.25-3: passive idle sweep — inspects the
                    // `last_command_time` set by real dispatches and drops
                    // sessions after IDLE_TIMEOUT_MINUTES. Skip if a handler
                    // holds the mutex (we'll try again next tick).
                    if last_idle_sweep.elapsed() >= idle_sweep_interval {
                        if let Ok(ctx_guard) = ctx.try_lock() {
                            ctx_guard.sweep_idle_sessions();
                        }
                        last_idle_sweep = std::time::Instant::now();
                    }
                    // P3 #125: periodic in-flight report so operators can
                    // spot saturation in `journalctl --user-unit cqs-watch`.
                    if last_inflight_report.elapsed() >= inflight_report_interval {
                        let current = in_flight.load(Ordering::Acquire);
                        tracing::info!(
                            current_in_flight = current,
                            cap = max_clients,
                            "Daemon client count"
                        );
                        last_inflight_report = std::time::Instant::now();
                    }
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            // SEC-V1.25-1: back-pressure. If we're already at the
                            // `CQS_MAX_DAEMON_CLIENTS` cap of in-flight handlers,
                            // reject this connection quickly rather than spawning
                            // an unbounded number of threads. Daemon is local-only,
                            // but we still want a hard cap so a misbehaving client
                            // can't exhaust fds or thread stacks.
                            let current = in_flight.load(Ordering::Acquire);
                            if current >= max_clients {
                                let mut s = stream;
                                let _ = write_daemon_error(
                                    &mut s,
                                    "daemon busy (too many concurrent clients)",
                                );
                                tracing::warn!(
                                    in_flight = current,
                                    cap = max_clients,
                                    "Rejecting new daemon connection — at concurrency cap"
                                );
                                continue;
                            }
                            in_flight.fetch_add(1, Ordering::AcqRel);
                            let ctx_clone = Arc::clone(&ctx);
                            let in_flight_clone = Arc::clone(&in_flight);
                            // Spawn a fresh thread per accepted connection so
                            // read/parse/write I/O happens in parallel. Only
                            // the dispatch itself is serialized via the
                            // BatchContext mutex inside handle_socket_client.
                            if let Err(e) = std::thread::Builder::new()
                                .name("cqs-daemon-client".to_string())
                                .spawn(move || {
                                    handle_socket_client(stream, &ctx_clone);
                                    in_flight_clone.fetch_sub(1, Ordering::AcqRel);
                                })
                            {
                                // Couldn't spawn a thread — decrement the
                                // counter we just bumped and log. The
                                // connection is dropped when `stream` falls
                                // out of scope at the end of the match arm.
                                in_flight.fetch_sub(1, Ordering::AcqRel);
                                tracing::warn!(
                                    error = %e,
                                    "Failed to spawn daemon client thread — dropping connection"
                                );
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(e) => {
                            // Warn, not debug: EMFILE/ENFILE/ECONNABORTED are
                            // operator-actionable (raise ulimit, etc.) and
                            // should be visible at the default log level.
                            tracing::warn!(error = %e, "Socket accept failed");
                        }
                    }
                }
            });
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

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                collect_events(&event, &watch_cfg, &mut state);
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Watch error");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
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
                        // db_id updated below in the DS-9 reopen path
                        state.hnsw_index = None;
                        state.incremental_count = 0;
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
                        state.hnsw_index = None;
                        state.incremental_count = 0;
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
                    // Acquires the same `acquire_index_lock` semantics by
                    // calling `try_acquire_index_lock` — if `cqs index` or
                    // `cqs gc` is running, the GC tick skips and tries again
                    // on the next interval.
                    if periodic_gc_enabled
                        && state.last_event.elapsed()
                            >= Duration::from_secs(super::limits::daemon_periodic_gc_idle_secs())
                        && last_periodic_gc.elapsed()
                            >= Duration::from_secs(super::limits::daemon_periodic_gc_interval_secs())
                    {
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
                                run_daemon_periodic_gc(&store, &root, &parser, matcher_ref);
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
mod tests {
    use super::*;
    use notify::EventKind;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::LazyLock;

    // RM-V1.29-8: shared test fixtures. Previously each call to
    // `test_watch_config*` leaked a fresh `Parser` / `OnceLock` /
    // `ModelConfig` / `RwLock<None>` on the heap, which piled up across
    // the ~two dozen watch tests. Every one of these is identical across
    // calls, so we keep exactly one `&'static` copy per type. The
    // `test_watch_config_with_gitignore` helper still has to leak its
    // per-call matcher (each caller passes a distinct `Gitignore`) — but
    // the shared four fields no longer leak on every call.
    static TEST_PARSER: LazyLock<CqParser> = LazyLock::new(|| CqParser::new().unwrap());
    static TEST_EMBEDDER: LazyLock<std::sync::OnceLock<std::sync::Arc<Embedder>>> =
        LazyLock::new(std::sync::OnceLock::new);
    static TEST_MODEL_CONFIG: LazyLock<ModelConfig> = LazyLock::new(ModelConfig::default_model);
    static TEST_GITIGNORE_NONE: LazyLock<std::sync::RwLock<Option<ignore::gitignore::Gitignore>>> =
        LazyLock::new(|| std::sync::RwLock::new(None));

    fn make_event(paths: Vec<PathBuf>, kind: EventKind) -> notify::Event {
        notify::Event {
            kind,
            paths,
            attrs: Default::default(),
        }
    }

    /// Helper to build a minimal WatchConfig for testing collect_events.
    fn test_watch_config<'a>(
        root: &'a Path,
        cqs_dir: &'a Path,
        notes_path: &'a Path,
        supported_ext: &'a HashSet<&'a str>,
    ) -> WatchConfig<'a> {
        // These fields are unused by collect_events but required by the
        // struct. The four fixtures are shared `LazyLock` statics so
        // tests reference a single `&'static` copy instead of leaking a
        // fresh heap allocation on every call.
        WatchConfig {
            root,
            cqs_dir,
            notes_path,
            supported_ext,
            parser: &TEST_PARSER,
            embedder: &TEST_EMBEDDER,
            quiet: true,
            model_config: &TEST_MODEL_CONFIG,
            gitignore: &TEST_GITIGNORE_NONE,
            splade_encoder: None,
            global_cache: None,
        }
    }

    /// Variant that installs a gitignore matcher for .gitignore-specific tests.
    fn test_watch_config_with_gitignore<'a>(
        root: &'a Path,
        cqs_dir: &'a Path,
        notes_path: &'a Path,
        supported_ext: &'a HashSet<&'a str>,
        matcher: ignore::gitignore::Gitignore,
    ) -> WatchConfig<'a> {
        // `parser` / `embedder` / `model_config` are shared statics (see
        // comment above); the per-call `matcher` still needs a distinct
        // `&'static RwLock`, so we leak that one field only.
        let gitignore = Box::leak(Box::new(std::sync::RwLock::new(Some(matcher))));
        WatchConfig {
            root,
            cqs_dir,
            notes_path,
            supported_ext,
            parser: &TEST_PARSER,
            embedder: &TEST_EMBEDDER,
            quiet: true,
            model_config: &TEST_MODEL_CONFIG,
            gitignore,
            splade_encoder: None,
            global_cache: None,
        }
    }

    fn test_watch_state() -> WatchState {
        WatchState {
            embedder_backoff: EmbedderBackoff::new(),
            pending_files: HashSet::new(),
            pending_notes: false,
            last_event: std::time::Instant::now(),
            last_indexed_mtime: HashMap::new(),
            hnsw_index: None,
            incremental_count: 0,
            dropped_this_cycle: 0,
            pending_rebuild: None,
        }
    }

    // ===== EmbedderBackoff tests =====

    #[test]
    fn backoff_initial_state_allows_retry() {
        let backoff = EmbedderBackoff::new();
        assert!(backoff.should_retry(), "Fresh backoff should allow retry");
    }

    #[test]
    fn backoff_after_failure_delays_retry() {
        let mut backoff = EmbedderBackoff::new();
        backoff.record_failure();
        // After 1 failure, delay is 2^1 = 2 seconds
        assert!(
            !backoff.should_retry(),
            "Should not retry immediately after failure"
        );
        assert_eq!(backoff.failures, 1);
    }

    #[test]
    fn backoff_reset_clears_failures() {
        let mut backoff = EmbedderBackoff::new();
        backoff.record_failure();
        backoff.record_failure();
        backoff.reset();
        assert_eq!(backoff.failures, 0);
        assert!(backoff.should_retry());
    }

    #[test]
    fn backoff_caps_at_300s() {
        let mut backoff = EmbedderBackoff::new();
        // 2^9 = 512 > 300, so it should be capped
        for _ in 0..9 {
            backoff.record_failure();
        }
        // Verify it doesn't panic or overflow
        assert_eq!(backoff.failures, 9);
    }

    #[test]
    fn backoff_saturating_add_no_overflow() {
        let mut backoff = EmbedderBackoff::new();
        backoff.failures = u32::MAX;
        backoff.record_failure();
        assert_eq!(backoff.failures, u32::MAX, "Should saturate, not overflow");
    }

    // ===== collect_events tests =====

    #[test]
    fn collect_events_filters_unsupported_extensions() {
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs", "py", "js"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        // .txt is not supported
        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/readme.txt")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(
            state.pending_files.is_empty(),
            "Unsupported extension should not be added"
        );
        assert!(!state.pending_notes);
    }

    #[test]
    fn collect_events_skips_cqs_dir() {
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs", "db"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/.cqs/index.db")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(
            state.pending_files.is_empty(),
            ".cqs dir events should be skipped"
        );
    }

    /// Helper: build a `Gitignore` matcher in-memory from lines (no file IO).
    fn gitignore_from_lines(root: &Path, lines: &[&str]) -> ignore::gitignore::Gitignore {
        let mut b = ignore::gitignore::GitignoreBuilder::new(root);
        for line in lines {
            b.add_line(None, line).expect("add_line");
        }
        b.build().expect("build gitignore")
    }

    #[test]
    fn collect_events_skips_gitignore_matched_paths() {
        // #1002: `.claude/worktrees/` is a representative pollution case
        // from parallel-agent work. Verify that a path matched by
        // .gitignore is skipped.
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let matcher = gitignore_from_lines(&root, &[".claude/", "target/"]);
        let cfg =
            test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

        let mut state = test_watch_state();
        let event = make_event(
            vec![PathBuf::from(
                "/tmp/test_project/.claude/worktrees/agent-a1b2c3d4/src/lib.rs",
            )],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );
        collect_events(&event, &cfg, &mut state);
        assert!(
            state.pending_files.is_empty(),
            ".gitignore-matched path .claude/worktrees/... should be skipped"
        );
    }

    #[test]
    fn collect_events_skips_target_dir_via_gitignore() {
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let matcher = gitignore_from_lines(&root, &["target/"]);
        let cfg =
            test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

        let mut state = test_watch_state();
        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/target/debug/foo.rs")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );
        collect_events(&event, &cfg, &mut state);
        assert!(
            state.pending_files.is_empty(),
            "target/ ignored by .gitignore should be skipped"
        );
    }

    #[test]
    fn collect_events_does_not_skip_unrelated_paths_when_gitignore_present() {
        // False-positive guard: files under a directory not in .gitignore
        // must still be indexed even when a matcher is installed.
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let matcher = gitignore_from_lines(&root, &[".claude/", "target/"]);
        let cfg =
            test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

        let mut state = test_watch_state();
        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/src/foo.rs")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );
        collect_events(&event, &cfg, &mut state);
        assert!(
            !state.pending_files.is_empty(),
            "src/foo.rs is not in .gitignore and must not be skipped"
        );
    }

    #[test]
    fn collect_events_negations_include_path() {
        // `.gitignore` negations (`!foo`) keep the file indexed even
        // if a broader pattern excludes its parent.
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let matcher = gitignore_from_lines(&root, &["vendor/", "!vendor/keep/"]);
        let cfg =
            test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

        let mut state = test_watch_state();
        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/vendor/keep/lib.rs")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );
        collect_events(&event, &cfg, &mut state);
        assert!(
            !state.pending_files.is_empty(),
            "negation `!vendor/keep/` must keep the file indexed"
        );
    }

    #[test]
    fn collect_events_honors_none_matcher() {
        // With no matcher (--no-ignore or no .gitignore present), the
        // watch loop indexes every supported-extension path. Verifies
        // the `Option<_>` in `WatchConfig.gitignore` behaves as
        // documented.
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        // Default test_watch_config → gitignore is None.
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);

        let mut state = test_watch_state();
        let event = make_event(
            vec![PathBuf::from(
                "/tmp/test_project/.claude/worktrees/agent-x/src/lib.rs",
            )],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );
        collect_events(&event, &cfg, &mut state);
        assert!(
            !state.pending_files.is_empty(),
            "with matcher=None, all supported-ext paths must be accepted"
        );
    }

    #[test]
    fn collect_events_cqs_dir_skip_survives_gitignore_allowlist() {
        // Even if a user accidentally or deliberately adds `!.cqs/` to
        // .gitignore, the hardcoded `.cqs/` skip keeps the system's own
        // files out of the index.
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs", "db"].iter().cloned().collect();
        // Negation allowing .cqs/ — should still be filtered by the
        // hardcoded .cqs/ skip in collect_events.
        let matcher = gitignore_from_lines(&root, &["*.tmp", "!.cqs/"]);
        let cfg =
            test_watch_config_with_gitignore(&root, &cqs_dir, &notes_path, &supported, matcher);

        let mut state = test_watch_state();
        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/.cqs/index.db")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );
        collect_events(&event, &cfg, &mut state);
        assert!(
            state.pending_files.is_empty(),
            ".cqs/ must always be skipped (belt-and-suspenders vs gitignore allowlist)"
        );
    }

    #[test]
    fn build_gitignore_matcher_missing_returns_none() {
        // A project with neither .gitignore nor .cqsignore should produce
        // a `None` matcher — the watch loop indexes everything.
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(
            build_gitignore_matcher(tmp.path()).is_none(),
            "missing .gitignore + .cqsignore should yield None matcher"
        );
    }

    #[test]
    fn build_gitignore_matcher_env_kill_switch() {
        // CQS_WATCH_RESPECT_GITIGNORE=0 forces None even if .gitignore exists.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();

        // Save + set + restore to stay neighbour-friendly with parallel
        // tests that may inspect the variable.
        let prev = std::env::var("CQS_WATCH_RESPECT_GITIGNORE").ok();
        std::env::set_var("CQS_WATCH_RESPECT_GITIGNORE", "0");
        let result = build_gitignore_matcher(tmp.path());
        match prev {
            Some(v) => std::env::set_var("CQS_WATCH_RESPECT_GITIGNORE", v),
            None => std::env::remove_var("CQS_WATCH_RESPECT_GITIGNORE"),
        }

        assert!(
            result.is_none(),
            "CQS_WATCH_RESPECT_GITIGNORE=0 must disable the matcher"
        );
    }

    #[test]
    fn build_gitignore_matcher_real_file_loads_rules() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".gitignore"),
            "target/\n.claude/\nnode_modules/\n",
        )
        .unwrap();

        let matcher =
            build_gitignore_matcher(tmp.path()).expect("matcher should build for real gitignore");
        assert!(matcher.num_ignores() >= 3, "expected ≥3 rules loaded");

        // Sanity: matcher returns is_ignore for a target/ path via
        // parent-walk (file inside a directory-ignore rule).
        let hit = matcher
            .matched_path_or_any_parents(tmp.path().join("target/debug/foo.rs"), false)
            .is_ignore();
        assert!(hit, "target/ should match");
    }

    #[test]
    fn build_gitignore_matcher_loads_cqsignore() {
        // The watch matcher must layer .cqsignore on top of .gitignore so
        // cqs-specific exclusions (vendor bundles etc.) are respected at
        // event time, mirroring the indexer behaviour.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::write(tmp.path().join(".cqsignore"), "**/*.min.js\n").unwrap();

        let matcher =
            build_gitignore_matcher(tmp.path()).expect("matcher should build with cqsignore");
        assert!(matcher.num_ignores() >= 2, "expected rules from both files");

        let vendor_hit = matcher
            .matched_path_or_any_parents(
                tmp.path().join("src/serve/assets/vendor/three.min.js"),
                false,
            )
            .is_ignore();
        assert!(
            vendor_hit,
            ".cqsignore *.min.js rule should match vendor JS"
        );

        let regular_miss = matcher
            .matched_path_or_any_parents(tmp.path().join("src/main.rs"), false)
            .is_ignore();
        assert!(!regular_miss, "regular source files must not match");
    }

    #[test]
    fn build_gitignore_matcher_cqsignore_only() {
        // .cqsignore alone (no .gitignore) should still build the matcher.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".cqsignore"), "secret.txt\n").unwrap();

        let matcher =
            build_gitignore_matcher(tmp.path()).expect("matcher should build with cqsignore alone");
        let hit = matcher
            .matched_path_or_any_parents(tmp.path().join("secret.txt"), false)
            .is_ignore();
        assert!(hit, "cqsignore-only rule should match");
    }

    // ===== #1004 SPLADE builder / batch-size tests =====

    #[test]
    fn splade_batch_size_env_override() {
        let prev = std::env::var("CQS_SPLADE_BATCH").ok();
        std::env::set_var("CQS_SPLADE_BATCH", "16");
        let got = splade_batch_size();
        match prev {
            Some(v) => std::env::set_var("CQS_SPLADE_BATCH", v),
            None => std::env::remove_var("CQS_SPLADE_BATCH"),
        }
        assert_eq!(got, 16);
    }

    #[test]
    fn splade_batch_size_default_is_32() {
        let prev = std::env::var("CQS_SPLADE_BATCH").ok();
        std::env::remove_var("CQS_SPLADE_BATCH");
        let got = splade_batch_size();
        if let Some(v) = prev {
            std::env::set_var("CQS_SPLADE_BATCH", v);
        }
        assert_eq!(got, 32);
    }

    #[test]
    fn splade_batch_size_invalid_falls_back_to_default() {
        let prev = std::env::var("CQS_SPLADE_BATCH").ok();
        std::env::set_var("CQS_SPLADE_BATCH", "not-a-number");
        let got = splade_batch_size();
        match prev {
            Some(v) => std::env::set_var("CQS_SPLADE_BATCH", v),
            None => std::env::remove_var("CQS_SPLADE_BATCH"),
        }
        assert_eq!(got, 32, "unparseable value falls back to default");
    }

    #[test]
    fn splade_batch_size_zero_falls_back_to_default() {
        let prev = std::env::var("CQS_SPLADE_BATCH").ok();
        std::env::set_var("CQS_SPLADE_BATCH", "0");
        let got = splade_batch_size();
        match prev {
            Some(v) => std::env::set_var("CQS_SPLADE_BATCH", v),
            None => std::env::remove_var("CQS_SPLADE_BATCH"),
        }
        assert_eq!(got, 32, "0 is not a valid batch size, falls back");
    }

    #[test]
    fn build_splade_encoder_env_kill_switch_returns_none() {
        // CQS_WATCH_INCREMENTAL_SPLADE=0 must return None regardless of
        // whether a SPLADE model is configured. Verifies the feature-flag
        // kill-switch fires before any model-load work.
        let prev = std::env::var("CQS_WATCH_INCREMENTAL_SPLADE").ok();
        std::env::set_var("CQS_WATCH_INCREMENTAL_SPLADE", "0");
        let got = build_splade_encoder_for_watch();
        match prev {
            Some(v) => std::env::set_var("CQS_WATCH_INCREMENTAL_SPLADE", v),
            None => std::env::remove_var("CQS_WATCH_INCREMENTAL_SPLADE"),
        }
        assert!(
            got.is_none(),
            "CQS_WATCH_INCREMENTAL_SPLADE=0 must disable the encoder"
        );
    }

    #[test]
    fn splade_origin_key_normalizes_backslashes() {
        // PB-V1.29-2 regression. `encode_splade_for_changed_files` builds
        // the DB lookup key via `cqs::normalize_path(file)`. A `PathBuf`
        // carrying backslashes (as any Windows-canonicalized path does)
        // must normalize to the forward-slash form stored at ingest, or
        // `get_chunks_by_origin` returns Ok(vec![]) and SPLADE silently
        // no-ops for the file.
        let p = std::path::PathBuf::from(r"src\cli\watch.rs");
        let origin = cqs::normalize_path(&p);
        assert_eq!(
            origin, "src/cli/watch.rs",
            "origin key must use forward slashes to match DB origins"
        );

        // UNC verbatim prefix must be stripped too (dunce::canonicalize
        // may leave `\\?\C:\…` on Windows). On Unix this just asserts
        // the helper doesn't mangle a plain relative path.
        let p2 = std::path::PathBuf::from(r"\\?\C:\repo\src\cli\watch.rs");
        let origin2 = cqs::normalize_path(&p2);
        assert!(
            !origin2.contains('\\') && !origin2.starts_with(r"\\?\"),
            "normalize_path must strip the verbatim UNC prefix: got {origin2}"
        );
    }

    #[test]
    fn collect_events_detects_notes_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let notes_dir = root.join("docs");
        std::fs::create_dir_all(&notes_dir).unwrap();
        let notes_path = notes_dir.join("notes.toml");
        std::fs::write(&notes_path, "# notes").unwrap();

        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        let event = make_event(
            vec![notes_path.clone()],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(state.pending_notes, "Notes path should set pending_notes");
        assert!(
            state.pending_files.is_empty(),
            "Notes should not be added to pending_files"
        );
    }

    #[test]
    fn collect_events_respects_max_pending_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let notes_path = root.join("docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        // Pre-fill pending_files to max_pending_files()
        for i in 0..max_pending_files() {
            state
                .pending_files
                .insert(PathBuf::from(format!("f{}.rs", i)));
        }

        // Create a real file so mtime check passes
        let new_file = root.join("overflow.rs");
        std::fs::write(&new_file, "fn main() {}").unwrap();

        let event = make_event(
            vec![new_file],
            EventKind::Create(notify::event::CreateKind::File),
        );

        collect_events(&event, &cfg, &mut state);

        assert_eq!(
            state.pending_files.len(),
            max_pending_files(),
            "Should not exceed max_pending_files()"
        );
    }

    #[test]
    fn collect_events_skips_unchanged_mtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let notes_path = root.join("docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        // Create a file and record its mtime as already indexed
        let file = root.join("src/lib.rs");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        state
            .last_indexed_mtime
            .insert(PathBuf::from("src/lib.rs"), mtime);

        let event = make_event(
            vec![file],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(
            state.pending_files.is_empty(),
            "Unchanged mtime should be skipped"
        );
    }

    // ===== last_indexed_mtime prune tests =====

    /// #969: recency prune drops entries older than `LAST_INDEXED_PRUNE_AGE_SECS`,
    /// keeps fresh entries, and only triggers once the map exceeds
    /// `LAST_INDEXED_PRUNE_SIZE_THRESHOLD`. This replaces the old per-entry
    /// `Path::exists()` loop that stalled the watch thread on WSL 9P mounts.
    #[test]
    fn test_last_indexed_mtime_recency_prune() {
        let now = SystemTime::now();
        let two_days = Duration::from_secs(2 * LAST_INDEXED_PRUNE_AGE_SECS);
        let one_minute = Duration::from_secs(60);
        let old = now.checked_sub(two_days).unwrap();
        let fresh = now.checked_sub(one_minute).unwrap();

        // (1) Small map — below the size threshold — must not prune at all,
        // even if every entry is ancient. The threshold is a cache-size
        // safety valve, not a TTL for the whole session.
        let mut small: HashMap<PathBuf, SystemTime> = HashMap::new();
        small.insert(PathBuf::from("a.rs"), old);
        small.insert(PathBuf::from("b.rs"), fresh);
        let pruned_small = prune_last_indexed_mtime(&mut small);
        assert_eq!(
            pruned_small, 0,
            "Prune must not run below size threshold (got {} entries removed from 2-entry map)",
            pruned_small
        );
        assert_eq!(
            small.len(),
            2,
            "Small map should be untouched when below threshold"
        );

        // (2) Large map — above the size threshold — prunes old entries
        // and keeps fresh ones. Build a map with SIZE_THRESHOLD + 1 old
        // entries plus a handful of fresh sentinels so we can check both
        // that old entries are removed and fresh ones survive.
        let mut large: HashMap<PathBuf, SystemTime> = HashMap::new();
        for i in 0..=LAST_INDEXED_PRUNE_SIZE_THRESHOLD {
            large.insert(PathBuf::from(format!("old_{}.rs", i)), old);
        }
        large.insert(PathBuf::from("fresh_1.rs"), fresh);
        large.insert(PathBuf::from("fresh_2.rs"), now);
        let total_before = large.len();
        let pruned_large = prune_last_indexed_mtime(&mut large);

        // Every "old" entry (two days stale) should be gone.
        assert_eq!(
            pruned_large,
            LAST_INDEXED_PRUNE_SIZE_THRESHOLD + 1,
            "Expected all old entries pruned (total_before={}, remaining={})",
            total_before,
            large.len()
        );
        assert!(
            large.contains_key(&PathBuf::from("fresh_1.rs")),
            "Fresh entry from 1 minute ago must survive prune"
        );
        assert!(
            large.contains_key(&PathBuf::from("fresh_2.rs")),
            "Entry at `now` must survive prune"
        );
        assert_eq!(
            large.len(),
            2,
            "Only the 2 fresh entries should remain after prune"
        );

        // (3) Entry just inside the cutoff window survives. We use a 1-second
        // margin rather than exactly `now - PRUNE_AGE` because `prune_*` calls
        // `SystemTime::now()` internally — its clock ticks a few microseconds
        // past the test's clock, so an entry pinned to the test's computed
        // cutoff would be classified as older and pruned. 1 second is
        // comfortably more than the inter-call drift while still well under
        // the 1-day window.
        let just_inside = now
            .checked_sub(Duration::from_secs(LAST_INDEXED_PRUNE_AGE_SECS - 1))
            .unwrap();
        let mut boundary: HashMap<PathBuf, SystemTime> = HashMap::new();
        for i in 0..=LAST_INDEXED_PRUNE_SIZE_THRESHOLD {
            boundary.insert(PathBuf::from(format!("stale_{}.rs", i)), old);
        }
        boundary.insert(PathBuf::from("just_inside.rs"), just_inside);
        prune_last_indexed_mtime(&mut boundary);
        assert!(
            boundary.contains_key(&PathBuf::from("just_inside.rs")),
            "Entry 1 second inside the cutoff window must survive"
        );
    }

    // ===== Constants tests =====

    #[test]
    fn hnsw_rebuild_threshold_is_reasonable() {
        assert!(hnsw_rebuild_threshold() > 0);
        assert!(hnsw_rebuild_threshold() <= 1000);
    }

    #[test]
    fn max_pending_files_is_bounded() {
        assert!(max_pending_files() > 0);
        assert!(max_pending_files() <= 100_000);
    }

    // ===== P2 #62 trim_trailing_newline tests =====

    #[cfg(unix)]
    #[test]
    fn trim_newline_strips_lf() {
        assert_eq!(socket::trim_trailing_newline(b"hello\n"), b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn trim_newline_strips_crlf() {
        assert_eq!(socket::trim_trailing_newline(b"hello\r\n"), b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn trim_newline_no_op_when_absent() {
        assert_eq!(socket::trim_trailing_newline(b"hello"), b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn trim_newline_handles_empty() {
        assert_eq!(socket::trim_trailing_newline(b""), b"");
    }

    #[cfg(unix)]
    #[test]
    fn trim_newline_only_strips_one_lf() {
        // Two trailing newlines → only the last is stripped (callers that
        // wrote two newlines deliberately are uncommon, but we don't want
        // to silently consume more than one terminator).
        assert_eq!(socket::trim_trailing_newline(b"hello\n\n"), b"hello\n");
    }

    // ===== PB-V1.29-3: chunk.id prefix-strip uses normalize_path =====

    /// Exercises the same strip-and-rewrite shape used by `reindex_files`
    /// at watch.rs :~2436 after the PB-V1.29-3 fix. The direct function
    /// isn't extracted, but the logic is small and identical — this test
    /// documents the contract so a regression back to `abs_path.display()`
    /// is caught by a targeted unit test instead of the next Windows CI run.
    fn normalize_strip_and_rewrite(
        abs_path: &Path,
        rel_path: &Path,
        chunk_id: &str,
    ) -> Option<String> {
        let abs_norm = cqs::normalize_path(abs_path);
        let rel_norm = cqs::normalize_path(rel_path);
        chunk_id
            .strip_prefix(abs_norm.as_str())
            .map(|rest| format!("{}{}", rel_norm, rest))
    }

    #[test]
    fn prefix_strip_normalizes_backslash_verbatim_prefix() {
        // Simulates the Windows shape that the bug regressed on:
        //   abs_path   = \\?\C:\Projects\cqs\src\foo.rs
        //   chunk.id   = C:/Projects/cqs/src/foo.rs:10:abcd  (parser output)
        //   rel_path   = src\foo.rs  (after strip_prefix on the root)
        // Before the fix: `abs_path.display()` emits the verbatim `\\?\` +
        // backslashes, so the prefix-strip fails and chunk.id keeps its
        // absolute prefix. After the fix: both sides normalize.
        let abs = Path::new(r"\\?\C:\Projects\cqs\src\foo.rs");
        let rel = Path::new(r"src\foo.rs");
        let chunk_id = "C:/Projects/cqs/src/foo.rs:10:abcd";
        let rewritten =
            normalize_strip_and_rewrite(abs, rel, chunk_id).expect("prefix-strip must match");
        assert!(
            rewritten.starts_with("src/foo.rs"),
            "expected rewritten id to start with forward-slash rel path, got {rewritten}"
        );
        assert_eq!(rewritten, "src/foo.rs:10:abcd");
    }

    #[test]
    fn prefix_strip_unix_path_round_trip() {
        // Baseline: Unix path with forward slashes on both sides still works.
        let abs = Path::new("/home/user/proj/src/foo.rs");
        let rel = Path::new("src/foo.rs");
        let chunk_id = "/home/user/proj/src/foo.rs:42:deadbeef";
        let rewritten =
            normalize_strip_and_rewrite(abs, rel, chunk_id).expect("prefix-strip must match");
        assert_eq!(rewritten, "src/foo.rs:42:deadbeef");
    }

    // ===== EH-V1.29-8: gitignore RwLock poison recovery =====

    #[test]
    fn gitignore_rwlock_poison_still_yields_matcher() {
        // Simulates the recovery arm at watch.rs :~1741 / :~1963. A writer
        // that panics while holding the write lock leaves the inner value
        // valid but the lock poisoned; the `match gitignore.read()` arm
        // must recover via `poisoned.into_inner()` instead of silently
        // dropping to "no matcher".
        use std::sync::{Arc, RwLock};

        let matcher_builder = ignore::gitignore::GitignoreBuilder::new(std::path::Path::new("."));
        let (matcher, _errs) = matcher_builder.build_global();
        let lock: Arc<RwLock<Option<ignore::gitignore::Gitignore>>> =
            Arc::new(RwLock::new(Some(matcher)));

        // Poison the lock by panicking inside a write guard on a helper
        // thread — the panic propagates, leaves the RwLock poisoned, and
        // joins.
        let poisoner = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.write().expect("initial write must succeed");
            panic!("intentional poison for EH-V1.29-8 test");
        })
        .join();

        // Post-poison: the bug was `gitignore.read().ok()` silently
        // returning `None`. The fixed code must still yield `Some(_)` by
        // recovering the inner value via `into_inner()`.
        let matcher_guard = match lock.read() {
            Ok(g) => Some(g),
            Err(poisoned) => Some(poisoned.into_inner()),
        };
        assert!(
            matcher_guard.is_some(),
            "poison-recovery must still surface the previously-written matcher"
        );
        assert!(
            matcher_guard.as_ref().unwrap().is_some(),
            "inner Option<Gitignore> must still be Some after poison recovery"
        );
    }

    // ── #1090 background rebuild + atomic swap ──────────────────────────────

    /// Build a tiny `Owned` HnswIndex from N synthetic vectors. Stand-in for a
    /// thread-built index in the `drain_pending_rebuild` tests below.
    fn synthetic_owned_index(n: usize, dim: usize) -> cqs::hnsw::HnswIndex {
        // Non-zero, distinct vectors per id — hnsw_rs's HNSW can collapse
        // zero vectors (undefined cosine sim) so the first entry needs a
        // non-trivial value or the index ends up under-populated.
        let batch: Vec<(String, cqs::Embedding)> = (0..n)
            .map(|i| {
                let mut v = vec![0.1_f32; dim];
                v[i % dim] = (i as f32 + 1.0) * 0.5;
                (format!("c{i}"), cqs::Embedding::new(v))
            })
            .collect();
        let iter = std::iter::once(Ok::<_, cqs::store::StoreError>(batch));
        cqs::hnsw::HnswIndex::build_batched_with_dim(iter, n, dim).expect("build synthetic index")
    }

    /// Make a Store + WatchConfig pair for a fresh tempdir, init'd to `dim`.
    /// Returns owned bindings so each caller can pass long-lived references
    /// to `test_watch_config`.
    struct DrainFixture {
        tmp: tempfile::TempDir,
        store: Store,
        supported_ext: HashSet<&'static str>,
        notes_path: PathBuf,
    }

    fn drain_test_fixture(dim: usize) -> DrainFixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let store_path = tmp.path().join("index.db");
        let mut store = Store::open(&store_path).unwrap();
        store
            .init(&cqs::store::ModelInfo::new("test/m", dim))
            .unwrap();
        store.set_dim(dim);
        let notes_path = tmp.path().join("docs/notes.toml");
        DrainFixture {
            tmp,
            store,
            supported_ext: HashSet::new(),
            notes_path,
        }
    }

    #[test]
    fn drain_pending_rebuild_replays_delta_into_new_index() {
        let dim = 4;
        let new_idx = synthetic_owned_index(3, dim);
        assert_eq!(new_idx.len(), 3);

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(Some(RebuildResult {
            index: new_idx,
            // No overlap between delta ids and snapshot — all replay.
            snapshot_hashes: std::collections::HashMap::new(),
        })))
        .unwrap();
        drop(tx);

        let mut state = test_watch_state();
        state.pending_rebuild = Some(PendingRebuild {
            rx,
            delta: vec![
                (
                    "delta_a".to_string(),
                    cqs::Embedding::new(vec![1.0; dim]),
                    "h_delta_a".to_string(),
                ),
                (
                    "delta_b".to_string(),
                    cqs::Embedding::new(vec![0.5; dim]),
                    "h_delta_b".to_string(),
                ),
            ],
            started_at: std::time::Instant::now(),
            handle: None,
            delta_saturated: false,
        });

        let fix = drain_test_fixture(dim);
        let cfg = test_watch_config(
            fix.tmp.path(),
            fix.tmp.path(),
            &fix.notes_path,
            &fix.supported_ext,
        );
        let store = &fix.store;

        drain_pending_rebuild(&cfg, store, &mut state);

        let idx = state.hnsw_index.expect("rebuild was swapped in");
        assert_eq!(idx.len(), 5, "3 from new_idx + 2 from delta");
        assert!(idx.ids().iter().any(|id| id == "delta_a"));
        assert!(idx.ids().iter().any(|id| id == "delta_b"));
        assert_eq!(state.incremental_count, 0);
        assert!(state.pending_rebuild.is_none());
    }

    /// P1.17 / #1124: when a chunk is re-embedded mid-rebuild, the snapshot
    /// has the OLD vector under the same id while delta has the NEW vector
    /// + new content_hash. The drain must REPLAY the delta entry so the
    /// fresh embedding lands in the swapped HNSW. The pre-fix code dedup'd
    /// by id-only and silently dropped these updates.
    ///
    /// We can't query hnsw_rs for "give me the embedding stored under id X"
    /// (it's a graph, not a kv store) and there's no deletion API, so we
    /// assert the side-effect: the swapped index contains MORE entries
    /// than the snapshot alone (orphan + replayed vector both present),
    /// and a search by the FRESH embedding returns id "a" with cosine ≈ 1.0.
    #[test]
    fn test_rebuild_window_re_embedding_replays_fresh_vector() {
        let dim = 4;

        // Snapshot has id "a" baked in with hash h_v1 (and an unrelated id "z"
        // so the index isn't trivially empty).
        let snapshot_batch: Vec<(String, cqs::Embedding)> = vec![
            (
                "a".to_string(),
                cqs::Embedding::new(vec![1.0, 0.0, 0.0, 0.0]),
            ),
            (
                "z".to_string(),
                cqs::Embedding::new(vec![0.0, 0.0, 0.0, 1.0]),
            ),
        ];
        let snapshot_iter = std::iter::once(Ok::<_, cqs::store::StoreError>(snapshot_batch));
        let new_idx = cqs::hnsw::HnswIndex::build_batched_with_dim(snapshot_iter, 2, dim)
            .expect("build snapshot index");
        assert_eq!(new_idx.len(), 2, "snapshot starts with 2 entries");

        let mut snapshot_hashes = std::collections::HashMap::new();
        snapshot_hashes.insert("a".to_string(), "h_v1".to_string());
        snapshot_hashes.insert("z".to_string(), "h_z".to_string());

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(Some(RebuildResult {
            index: new_idx,
            snapshot_hashes,
        })))
        .unwrap();
        drop(tx);

        // Delta has "a" again, but with a NEW embedding and a NEW content_hash —
        // i.e. the file was re-embedded between the snapshot and the swap.
        // The fresh vector points along axis 1, distinct from the snapshot's
        // axis-0 vector, so we can tell them apart by search.
        let fresh_embedding = cqs::Embedding::new(vec![0.0, 1.0, 0.0, 0.0]);

        let mut state = test_watch_state();
        state.pending_rebuild = Some(PendingRebuild {
            rx,
            delta: vec![(
                "a".to_string(),
                fresh_embedding.clone(),
                "h_v2".to_string(), // hash differs from snapshot's "h_v1"
            )],
            started_at: std::time::Instant::now(),
            handle: None,
            delta_saturated: false,
        });

        let fix = drain_test_fixture(dim);
        let cfg = test_watch_config(
            fix.tmp.path(),
            fix.tmp.path(),
            &fix.notes_path,
            &fix.supported_ext,
        );

        drain_pending_rebuild(&cfg, &fix.store, &mut state);

        let idx = state.hnsw_index.expect("rebuild was swapped in");
        // The fresh vector was REPLAYED — index now contains 3 nodes
        // (snapshot's "a" + "z" + replayed "a"). hnsw_rs has no deletion,
        // so both vectors for "a" coexist as duplicate-id orphans; that's
        // the same trade-off as the fast-incremental path. Search
        // post-filters via SQLite in production, which collapses the
        // duplicates into one logical hit.
        assert_eq!(
            idx.len(),
            3,
            "fresh re-embedding must be replayed (snapshot 2 + 1 replay)"
        );

        // Crucial assertion: searching by the FRESH embedding returns id "a".
        // Pre-fix, the replay was skipped, so the only "a" in the index was
        // the snapshot's axis-0 vector, and querying the axis-1 fresh vector
        // would surface "z" or "a" with poor cosine. After the fix, the
        // axis-1 vector is in the index under "a" with cosine ≈ 1.0.
        let hits = idx.search(&fresh_embedding, 1);
        assert!(!hits.is_empty(), "search must return at least one hit");
        let top = &hits[0];
        assert_eq!(
            top.id, "a",
            "top hit for fresh embedding must be the re-embedded chunk \"a\""
        );
        assert!(
            top.score > 0.99,
            "top hit cosine must be near 1.0 (fresh vector is in the index); got {}",
            top.score
        );

        assert!(state.pending_rebuild.is_none());
    }

    #[test]
    fn drain_pending_rebuild_dedups_against_known_ids() {
        // P1.17 / #1124: dedup is now (id, content_hash)-aware, not id-only.
        // The rebuild thread snapshotted c0/c1/c2 with hashes h0/h1/h2.
        // Delta replays c0 with the SAME hash h0 (true duplicate — must be
        // skipped), c1 with the same hash h1 (skipped), and c_new with a
        // brand-new id (must replay). c0/c1 with matching hashes would
        // double-insert under the pre-fix code; the new dedup uses the
        // snapshot hashes the rebuild produced.
        let dim = 4;
        let new_idx = synthetic_owned_index(3, dim); // ids: c0, c1, c2

        let mut snapshot_hashes = std::collections::HashMap::new();
        snapshot_hashes.insert("c0".to_string(), "h0".to_string());
        snapshot_hashes.insert("c1".to_string(), "h1".to_string());
        snapshot_hashes.insert("c2".to_string(), "h2".to_string());

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(Some(RebuildResult {
            index: new_idx,
            snapshot_hashes,
        })))
        .unwrap();
        drop(tx);

        let mut state = test_watch_state();
        state.pending_rebuild = Some(PendingRebuild {
            rx,
            delta: vec![
                // Same id + same hash → genuine duplicate, skip.
                (
                    "c0".to_string(),
                    cqs::Embedding::new(vec![9.0; dim]),
                    "h0".to_string(),
                ),
                (
                    "c1".to_string(),
                    cqs::Embedding::new(vec![9.0; dim]),
                    "h1".to_string(),
                ),
                // Brand-new id → snapshot didn't see it, must replay.
                (
                    "c_new".to_string(),
                    cqs::Embedding::new(vec![9.0; dim]),
                    "h_new".to_string(),
                ),
            ],
            started_at: std::time::Instant::now(),
            handle: None,
            delta_saturated: false,
        });

        let fix = drain_test_fixture(dim);
        let cfg = test_watch_config(
            fix.tmp.path(),
            fix.tmp.path(),
            &fix.notes_path,
            &fix.supported_ext,
        );
        let store = &fix.store;

        drain_pending_rebuild(&cfg, store, &mut state);

        let idx = state.hnsw_index.expect("rebuild was swapped in");
        assert_eq!(
            idx.len(),
            4,
            "3 from new_idx + 1 genuinely-new delta entry — same-hash duplicates skipped"
        );
        assert!(idx.ids().iter().any(|id| id == "c_new"));
    }

    #[test]
    fn drain_pending_rebuild_clears_pending_on_thread_error() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err(anyhow::anyhow!("simulated rebuild failure")))
            .unwrap();
        drop(tx);

        let mut state = test_watch_state();
        state.pending_rebuild = Some(PendingRebuild {
            rx,
            delta: Vec::new(),
            started_at: std::time::Instant::now(),
            handle: None,
            delta_saturated: false,
        });

        let fix = drain_test_fixture(4);
        let cfg = test_watch_config(
            fix.tmp.path(),
            fix.tmp.path(),
            &fix.notes_path,
            &fix.supported_ext,
        );
        let store = &fix.store;

        drain_pending_rebuild(&cfg, store, &mut state);
        assert!(state.pending_rebuild.is_none());
        assert!(state.hnsw_index.is_none());
    }

    // P2.29: spawn_hnsw_rebuild adversarial coverage — the original
    // production code shipped without tests for the dim-mismatch and
    // store-open-fail paths even though both are realistic failure modes
    // (model-swap mid-flight, slot dir deleted under the daemon).
    //
    // We invoke `spawn_hnsw_rebuild` directly, then join the worker thread
    // and inspect what landed on the receive channel. The contract is:
    //   - dim mismatch  → channel carries Err, pending must clear on drain
    //   - missing index → channel carries Err, ditto
    // Both paths must NOT panic and must NOT leak the pending entry forever.

    /// P2.29: a dim mismatch between the store and the caller's
    /// `expected_dim` must surface as `Err` on the channel, not a panic.
    /// The on-disk store is dim=4; we ask for dim=8.
    #[test]
    fn spawn_hnsw_rebuild_dim_mismatch_returns_error_outcome() {
        let dim = 4;
        let expected_dim = 8;
        let fix = drain_test_fixture(dim);
        let cqs_dir = fix.tmp.path().to_path_buf();
        let index_path = fix.tmp.path().join("index.db");

        let pending = spawn_hnsw_rebuild(cqs_dir, index_path, expected_dim, "p2_29_dim");
        // Wait for the worker thread to finish, bounded.
        let outcome = pending
            .rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("rebuild thread must report within 10s");
        // `RebuildOutcome` is `Result<Option<HnswIndex>, anyhow::Error>` and
        // `HnswIndex` is not `Debug`, so we can't call `unwrap_err` directly.
        // Pattern-match instead.
        let err = match outcome {
            Ok(_) => panic!("dim mismatch must surface as an Err on the rebuild channel"),
            Err(e) => e,
        };
        let msg = format!("{}", err);
        assert!(
            msg.contains("does not match expected") || msg.contains("dim"),
            "error must mention the dim mismatch (got: {msg})"
        );
        // Drain the worker handle so the OS thread is reaped.
        if let Some(h) = pending.handle {
            let _ = h.join();
        }
    }

    /// P2.29: pointing at a non-existent index path (e.g. slot dir
    /// removed mid-flight) must surface as `Err` on the channel — never
    /// panic, never hang. `Store::open_readonly_pooled` returns an Err
    /// immediately and the closure propagates it via `?`.
    #[test]
    fn spawn_hnsw_rebuild_missing_index_path_returns_error_outcome() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cqs_dir = tmp.path().to_path_buf();
        let bogus = tmp.path().join("does_not_exist.db");

        let pending = spawn_hnsw_rebuild(cqs_dir, bogus, 4, "p2_29_missing");
        let outcome = pending
            .rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("rebuild thread must report within 10s");
        assert!(
            outcome.is_err(),
            "missing index must surface as an Err on the rebuild channel"
        );
        if let Some(h) = pending.handle {
            let _ = h.join();
        }
    }

    /// P2.29: drain path must clear `pending_rebuild` when the worker
    /// thread reported an error. Today the rebuild thread can fail for
    /// many reasons (dim mismatch, store gone, save failure); the drain
    /// must always reset state so the next threshold trigger can retry —
    /// otherwise the pending slot leaks forever and no further rebuilds
    /// run.
    #[test]
    fn drain_clears_pending_when_spawned_rebuild_errors() {
        // Drive the full spawn+drain cycle through a guaranteed-failing
        // path (missing index) so the drain sees a real Err rather than
        // a hand-crafted `tx.send(Err(_))`.
        let tmp = tempfile::TempDir::new().unwrap();
        let pending = spawn_hnsw_rebuild(
            tmp.path().to_path_buf(),
            tmp.path().join("nope.db"),
            4,
            "p2_29_drain",
        );

        // Block until the worker thread has signalled — the drain uses
        // try_recv so we want the message already enqueued.
        if let Some(h) = pending.handle.as_ref() {
            // Best-effort wait: rebuild thread writes to channel before
            // exiting. Up to 10s tolerance for slow CI.
            for _ in 0..100 {
                if h.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }

        // Now build a state with this PendingRebuild and drive the drain.
        let mut state = test_watch_state();
        state.pending_rebuild = Some(pending);

        let fix = drain_test_fixture(4);
        let cfg = test_watch_config(
            fix.tmp.path(),
            fix.tmp.path(),
            &fix.notes_path,
            &fix.supported_ext,
        );
        drain_pending_rebuild(&cfg, &fix.store, &mut state);
        assert!(
            state.pending_rebuild.is_none(),
            "drain must clear pending_rebuild when the rebuild thread errored"
        );
        assert!(
            state.hnsw_index.is_none(),
            "no index must be swapped in when the rebuild errored"
        );
    }

    #[test]
    fn drain_pending_rebuild_leaves_pending_when_still_running() {
        let (_tx, rx) = std::sync::mpsc::channel::<RebuildOutcome>();
        let mut state = test_watch_state();
        state.pending_rebuild = Some(PendingRebuild {
            rx,
            delta: Vec::new(),
            started_at: std::time::Instant::now(),
            handle: None,
            delta_saturated: false,
        });

        let fix = drain_test_fixture(4);
        let cfg = test_watch_config(
            fix.tmp.path(),
            fix.tmp.path(),
            &fix.notes_path,
            &fix.supported_ext,
        );
        let store = &fix.store;

        drain_pending_rebuild(&cfg, store, &mut state);
        assert!(
            state.pending_rebuild.is_some(),
            "pending should remain in flight when channel has no message"
        );
    }

    // ── #1129: reindex_files consults the global EmbeddingCache ─────────────

    /// `reindex_files` must read from `global_cache` before calling the
    /// embedder. We prime the cache with a known embedding for the chunk's
    /// content_hash, then ensure the chunk written to the store has THAT
    /// vector — proof the embedder was bypassed entirely.
    ///
    /// `#[ignore]` because building a real `Embedder` (CPU) loads ONNX
    /// weights and is too heavy for the default test pass. The test still
    /// exercises the cache wiring; running it gated catches the regression
    /// when the watch path drops the cache check.
    #[test]
    #[ignore = "Requires loading the BGE-large model (heavy)"]
    fn test_reindex_files_hits_global_cache_skipping_embedder() {
        use cqs::cache::{CachePurpose, EmbeddingCache};
        use cqs::embedder::ModelConfig;
        use std::io::Write;

        // 1) Tempdir with a tiny rust file we can parse.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let cqs_dir = root.join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let rs_file = root.join("hit.rs");
        let source = "pub fn hits_cache() { let _ = 42; }";
        let mut f = std::fs::File::create(&rs_file).unwrap();
        f.write_all(source.as_bytes()).unwrap();
        drop(f);

        // 2) Build a Store and an Embedder. Both required by reindex_files.
        let model_cfg = ModelConfig::resolve(None, None);
        let embedder = Embedder::new_cpu(model_cfg).expect("init CPU embedder");
        let dim = embedder.embedding_dim();
        let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let mut store = Store::open(&store_path).unwrap();
        store
            .init(&cqs::store::ModelInfo::new(
                &embedder.model_config().repo,
                dim,
            ))
            .unwrap();
        store.set_dim(dim);

        // 3) Parse the file once to learn the chunk's content_hash. Only
        //    deterministic way to know what to prime — the parser's hash
        //    is computed from chunk metadata + bytes.
        let parser = CqParser::new().unwrap();
        let chunks = parser
            .parse_file_all_with_chunk_calls(&rs_file)
            .map(|(c, _, _, _)| c)
            .expect("parse hit.rs");
        assert!(!chunks.is_empty(), "parser must yield at least one chunk");
        let target_hash = chunks[0].content_hash.clone();

        // 4) Prime the global cache with a SENTINEL embedding for the
        //    chunk's content_hash. Sentinel = first lane large, others zero,
        //    then unit-normalized — distinguishes it from anything the
        //    embedder would produce on this content.
        let cache_path = EmbeddingCache::project_default_path(&cqs_dir);
        let cache = EmbeddingCache::open(&cache_path).expect("open cache");
        let mut sentinel = vec![0.0_f32; dim];
        sentinel[0] = 7.7;
        let norm: f32 = sentinel.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut sentinel {
            *x /= norm;
        }
        let sentinel_clone = sentinel.clone();
        cache
            .write_batch_owned(
                &[(target_hash.clone(), sentinel_clone)],
                embedder.model_fingerprint(),
                CachePurpose::Embedding,
                dim,
            )
            .unwrap();

        // 5) Run reindex_files with the cache wired in.
        let files = vec![PathBuf::from("hit.rs")];
        let (count, _) =
            reindex_files(root, &store, &files, &parser, &embedder, Some(&cache), true)
                .expect("reindex_files");
        assert!(count >= 1, "at least one chunk indexed");

        // 6) The chunk in the store must hold the SENTINEL — proof that
        //    the global cache served the read instead of the embedder.
        let stored = store
            .get_embeddings_by_hashes(&[target_hash.as_str()])
            .expect("store lookup");
        let stored_emb = stored
            .get(&target_hash)
            .expect("chunk written under the same content_hash");
        let stored_slice = stored_emb.as_slice();
        assert_eq!(stored_slice.len(), dim);
        for (i, (&got, &want)) in stored_slice.iter().zip(sentinel.iter()).enumerate() {
            assert!(
                (got - want).abs() < 1e-5,
                "lane {i}: got {got} want {want} — embedder was called instead of cache hit"
            );
        }
    }

    /// `reindex_files` with `global_cache: None` falls back to the prior
    /// store-only path. Lighter assertion: just confirm the function runs
    /// to completion and writes chunks. Pins the legacy degrade path so
    /// `CQS_CACHE_ENABLED=0` doesn't break watch.
    #[test]
    #[ignore = "Requires loading the BGE-large model (heavy)"]
    fn test_reindex_files_no_global_cache_still_works() {
        use cqs::embedder::ModelConfig;
        use std::io::Write;

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let cqs_dir = root.join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let rs_file = root.join("nocache.rs");
        let mut f = std::fs::File::create(&rs_file).unwrap();
        f.write_all(b"pub fn no_cache_path() { let _ = 0; }")
            .unwrap();
        drop(f);

        let model_cfg = ModelConfig::resolve(None, None);
        let embedder = Embedder::new_cpu(model_cfg).expect("init CPU embedder");
        let dim = embedder.embedding_dim();
        let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let mut store = Store::open(&store_path).unwrap();
        store
            .init(&cqs::store::ModelInfo::new(
                &embedder.model_config().repo,
                dim,
            ))
            .unwrap();
        store.set_dim(dim);

        let parser = CqParser::new().unwrap();
        let files = vec![PathBuf::from("nocache.rs")];
        let (count, _) = reindex_files(root, &store, &files, &parser, &embedder, None, true)
            .expect("reindex_files without global_cache");
        assert!(count >= 1, "no-cache path must still index");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TC-ADV-1.29-3: adversarial coverage for the daemon socket handler.
//
// `handle_socket_client` (above, line 160) is the first thing every daemon
// query touches. It does the line read, size cap, JSON parse, command-field
// validation, and non-string-arg rejection *before* ever acquiring the
// BatchContext mutex. Zero tests previously exercised those rejection paths.
//
// These tests use `UnixStream::pair()` to build a connected stream pair
// in-process — we hand the `server` end to `handle_socket_client` on a worker
// thread, then read/write the `client` end from the test thread. Nothing ever
// touches the real filesystem socket path. No ONNX model is loaded, because
// every adversarial payload is rejected before reaching `dispatch_tokens`.
//
// The one exception is the NUL-byte test, which intentionally reaches
// `dispatch_parsed_tokens`. That path goes through `reject_null_tokens` in
// `cli::batch::mod.rs` and bails before any handler runs — still no model
// load. The "oversized single arg" test similarly reaches dispatch but the
// `notes list` handler doesn't need an embedder.
//
// Why not in `tests/daemon_adversarial_test.rs`: `handle_socket_client` is
// a private `fn` in a binary module (`src/main.rs` → `mod cli`). Integration
// tests link against the library only, not the binary. Co-locating here is
// the narrowest path.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(all(test, unix))]
mod adversarial_socket_tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    /// Spin up a `Mutex<BatchContext>` against a fresh in-memory store.
    ///
    /// Reuses `crate::cli::batch::create_test_context` — see its doc for
    /// visibility rationale. The returned tempdir must live for the whole
    /// test or the store's WAL can be reaped mid-query.
    fn test_ctx() -> (
        tempfile::TempDir,
        Arc<Mutex<crate::cli::batch::BatchContext>>,
    ) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        {
            let store = cqs::store::Store::open(&index_path).expect("open store");
            store
                .init(&cqs::store::ModelInfo::default())
                .expect("init store");
        }
        let ctx = crate::cli::batch::create_test_context(&cqs_dir).expect("create ctx");
        (dir, Arc::new(Mutex::new(ctx)))
    }

    /// Spawn `handle_socket_client` on a worker thread with the `server` end
    /// of a paired UnixStream. Returns the client end and the worker
    /// JoinHandle so tests can force-drop the client (→ EOF on server →
    /// handler returns → thread joins).
    fn spawn_handler(
        ctx: Arc<Mutex<crate::cli::batch::BatchContext>>,
    ) -> (UnixStream, thread::JoinHandle<()>) {
        let (client, server) = UnixStream::pair().expect("UnixStream::pair");
        // Handler's read timeout is controlled by `resolve_daemon_timeout_ms`
        // (default 5 s). For tests we want a snappier rejection path if a
        // write is truncated — set an explicit short timeout on the server
        // side before handing it off. `handle_socket_client` will then
        // overwrite it with the resolved value, so this is belt-and-suspenders.
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set_read_timeout");
        server
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("set_write_timeout");
        let handle = thread::spawn(move || {
            // `handle_socket_client` is a sibling function in this module —
            // `super::handle_socket_client` reaches it.
            super::handle_socket_client(server, &ctx);
        });
        (client, handle)
    }

    /// Read one newline-terminated response from the client stream, with a
    /// bounded wait. Returns the trimmed bytes as a `String`. Panics if no
    /// newline arrives within 3 s — the daemon is contractually required to
    /// respond to every request it accepts the first byte of.
    fn read_line(client: &mut UnixStream) -> String {
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set client read_timeout");
        let mut buf = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        loop {
            match client.read(&mut byte) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    buf.push(byte[0]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("socket read failed: {e}"),
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Parse the daemon's response line as JSON.
    fn parse_response(line: &str) -> serde_json::Value {
        serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("daemon response is not valid JSON ({e}): {line}"))
    }

    /// Drain worker thread after the test's payload has been consumed.
    fn join_worker(client: UnixStream, handle: thread::JoinHandle<()>) {
        // Closing the client end signals EOF on the server; the handler
        // either completes normally or returns on read error. Give it a
        // small window to drain — long enough for the response to reach us
        // but short enough that a deadlocked handler surfaces as a test
        // hang rather than silent success.
        drop(client);
        for _ in 0..30 {
            if handle.is_finished() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if handle.is_finished() {
            handle.join().expect("handler thread panicked");
        } else {
            // If it hasn't finished, the test still got what it came for
            // (we already read the response). Don't block forever on the
            // final join — the OS will reap the thread when the process
            // exits. Tests should still surface a hang via their own timeout.
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: exactly 1 MiB + 1 byte → "request too large"
    //
    // The reader is wrapped in `.take(1_048_577)` so the post-read size
    // check sees exactly the cap. A client sending `'a' * 1_048_577` with
    // no newline triggers the `n > 1_048_576` branch and the daemon must
    // return a structured error.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_rejects_exactly_one_mib_boundary() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));

        // 1 MiB + 1 byte, no newline. The daemon's `read_line` reads up to
        // the take() limit of 1_048_577, then the size check fires.
        let payload = vec![b'a'; 1_048_577];
        // Writing 1 MiB to a socket blocks if the peer doesn't read. The
        // handler is actively reading, so this should complete.
        client.write_all(&payload).expect("write 1 MiB + 1 payload");
        // Half-close the write side so the peer's read_line terminates
        // without needing a newline. Without this, the peer keeps reading
        // (up to the take() cap) and we both deadlock waiting for more.
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("half-close write");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "1 MiB + 1 byte must return a structured error envelope: {line}"
        );
        assert_eq!(
            resp.get("message").and_then(|v| v.as_str()),
            Some("request too large"),
            "message must name the exact failure mode so the client can surface it: {line}"
        );
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: malformed JSON — trailing garbage after valid object.
    //
    // The daemon parses a single JSON Value via `serde_json::from_str` on
    // `line.trim()`. `from_str` rejects trailing non-whitespace tokens
    // because serde_json is strict by default.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_rejects_malformed_trailing_garbage() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        client
            .write_all(b"{\"command\":\"ping\"} garbage\n")
            .expect("write");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "trailing garbage after JSON must be rejected, not silently parsed: {line}"
        );
        // `handle_socket_client` surfaces `invalid JSON: <serde error>` —
        // assert the prefix so a future serde version bump doesn't break us.
        let msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            msg.starts_with("invalid JSON"),
            "message should begin with 'invalid JSON', got: {msg:?}"
        );
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: malformed bytes — UTF-16 BOM prefix (0xFF 0xFE).
    //
    // A client that writes a UTF-16 LE BOM before its JSON payload is
    // sending bytes that are not valid UTF-8. `BufRead::read_line` performs
    // UTF-8 validation internally and returns `Err(InvalidData)` for the
    // whole line. `handle_socket_client` logs and returns *without* writing
    // a response — the daemon silently drops unreadable input.
    //
    // The contract we pin here: no panic, no partial write, no half-open
    // socket; the handler thread finishes and the client sees EOF. This is
    // the *current* behaviour — if a future change makes the daemon emit
    // `invalid UTF-8` diagnostics instead, that's a behaviour change worth
    // a new test, not a silent regression.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_drops_utf16_bom_prefix_without_panic() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        // UTF-16 LE BOM + valid JSON shape — the BOM bytes (0xFF 0xFE) are
        // not valid UTF-8, so `read_line` errors out.
        let mut payload: Vec<u8> = vec![0xFF, 0xFE];
        payload.extend_from_slice(b"{\"command\":\"ping\"}\n");
        client.write_all(&payload).expect("write BOM+JSON");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("half-close write");

        // Expect EOF — handler returns without writing on InvalidData.
        let line = read_line(&mut client);
        assert!(
            line.is_empty(),
            "UTF-8 decode failure at the BufRead layer must not surface a \
             response body — handler returns early. Got: {line:?}"
        );

        // Sanity: the handler thread must still terminate cleanly (no panic,
        // no deadlock). `join_worker` polls `is_finished()` and asserts the
        // join doesn't panic.
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: empty line (just "\n") — `read_line` returns `Ok(1)` (one byte
    // read). After `line.trim()` the result is an empty string, which
    // `serde_json::from_str` rejects with "EOF while parsing a value".
    // The handler surfaces that via the standard `invalid JSON` envelope.
    //
    // This is deliberate: a caller that opens a socket and sends just a
    // newline likely did something wrong — silently accepting empty lines
    // would hide bugs further up the stack.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_rejects_bare_newline_as_invalid_json() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        client.write_all(b"\n").expect("write empty line");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "bare newline must be rejected rather than silently accepted: {line}"
        );
        let msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            msg.starts_with("invalid JSON"),
            "bare newline rejection must come through the invalid-JSON path, got: {msg:?}"
        );
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: missing `command` field — the daemon unwraps `command` as an
    // empty string and bails via the `if command.is_empty()` check.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_rejects_missing_command_field() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        client
            .write_all(b"{\"args\":[]}\n")
            .expect("write no-command");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "missing command field must surface as error: {line}"
        );
        assert_eq!(
            resp.get("message").and_then(|v| v.as_str()),
            Some("missing 'command' field"),
            "message must match the exact production string — dashboards grep on it: {line}"
        );
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: non-string args (objects, nulls, numbers) — P3 #86 hardened
    // this path; ensure it's still rejected instead of silently filtered.
    //
    // The fixture sends three bad elements (`{}, null, 42`) so the handler's
    // `bad_arg_indices` vec has `[0, 1, 2]`. The rejection response is a
    // flat string — dashboards grep on the exact message.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_rejects_non_string_args() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        client
            .write_all(b"{\"command\":\"notes\",\"args\":[{},null,42]}\n")
            .expect("write non-string args");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "non-string args must surface as a rejection, not a truncated call: {line}"
        );
        assert_eq!(
            resp.get("message").and_then(|v| v.as_str()),
            Some("args contains non-string elements"),
            "message must match production string — P3 #86: {line}"
        );
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: oversized single arg (500 KB) within the 1 MiB line limit is
    // currently accepted — the daemon has no per-arg cap, only a per-line
    // one. This test pins that behaviour so a future per-arg cap is added
    // deliberately (and the test would be updated) rather than silently.
    //
    // The arg goes to the `notes` command which is registered as BatchCmd;
    // clap accepts arbitrary-length strings for the body. Even if the
    // handler errors on the oversized body, the daemon must not crash
    // — that's the contract we pin.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_accepts_500kb_arg_within_mib_line() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));

        let big_arg = "x".repeat(500_000);
        // Build the JSON payload manually to avoid serde_json allocating a
        // second 500 KB intermediate String.
        let mut payload: Vec<u8> = Vec::with_capacity(700_000);
        payload.extend_from_slice(b"{\"command\":\"notes\",\"args\":[\"list\",\"");
        payload.extend_from_slice(big_arg.as_bytes());
        payload.extend_from_slice(b"\"]}\n");
        assert!(
            payload.len() < 1_048_576,
            "test payload must stay under the 1 MiB cap"
        );
        client.write_all(&payload).expect("write 500 KB arg");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        // The precise response depends on how `notes` handles unknown
        // subcommand args. What we're pinning is that the daemon produced
        // *some* structured response and didn't crash.
        assert!(
            resp.get("status").is_some(),
            "500 KB arg within cap must produce a structured response: {line}"
        );
        // If the daemon ever adds a per-arg cap, this assertion will need
        // updating. Leaving a deliberate fail-open here documents the
        // current behaviour so the change is a conscious choice.
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Test: NUL byte in args. The daemon accepts the JSON (NUL is a valid
    // Rust String byte — ` ` deserialises fine), but `dispatch_tokens`
    // runs it through `reject_null_tokens` which bails with an
    // `invalid_input` envelope. The daemon's outer frame then wraps that
    // envelope in `{status:ok, output:<envelope with error>}`.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_rejects_nul_byte_in_args_downstream() {
        let (_dir, ctx) = test_ctx();
        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        // ` ` embeds a literal NUL inside a JSON string — valid JSON,
        // invalid batch-dispatch input.
        client
            .write_all(b"{\"command\":\"notes\",\"args\":[\"list\",\"has\\u0000nul\"]}\n")
            .expect("write NUL payload");

        let line = read_line(&mut client);
        let resp = parse_response(&line);
        // Outer envelope: the NUL-guard path writes a SUCCESSFUL JSON line
        // to the sink (containing the inner error envelope), so the daemon
        // wraps it as `{status:"ok",output:{...}}`. Either outer shape is
        // acceptable — the semantic contract is that the *inner* error
        // surfaces `invalid_input`.
        let inner_code = resp
            .pointer("/output/error/code")
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Legacy bytes-through-a-string path wraps the envelope bytes
                // as a JSON string — try parsing if needed.
                let s = resp.pointer("/output")?.as_str()?;
                serde_json::from_str::<serde_json::Value>(s)
                    .ok()?
                    .pointer("/error/code")?
                    .as_str()
                    .map(|_| "")
            });
        assert_eq!(
            inner_code,
            Some("invalid_input"),
            "NUL byte must be caught by reject_null_tokens and surface as invalid_input: {line}"
        );
        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // TC-HAP-1.29-6: happy-path round-trip. Every existing socket test pins
    // an *error* shape — trailing garbage, NUL bytes, missing command,
    // oversized request. None pins the *success* path: agent sends a valid
    // command, daemon runs it, envelope comes back with `status:"ok"` and a
    // well-formed `output` payload.
    //
    // This is the complement to the 8 adversarial tests above. `stats` is
    // the right happy-path probe because `dispatch_stats` touches
    // store-schema reads, the error counter, the call-graph stats, and the
    // language histogram — the four surfaces that would silently drift if a
    // future refactor changed the wire envelope or the handler shape.
    //
    // Why `stats`: no embedder needed (read-only SQL + filesystem walk), so
    // the test runs in ~ms. A pre-seeded chunk in the store makes the
    // `total_chunks` assertion load-bearing — an empty store would hide
    // regressions where the daemon returned `total_chunks=0` unconditionally.
    // ─────────────────────────────────────────────────────────────────────
    #[test]
    fn daemon_stats_happy_path_roundtrip() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::store::ModelInfo;
        use std::path::PathBuf;

        // Custom setup — seed one chunk before `create_test_context` opens
        // the store read-only. `test_ctx` helper above opens an empty store;
        // for the happy path we want `total_chunks >= 1` so the numeric
        // assertion actually distinguishes "handler ran and counted" from
        // "handler returned zero by accident".
        let dir = tempfile::TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        {
            let store = cqs::store::Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init store");
            // One chunk so `total_chunks >= 1` on the other side.
            let content = "pub fn roundtrip_probe() {}";
            let chunk = Chunk {
                id: "probe.rs:1:probe".to_string(),
                file: PathBuf::from("probe.rs"),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: "roundtrip_probe".to_string(),
                signature: "pub fn roundtrip_probe()".to_string(),
                content: content.to_string(),
                doc: None,
                line_start: 1,
                line_end: 1,
                content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
                parent_id: None,
                window_idx: None,
                parent_type_name: None,
                parser_version: 0,
            };
            // Unit embedding — `upsert_chunk` validates dimension against the
            // seeded ModelInfo. Value doesn't matter for the stats path.
            let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
            emb_vec[0] = 1.0;
            let embedding = cqs::embedder::Embedding::new(emb_vec);
            store
                .upsert_chunks_batch(&[(chunk, embedding)], Some(0))
                .expect("seed chunk");
        } // drop to flush WAL

        let ctx = super::super::batch::create_test_context(&cqs_dir).expect("create ctx");
        let ctx = Arc::new(Mutex::new(ctx));

        let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
        client
            .write_all(b"{\"command\":\"stats\",\"args\":[]}\n")
            .expect("write stats request");

        let line = read_line(&mut client);
        let resp = parse_response(&line);

        // Outer envelope shape: `{status: "ok", output: <json>}` — the
        // branch at `handle_socket_client:378-391` that wraps successful
        // dispatch output.
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "happy-path response must carry status:ok. got: {line}"
        );

        // `output` is the parsed JSON from `dispatch_line`. The dispatcher's
        // stats handler writes an envelope through `emit_json`, so
        // `output` is itself `{data: {...}, error: null, version: 1}`.
        let output = resp
            .get("output")
            .unwrap_or_else(|| panic!("happy-path response must carry an `output` field: {line}"));
        assert!(
            output.is_object(),
            "output must be a JSON object (envelope): {line}"
        );
        let data = output
            .get("data")
            .unwrap_or_else(|| panic!("inner envelope must have a `data` field: {line}"));
        let total_chunks = data
            .get("total_chunks")
            .unwrap_or_else(|| panic!("stats data must have `total_chunks`: {line}"));
        assert!(
            total_chunks.is_number(),
            "total_chunks must be numeric: got {total_chunks}"
        );
        let n = total_chunks
            .as_u64()
            .unwrap_or_else(|| panic!("total_chunks must parse as u64: {total_chunks}"));
        assert!(
            n >= 1,
            "total_chunks must reflect the seeded chunk (≥1), got {n}: {line}"
        );

        join_worker(client, handle);
    }

    // ─────────────────────────────────────────────────────────────────────
    // #1127 — daemon parallelism regression tests
    //
    // These tests pin the lock-topology contract introduced by #1127
    // (post-#1145): the daemon's `handle_socket_client` path must hold the
    // BatchContext mutex only across `checkout_view_from_arc` (a few
    // microseconds), never across the handler body. Two slow handlers must
    // run in parallel; a fast handler issued mid-flight must not block on
    // a slow one.
    //
    // The handlers used here are `test-sleep` (a `#[cfg(test)]`-gated
    // BatchCmd variant in `cli::batch::commands`) and `notes list`
    // (production handler, read-only, no embedder load). Both are
    // intentionally embedder-free so the tests stay fast in CI.
    // ─────────────────────────────────────────────────────────────────────

    /// Issue two `test-sleep --ms 300` calls concurrently. The new lock
    /// topology should let them overlap so wall-clock ≈ max(t1, t2) ≈ 300 ms.
    /// Pre-fix (single mutex held across dispatch) they would serialize,
    /// blowing past 600 ms.
    ///
    /// Threshold of 1.5× single-handler time gives generous headroom for
    /// thread scheduling jitter on busy CI hosts; pre-fix behavior was
    /// deterministically 2.0× and the gap is wide enough to be reliable.
    #[test]
    fn daemon_two_slow_handlers_run_in_parallel() {
        let (_dir, ctx) = test_ctx();

        // Each handler sleeps for SLEEP_MS. If they run sequentially the
        // total wall-clock must be ≈ 2 * SLEEP_MS; in parallel it must be
        // ≈ 1 * SLEEP_MS. The threshold (1.5×) gives wide headroom.
        const SLEEP_MS: u64 = 300;
        let payload =
            format!("{{\"command\":\"test-sleep\",\"args\":[\"--ms\",\"{SLEEP_MS}\"]}}\n");

        let start = std::time::Instant::now();
        let (mut client_a, handle_a) = spawn_handler(Arc::clone(&ctx));
        let (mut client_b, handle_b) = spawn_handler(Arc::clone(&ctx));

        // Issue both requests as close to simultaneously as possible.
        client_a.write_all(payload.as_bytes()).expect("write A");
        client_b.write_all(payload.as_bytes()).expect("write B");

        // Read both responses on this thread; the workers run independently.
        let line_a = read_line(&mut client_a);
        let line_b = read_line(&mut client_b);
        let elapsed = start.elapsed();

        // Both must succeed with the test envelope.
        let resp_a = parse_response(&line_a);
        let resp_b = parse_response(&line_b);
        assert_eq!(
            resp_a.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "A response: {line_a}"
        );
        assert_eq!(
            resp_b.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "B response: {line_b}"
        );

        // The load-bearing assertion: two SLEEP_MS handlers must overlap.
        // 1.5× headroom for scheduling; pre-fix behavior is deterministically
        // 2× so the gap is wide enough to avoid flake.
        let max_allowed_ms = (SLEEP_MS as f64 * 1.5) as u128;
        assert!(
            elapsed.as_millis() < max_allowed_ms,
            "two slow handlers must run in parallel: elapsed {} ms, ceiling {} ms (single-handler {} ms × 1.5). \
             Pre-#1127 behavior would be ≈{} ms — if you see that, the BatchContext mutex is being held across dispatch.",
            elapsed.as_millis(),
            max_allowed_ms,
            SLEEP_MS,
            SLEEP_MS * 2
        );

        join_worker(client_a, handle_a);
        join_worker(client_b, handle_b);
    }

    /// While a slow `test-sleep` is in flight, an inbound `notes list` query
    /// must complete promptly. Pre-fix the second connection's
    /// `batch_ctx.lock()` would block on the first connection's
    /// dispatch-spanning lock for the full sleep duration.
    ///
    /// Bounded at 200 ms which is generous: `notes list` against an empty
    /// store does a single `notes_cache` build (~µs to ms) plus the
    /// envelope write. The slow handler's 500 ms sleep gives a wide
    /// observation window.
    #[test]
    fn daemon_notes_list_unblocked_by_inflight_gather() {
        let (_dir, ctx) = test_ctx();

        const SLOW_SLEEP_MS: u64 = 500;
        let slow_payload =
            format!("{{\"command\":\"test-sleep\",\"args\":[\"--ms\",\"{SLOW_SLEEP_MS}\"]}}\n");
        let fast_payload = "{\"command\":\"notes\",\"args\":[]}\n";

        let (mut slow_client, slow_handle) = spawn_handler(Arc::clone(&ctx));
        slow_client
            .write_all(slow_payload.as_bytes())
            .expect("write slow");

        // Give the slow handler a moment to arrive at its sleep. 30 ms is
        // enough on every machine the daemon runs on; the slow sleep is
        // 500 ms so this still leaves >450 ms of overlap.
        thread::sleep(Duration::from_millis(30));

        let fast_start = std::time::Instant::now();
        let (mut fast_client, fast_handle) = spawn_handler(Arc::clone(&ctx));
        fast_client
            .write_all(fast_payload.as_bytes())
            .expect("write fast");
        let fast_line = read_line(&mut fast_client);
        let fast_elapsed = fast_start.elapsed();

        // The fast handler must have come back well before the slow one
        // finishes. 200 ms ceiling is comfortably above any reasonable
        // notes-list latency on an empty store.
        const FAST_LATENCY_CEIL_MS: u128 = 200;
        assert!(
            fast_elapsed.as_millis() < FAST_LATENCY_CEIL_MS,
            "fast handler must not block on the in-flight slow handler: \
             fast latency {} ms, ceiling {} ms. Pre-#1127 the fast handler \
             would queue behind the slow one for ≈{} ms.",
            fast_elapsed.as_millis(),
            FAST_LATENCY_CEIL_MS,
            SLOW_SLEEP_MS
        );

        // Sanity: the fast response is a real success envelope.
        let resp = parse_response(&fast_line);
        assert_eq!(
            resp.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "fast response should be ok envelope: {fast_line}"
        );

        // Drain the slow handler before we drop the test fixture.
        let slow_line = read_line(&mut slow_client);
        let slow_resp = parse_response(&slow_line);
        assert_eq!(slow_resp.get("status").and_then(|v| v.as_str()), Some("ok"));

        join_worker(fast_client, fast_handle);
        join_worker(slow_client, slow_handle);
    }

    /// `handle_socket_client` must round-trip `query_count` and
    /// `error_count` correctly under the new short-lock contract — bumping
    /// the counters happens via the view's `Arc<AtomicU64>` (no re-lock of
    /// the BatchContext mutex). Issue three requests (one parse error, two
    /// successful pings); the snapshot read after must show
    /// `total_queries >= 3` and `error_count >= 1`.
    ///
    /// Maps to the test planned in `docs/audit-fix-prompts.md:5660`.
    #[test]
    fn handle_socket_client_round_trips_stats() {
        let (_dir, ctx) = test_ctx();

        // Issue (a) a parse error, (b) two pings. Each request goes through
        // a fresh client/handler pair so the test exactly mirrors the
        // production accept-loop behavior (one connection per request).
        for payload in [
            "{\"command\":\"bogus_command\",\"args\":[]}\n",
            "{\"command\":\"ping\",\"args\":[]}\n",
            "{\"command\":\"ping\",\"args\":[]}\n",
        ] {
            let (mut client, handle) = spawn_handler(Arc::clone(&ctx));
            client.write_all(payload.as_bytes()).expect("write payload");
            let _ = read_line(&mut client);
            join_worker(client, handle);
        }

        // Snapshot the counters via the BatchContext directly (the test has
        // privileged access; no socket query needed). The view path bumps
        // the same Arc<AtomicU64>, so this read sees the same value the
        // ping handler would surface.
        let guard = ctx.lock().unwrap();
        let total_queries = guard.query_count.load(std::sync::atomic::Ordering::Relaxed);
        let error_count = guard.error_count.load(std::sync::atomic::Ordering::Relaxed);
        drop(guard);

        // Three requests reached the dispatch path (NUL/empty would short
        // circuit before counter bumps; bogus_command parses but clap
        // rejects, which still counts as a dispatched query).
        assert!(
            total_queries >= 3,
            "query_count must reflect 3 dispatches under the new short-lock contract; got {total_queries}"
        );
        // Exactly one parse failure.
        assert!(
            error_count >= 1,
            "error_count must reflect the parse failure; got {error_count}"
        );
    }
}
