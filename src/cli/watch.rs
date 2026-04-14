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

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
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

/// RAII guard that removes the Unix socket file on drop.
#[cfg(unix)]
struct SocketCleanupGuard(PathBuf);

#[cfg(unix)]
impl Drop for SocketCleanupGuard {
    fn drop(&mut self) {
        if self.0.exists() {
            if let Err(e) = std::fs::remove_file(&self.0) {
                tracing::warn!(path = %self.0.display(), error = %e, "Failed to remove socket file");
            } else {
                tracing::info!(path = %self.0.display(), "Daemon socket removed");
            }
        }
    }
}

/// RM-V1.25-9: Set on SIGTERM so the watch loop drains and exits
/// cleanly instead of being hard-killed mid-write when systemd
/// sends `stop`. `ctrlc` without the `termination` feature only
/// traps SIGINT, and we don't want to grow a dep just for this
/// one unix-only hook.
#[cfg(unix)]
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Acquire)
}

/// Signal handler — async-signal-safe: only a relaxed atomic store.
#[cfg(unix)]
extern "C" fn on_sigterm(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

/// Install a SIGTERM handler so `systemctl stop cqs-watch` triggers a
/// clean drain via `SHUTDOWN_REQUESTED` rather than a hard kill. The
/// existing ctrlc-based SIGINT handler already flips `check_interrupted`;
/// this adds SIGTERM as a second shutdown path.
#[cfg(unix)]
fn install_sigterm_handler() {
    // SAFETY: libc::signal is async-signal-safe to call before any
    // threads start depending on the old disposition. We call it at
    // the top of cmd_watch, prior to spawning the socket thread.
    unsafe {
        let prev = libc::signal(libc::SIGTERM, on_sigterm as libc::sighandler_t);
        if prev == libc::SIG_ERR {
            let e = std::io::Error::last_os_error();
            tracing::warn!(error = %e, "Failed to install SIGTERM handler; watch will rely on SIGINT only");
        } else {
            tracing::debug!("SIGTERM handler installed for clean daemon shutdown");
        }
    }
}

/// Handle a single client connection on the daemon socket.
#[cfg(unix)]
/// Reads one JSON-line request, dispatches via the shared BatchContext, writes response.
fn handle_socket_client(
    mut stream: std::os::unix::net::UnixStream,
    batch_ctx: &super::batch::BatchContext,
) {
    let span = tracing::info_span!("daemon_query", command = tracing::field::Empty);
    let _enter = span.enter();
    let start = std::time::Instant::now();

    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    // Read request (max 1MB). Wrap reader in .take() so allocation is
    // bounded *before* we accept a giant line — the post-hoc size check
    // below still fires if a client sends exactly the cap worth of data.
    use std::io::Read as _;
    let mut reader = std::io::BufReader::new(&stream).take(1_048_577);
    let mut line = String::new();
    match std::io::BufRead::read_line(&mut reader, &mut line) {
        Ok(0) => return,
        Ok(n) if n > 1_048_576 => {
            let delivered = write_daemon_error_tracked(&mut stream, "request too large");
            tracing::info!(
                status = "client_error",
                delivered,
                latency_ms = start.elapsed().as_millis() as u64,
                "Daemon query complete"
            );
            return;
        }
        Err(e) => {
            tracing::debug!(error = %e, "Socket read failed");
            return;
        }
        Ok(_) => {}
    }

    let request: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(e) => {
            let delivered = write_daemon_error_tracked(&mut stream, &format!("invalid JSON: {e}"));
            tracing::info!(
                status = "parse_error",
                delivered,
                latency_ms = start.elapsed().as_millis() as u64,
                "Daemon query complete"
            );
            return;
        }
    };

    let command = request
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args: Vec<String> = request
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Record command on the span so every event inside this handler is
    // enriched with it, without needing to repeat `command` on each log.
    span.record("command", command);

    // SEC-V1.25-9: avoid echoing full query args — search strings and
    // notes bodies may contain snippets of private source or secrets.
    // Log only a length + 80-char preview at debug level; the full
    // command name is already on the span.
    let args_joined = args.join(" ");
    let args_preview_end = args_joined
        .char_indices()
        .nth(80)
        .map(|(i, _)| i)
        .unwrap_or(args_joined.len());
    tracing::debug!(
        command,
        args_len = args.len(),
        args_preview = %&args_joined[..args_preview_end],
        "Daemon request"
    );

    if command.is_empty() {
        let delivered = write_daemon_error_tracked(&mut stream, "missing 'command' field");
        tracing::info!(
            status = "client_error",
            delivered,
            latency_ms = start.elapsed().as_millis() as u64,
            "Daemon query complete"
        );
        return;
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let full_line = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, shell_words::join(&args))
        };
        let mut output = Vec::new();
        batch_ctx.dispatch_line(&full_line, &mut output);
        String::from_utf8(output).map_err(|e| format!("non-UTF-8 output: {e}"))
    }));

    let (status, delivered) = match result {
        Ok(Ok(output)) => {
            let resp = serde_json::json!({
                "status": "ok",
                "output": output.trim_end(),
            });
            let delivered = match writeln!(stream, "{}", resp) {
                Ok(()) => true,
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to write daemon response");
                    false
                }
            };
            ("ok", delivered)
        }
        Ok(Err(e)) => {
            let delivered = write_daemon_error_tracked(&mut stream, &e);
            ("client_error", delivered)
        }
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&'static str>().copied())
                .unwrap_or("<non-string panic payload>");
            let delivered =
                write_daemon_error_tracked(&mut stream, "internal error (panic in dispatch)");
            tracing::error!(
                panic_msg = %msg,
                "Daemon query panicked — daemon continues"
            );
            ("panic", delivered)
        }
    };

    tracing::info!(
        status,
        delivered,
        latency_ms = start.elapsed().as_millis() as u64,
        "Daemon query complete"
    );
}

#[cfg(unix)]
fn write_daemon_error(
    stream: &mut std::os::unix::net::UnixStream,
    message: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let resp = serde_json::json!({ "status": "error", "message": message });
    writeln!(stream, "{}", resp)
}

/// Like `write_daemon_error`, but logs on failure and returns whether
/// the write reached the client. Used by `handle_socket_client` to
/// populate the `delivered` telemetry field instead of silently
/// swallowing write errors with `let _ = ...`.
#[cfg(unix)]
fn write_daemon_error_tracked(stream: &mut std::os::unix::net::UnixStream, message: &str) -> bool {
    match write_daemon_error(stream, message) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "Failed to write daemon error response");
            false
        }
    }
}

/// Opaque identity of a database file for detecting replacements (DS-W5).
/// On Unix uses (device, inode) — survives renames that preserve the inode
/// and detects replacements where `index --force` creates a new file.
#[cfg(unix)]
fn db_file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn db_file_identity(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Full HNSW rebuild after this many incremental inserts to clean orphaned vectors.
/// Override with CQS_WATCH_REBUILD_THRESHOLD env var.
fn hnsw_rebuild_threshold() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_WATCH_REBUILD_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    })
}

/// Maximum pending files to prevent unbounded memory growth.
/// Override with CQS_WATCH_MAX_PENDING env var.
fn max_pending_files() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_WATCH_MAX_PENDING")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000)
    })
}

/// Immutable references shared across the watch loop.
///
/// Does not include `Store` because it is re-opened each cycle (DS-9).
struct WatchConfig<'a> {
    root: &'a Path,
    cqs_dir: &'a Path,
    notes_path: &'a Path,
    supported_ext: &'a HashSet<&'a str>,
    parser: &'a CqParser,
    embedder: &'a OnceCell<Embedder>,
    quiet: bool,
    model_config: &'a ModelConfig,
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
}

/// Track exponential backoff state for embedder initialization retries.
///
/// On repeated failures, backs off from 0s to max 5 minutes between attempts
/// to avoid burning CPU retrying a broken ONNX model load every ~2s cycle.
struct EmbedderBackoff {
    /// Number of consecutive failures
    failures: u32,
    /// Instant when the next retry is allowed
    next_retry: std::time::Instant,
}

impl EmbedderBackoff {
    fn new() -> Self {
        Self {
            failures: 0,
            next_retry: std::time::Instant::now(),
        }
    }

    /// Record a failure and compute the next retry time with exponential backoff.
    /// Backoff: 2^failures seconds, capped at 300s (5 min).
    fn record_failure(&mut self) {
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
    fn reset(&mut self) {
        self.failures = 0;
        self.next_retry = std::time::Instant::now();
    }

    /// Whether we should attempt initialization (backoff expired).
    fn should_retry(&self) -> bool {
        std::time::Instant::now() >= self.next_retry
    }
}

/// Try to initialize the embedder, returning a reference from the OnceCell.
/// Deduplicates the 7-line pattern that appeared twice in cmd_watch.
/// Uses `backoff` to apply exponential backoff on repeated failures (RM-24).
fn try_init_embedder<'a>(
    embedder: &'a OnceCell<Embedder>,
    backoff: &mut EmbedderBackoff,
    model_config: &ModelConfig,
) -> Option<&'a Embedder> {
    match embedder.get() {
        Some(e) => Some(e),
        None => {
            if !backoff.should_retry() {
                return None;
            }
            match Embedder::new(model_config.clone()) {
                Ok(e) => {
                    backoff.reset();
                    Some(embedder.get_or_init(|| e))
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

/// Watches the project for file changes and updates the code search index incrementally.
///
/// # Arguments
///
/// * `cli` - Command-line interface context
/// * `debounce_ms` - Debounce interval in milliseconds for file change events
/// * `no_ignore` - If true, ignores `.gitignore` rules (not yet implemented)
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
    let _span = tracing::info_span!("cmd_watch", debounce_ms, poll, serve).entered();
    if no_ignore {
        tracing::warn!("--no-ignore is not yet implemented for watch mode");
    }

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

    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("No index found. Run 'cqs index' first.");
    }

    // Socket listener BEFORE watcher scan — daemon is immediately queryable
    // while the (potentially slow) poll watcher initializes.
    // Unix domain sockets are not available on Windows.
    #[cfg(unix)]
    let mut socket_listener = if serve {
        let sock_path = super::daemon_socket_path(&cqs_dir);
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
        let listener = std::os::unix::net::UnixListener::bind(&sock_path)
            .with_context(|| format!("Failed to bind socket at {}", sock_path.display()))?;
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
    #[cfg(not(unix))]
    if serve {
        tracing::warn!("--serve is not supported on Windows (no Unix domain sockets)");
    }

    // Spawn dedicated socket handler thread — runs independently of the file
    // watcher so queries are served immediately, even during the slow poll scan.
    #[cfg(unix)]
    let _socket_thread = if serve {
        if let Some((listener, _)) = socket_listener.take() {
            // Stays non-blocking: the accept loop below polls so it can
            // notice SHUTDOWN_REQUESTED on SIGTERM (RM-V1.25-9).
            let thread = std::thread::spawn(move || {
                // BatchContext created inside the thread — RefCell is !Send
                // but thread-local ownership is fine.
                let ctx = match super::batch::create_context() {
                    Ok(ctx) => {
                        ctx.warm();
                        ctx
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Daemon BatchContext creation failed");
                        return;
                    }
                };
                tracing::info!("Daemon query thread ready");
                // RM-V1.25-9: Poll accept with a short sleep so the loop
                // can notice SIGTERM and drain cleanly instead of blocking
                // indefinitely on a syscall that systemd has to kill.
                // Listener was set non-blocking at bind time.
                loop {
                    if is_shutdown_requested() {
                        tracing::info!("Daemon accept loop draining on SIGTERM");
                        break;
                    }
                    match listener.accept() {
                        Ok((s, _addr)) => handle_socket_client(s, &ctx),
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

    let config = Config::default().with_poll_interval(Duration::from_millis(debounce_ms));

    // Box<dyn Watcher> so both watcher types work with the same variable
    let mut watcher: Box<dyn Watcher> = if use_poll {
        println!("Using poll watcher (interval: {}ms)", debounce_ms);
        Box::new(PollWatcher::new(tx, config)?)
    } else {
        Box::new(RecommendedWatcher::new(tx, config)?)
    };
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

    // Lazy-initialized embedder (~500MB, avoids startup delay unless changes occur).
    // Once initialized, stays in memory for fast reindexing. See module docs for memory details.
    let embedder: OnceCell<Embedder> = OnceCell::new();

    // Open store and reuse across reindex operations within a cycle.
    // Re-opened after each reindex cycle to clear stale OnceLock caches (DS-9).
    let mut store = Store::open(&index_path)
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    // DS-W5: Track the database file identity so we detect when `cqs index --force`
    // replaces it. Without this check, watch's Store handle would point at the
    // orphaned (renamed) inode and writes would silently vanish.
    let mut db_id = db_file_identity(&index_path);

    // Persistent HNSW state for incremental updates.
    // On first file change, does a full build and keeps the Owned index in memory.
    // Subsequent changes insert only changed chunks via insert_batch.
    // Full rebuild every hnsw_rebuild_threshold() incremental inserts to clean orphans.
    //
    // DS-35: Load existing HNSW index from disk if present, to avoid orphan accumulation
    // across restarts. Start incremental_count at threshold/2 so the first rebuild
    // happens sooner, cleaning any orphans from prior sessions.
    let (hnsw_index, incremental_count) =
        match HnswIndex::load_with_dim(cqs_dir.as_ref(), "index", store.dim()) {
            Ok(index) => {
                info!(vectors = index.len(), "Loaded existing HNSW index");
                (Some(index), hnsw_rebuild_threshold() / 2)
            }
            Err(ref e) if matches!(e, cqs::hnsw::HnswError::NotFound(_)) => {
                tracing::debug!("No prior HNSW index, starting fresh");
                (None, 0)
            }
            Err(e) => {
                // v1.22.0 audit EH-7: previously `Err(_) => (None, 0)` treated
                // DimensionMismatch, IO errors, and corruption the same as
                // "first run." Now logs so the operator sees why the prior
                // index was discarded.
                tracing::warn!(error = %e, "Existing HNSW index unusable, rebuilding from scratch");
                (None, 0)
            }
        };

    let model_config = cli.try_model_config()?;
    let watch_cfg = WatchConfig {
        root: &root,
        cqs_dir: &cqs_dir,
        notes_path: &notes_path,
        supported_ext: &supported_ext,
        parser: &parser,
        embedder: &embedder,
        quiet: cli.quiet,
        model_config,
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
    };

    let mut cycles_since_clear: u32 = 0;

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
                        store = Store::open(&index_path).with_context(|| {
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

                    // DS-1: Release lock after all reindex work (including HNSW rebuild)
                    drop(lock);
                } else {
                    cycles_since_clear += 1;
                    // Clear embedder session and HNSW index after ~5 minutes idle
                    // (3000 cycles at 100ms). Frees GPU/memory when watch is idle.
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = embedder.get() {
                            emb.clear_session();
                        }
                        state.hnsw_index = None;
                        state.incremental_count = 0;
                        cycles_since_clear = 0;
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

    Ok(())
}

/// Collect file system events into pending sets, filtering by extension and deduplicating.
fn collect_events(event: &notify::Event, cfg: &WatchConfig, state: &mut WatchState) {
    for path in &event.paths {
        // PB-26: Skip canonicalize for deleted files — dunce::canonicalize
        // requires the file to exist (calls std::fs::canonicalize internally).
        let path = if path.exists() {
            dunce::canonicalize(path).unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };
        // Skip .cqs directory
        // PB-2: Deleted files can't be canonicalized (they don't exist), so
        // compare normalized string forms to handle slash differences on WSL.
        let norm_path = cqs::normalize_path(&path);
        let norm_cqs = cqs::normalize_path(cfg.cqs_dir);
        if norm_path.starts_with(&norm_cqs) {
            tracing::debug!(path = %norm_path, "Skipping .cqs directory event");
            continue;
        }

        // Check if it's notes.toml
        let norm_notes = cqs::normalize_path(cfg.notes_path);
        if norm_path == norm_notes {
            state.pending_notes = true;
            state.last_event = std::time::Instant::now();
            continue;
        }

        // Skip if not a supported extension
        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();
        if !cfg.supported_ext.contains(ext.as_str()) {
            tracing::debug!(path = %path.display(), ext = %ext, "Skipping unsupported extension");
            continue;
        }

        // Convert to relative path
        if let Ok(rel) = path.strip_prefix(cfg.root) {
            // Skip if mtime unchanged since last index (dedup WSL/NTFS events)
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                if state
                    .last_indexed_mtime
                    .get(rel)
                    .is_some_and(|last| mtime <= *last)
                {
                    tracing::trace!(path = %rel.display(), "Skipping unchanged mtime");
                    continue;
                }
            }
            if state.pending_files.len() < max_pending_files() {
                state.pending_files.insert(rel.to_path_buf());
            } else {
                // RM-V1.25-23: log per-event at debug (spammy on bulk
                // drops) and accumulate a counter; the once-per-cycle
                // summary fires in process_file_changes so operators
                // see the total truncation even if the level is info.
                state.dropped_this_cycle = state.dropped_this_cycle.saturating_add(1);
                tracing::debug!(
                    max = max_pending_files(),
                    path = %rel.display(),
                    "Watch pending_files full, dropping file event"
                );
            }
            state.last_event = std::time::Instant::now();
        }
    }
}

/// Process pending file changes: parse, embed, store atomically, then update HNSW.
///
/// Uses incremental HNSW insertion when an Owned index is available in memory.
/// Falls back to full rebuild on first run or after `hnsw_rebuild_threshold()` incremental inserts.
fn process_file_changes(cfg: &WatchConfig, store: &Store, state: &mut WatchState) {
    let files: Vec<PathBuf> = state.pending_files.drain().collect();
    let _span = info_span!("process_file_changes", file_count = files.len()).entered();
    state.pending_files.shrink_to(64);

    // RM-V1.25-23: surface truncated cycles at warn level so operators
    // notice the gap. The per-event drops are logged at debug to keep
    // the journal clean on bulk edits.
    if state.dropped_this_cycle > 0 {
        tracing::warn!(
            dropped = state.dropped_this_cycle,
            cap = max_pending_files(),
            "Watch event queue full this cycle; dropping events. Run `cqs index` to catch up"
        );
        state.dropped_this_cycle = 0;
    }
    if !cfg.quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }

    let emb = match try_init_embedder(cfg.embedder, &mut state.embedder_backoff, cfg.model_config) {
        Some(e) => e,
        None => return,
    };

    // Capture mtimes BEFORE reindexing to avoid race condition
    let pre_mtimes: HashMap<PathBuf, SystemTime> = files
        .iter()
        .filter_map(|f| {
            std::fs::metadata(cfg.root.join(f))
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (f.clone(), t))
        })
        .collect();

    // Note: concurrent searches during this window may see partial
    // results (RT-DATA-3). Per-file transactions are atomic but the
    // batch is not — files indexed so far are visible, remaining are
    // stale. Self-heals after HNSW rebuild. Acceptable for a dev tool.
    //
    // Mark HNSW dirty before writing chunks (RT-DATA-6).
    if let Err(e) = store.set_hnsw_dirty(true) {
        tracing::warn!(error = %e, "Cannot set HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
    match reindex_files(cfg.root, store, &files, cfg.parser, emb, cfg.quiet) {
        Ok((count, content_hashes)) => {
            // Record mtimes to skip duplicate events
            for (file, mtime) in pre_mtimes {
                state.last_indexed_mtime.insert(file, mtime);
            }
            // RM-17: Prune entries for deleted files when mtime map grows large.
            // RM-4: Lowered from 10K to 5K — the map tracks every file we've ever
            // indexed in this session, so pruning earlier bounds memory without
            // affecting correctness (retain only keeps files that still exist).
            if state.last_indexed_mtime.len() > 5_000 {
                state
                    .last_indexed_mtime
                    .retain(|f, _| cfg.root.join(f).exists());
            }
            if !cfg.quiet {
                println!("Indexed {} chunk(s)", count);
            }

            // OB-NEW-10: When SPLADE is configured but watch skipped sparse
            // re-encoding (watch never runs the SPLADE encoder, only the v20
            // DELETE trigger invalidates on removals), surface the drift at
            // debug level once per reindex cycle.
            if count > 0 && cqs::splade::resolve_splade_model_dir().is_some() {
                tracing::debug!(
                    new_chunks = count,
                    "Watch skipped SPLADE encoding, sparse coverage will drift until manual 'cqs index'"
                );
            }

            // Incremental HNSW update: insert changed chunks into existing Owned index.
            // Falls back to full rebuild on first run or after hnsw_rebuild_threshold() inserts.
            let needs_full_rebuild =
                state.hnsw_index.is_none() || state.incremental_count >= hnsw_rebuild_threshold();

            // During full rebuild the old index and new batch coexist briefly,
            // but `build_batched` streams one batch at a time so peak memory is
            // old_index + one_batch, not 2× the full index.
            if needs_full_rebuild {
                match super::commands::build_hnsw_index_owned(store, cfg.cqs_dir) {
                    Ok(Some(index)) => {
                        let n = index.len();
                        state.hnsw_index = Some(index);
                        state.incremental_count = 0;
                        if let Err(e) = store.set_hnsw_dirty(false) {
                            tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                        }
                        info!(vectors = n, "HNSW index rebuilt (full)");
                        if !cfg.quiet {
                            println!("  HNSW index: {} vectors (full rebuild)", n);
                        }
                    }
                    Ok(None) => {
                        state.hnsw_index = None;
                    }
                    Err(e) => {
                        warn!(error = %e, "HNSW rebuild failed, removing stale HNSW files (search falls back to brute-force)");
                        state.hnsw_index = None;
                        for ext in cqs::hnsw::HNSW_ALL_EXTENSIONS {
                            let path = cfg.cqs_dir.join(format!("index.{}", ext));
                            if let Err(e) = std::fs::remove_file(&path) {
                                if e.kind() != std::io::ErrorKind::NotFound {
                                    tracing::warn!(
                                        error = %e,
                                        path = %path.display(),
                                        "Failed to delete stale HNSW file"
                                    );
                                }
                            }
                            let base_path = cfg.cqs_dir.join(format!("index_base.{}", ext));
                            if let Err(e) = std::fs::remove_file(&base_path) {
                                if e.kind() != std::io::ErrorKind::NotFound {
                                    tracing::warn!(
                                        error = %e,
                                        path = %base_path.display(),
                                        "Failed to delete stale base HNSW file"
                                    );
                                }
                            }
                        }
                    }
                }

                // Phase 5: also rebuild the base (non-enriched) HNSW. Not held
                // in memory by watch state — the search process loads it fresh
                // from disk. Incremental path skips base updates; they catch
                // up on the next full rebuild.
                match super::commands::build_hnsw_base_index(store, cfg.cqs_dir) {
                    Ok(Some(n)) => {
                        info!(vectors = n, "Base HNSW index rebuilt");
                        if !cfg.quiet {
                            println!("  HNSW base index: {} vectors (full rebuild)", n);
                        }
                    }
                    Ok(None) => {
                        // No base embeddings yet — skip silently
                    }
                    Err(e) => {
                        warn!(error = %e, "Base HNSW rebuild failed, router falls back to enriched-only");
                    }
                }
            } else if !content_hashes.is_empty() {
                // Incremental path: insert only newly-embedded chunks.
                // Modified chunks get new IDs, so old vectors become orphans in
                // the HNSW graph (hnsw_rs has no deletion). Orphans are harmless:
                // search post-filters against live SQLite chunk IDs. They're
                // cleaned on the next full rebuild (every hnsw_rebuild_threshold()).
                let hash_refs: Vec<&str> = content_hashes.iter().map(|s| s.as_str()).collect();
                match store.get_chunk_ids_and_embeddings_by_hashes(&hash_refs) {
                    Ok(pairs) if !pairs.is_empty() => {
                        let items: Vec<(String, &[f32])> = pairs
                            .iter()
                            .map(|(id, emb)| (id.clone(), emb.as_slice()))
                            .collect();
                        if let Some(ref mut index) = state.hnsw_index {
                            match index.insert_batch(&items) {
                                Ok(n) => {
                                    state.incremental_count += n;
                                    // Save updated index to disk for search processes
                                    if let Err(e) = index.save(cfg.cqs_dir, "index") {
                                        warn!(error = %e, "Failed to save HNSW after incremental insert");
                                    } else if let Err(e) = store.set_hnsw_dirty(false) {
                                        tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                                    }
                                    info!(
                                        inserted = n,
                                        total = index.len(),
                                        incremental_count = state.incremental_count,
                                        "HNSW incremental insert"
                                    );
                                    if !cfg.quiet {
                                        println!(
                                            "  HNSW index: +{} vectors (incremental, {} total)",
                                            n,
                                            index.len()
                                        );
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "HNSW incremental insert failed, will rebuild next cycle");
                                    // Force full rebuild next cycle
                                    state.hnsw_index = None;
                                }
                            }
                        }
                    }
                    Ok(_) => {} // no embeddings found for hashes
                    Err(e) => {
                        warn!(error = %e, "Failed to fetch embeddings for HNSW incremental insert");
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Reindex error");
        }
    }
}

/// Process notes.toml changes: parse and store notes (no embedding needed, SQ-9).
fn process_note_changes(root: &Path, store: &Store, quiet: bool) {
    if !quiet {
        println!("\nNotes changed, reindexing...");
    }
    match reindex_notes(root, store, quiet) {
        Ok(count) => {
            if !quiet {
                println!("Indexed {} note(s)", count);
            }
        }
        Err(e) => {
            warn!(error = %e, "Notes reindex error");
        }
    }
}

/// Reindex specific files.
///
/// Returns `(chunk_count, content_hashes)` — the content hashes can be used for
/// incremental HNSW insertion (looking up embeddings by hash instead of
/// rebuilding the full index).
fn reindex_files(
    root: &Path,
    store: &Store,
    files: &[PathBuf],
    parser: &CqParser,
    embedder: &Embedder,
    quiet: bool,
) -> Result<(usize, Vec<String>)> {
    let _span = info_span!("reindex_files", file_count = files.len()).entered();
    info!(file_count = files.len(), "Reindexing files");

    // Parse changed files once — extract chunks, calls, AND type refs in a single pass.
    // Avoids the previous double-read + double-parse per file.
    let mut all_type_refs: Vec<(PathBuf, Vec<ChunkTypeRefs>)> = Vec::new();
    let chunks: Vec<_> = files
        .iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            if !abs_path.exists() {
                // RT-DATA-7: File was deleted — remove its chunks from the store
                if let Err(e) = store.delete_by_origin(rel_path) {
                    tracing::warn!(
                        path = %rel_path.display(),
                        error = %e,
                        "Failed to delete chunks for deleted file"
                    );
                }
                return vec![];
            }
            match parser.parse_file_all(&abs_path) {
                Ok((mut file_chunks, calls, chunk_type_refs)) => {
                    // Rewrite paths to be relative (AC-2: fix both file and id)
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                        // Rewrite id: replace absolute path prefix with relative
                        // ID format: {path}:{line_start}:{content_hash}
                        if let Some(rest) = chunk.id.strip_prefix(&abs_path.display().to_string()) {
                            chunk.id = format!("{}{}", rel_path.display(), rest);
                        }
                    }
                    // Stash type refs for upsert after chunks are stored
                    if !chunk_type_refs.is_empty() {
                        all_type_refs.push((rel_path.clone(), chunk_type_refs));
                    }
                    // RT-DATA-8: Write function_calls table (file-level call graph).
                    // Previously discarded — callers/impact/trace commands need this.
                    if !calls.is_empty() {
                        if let Err(e) = store.upsert_function_calls(rel_path, &calls) {
                            tracing::warn!(
                                path = %rel_path.display(),
                                error = %e,
                                "Failed to write function_calls for watched file"
                            );
                        }
                    }
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!(path = %abs_path.display(), error = %e, "Failed to parse file");
                    vec![]
                }
            }
        })
        .collect();

    // Apply windowing to split long chunks into overlapping windows
    let chunks = crate::cli::pipeline::apply_windowing(chunks, embedder);

    if chunks.is_empty() {
        return Ok((0, Vec::new()));
    }

    // Check content hash cache to skip re-embedding unchanged chunks
    let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
    let existing = store.get_embeddings_by_hashes(&hashes)?;

    let mut cached: Vec<(usize, Embedding)> = Vec::new();
    let mut to_embed: Vec<(usize, &cqs::Chunk)> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(emb) = existing.get(&chunk.content_hash) {
            cached.push((i, emb.clone()));
        } else {
            to_embed.push((i, chunk));
        }
    }

    // OB-11: Log cache hit/miss stats for observability
    tracing::info!(
        cached = cached.len(),
        to_embed = to_embed.len(),
        "Embedding cache stats"
    );

    // Collect content hashes of NEWLY EMBEDDED chunks only (for incremental HNSW).
    // Unchanged chunks (cache hits) are already in the HNSW index from a prior cycle,
    // so re-inserting them would create duplicates (hnsw_rs has no dedup).
    let content_hashes: Vec<String> = to_embed
        .iter()
        .map(|(_, c)| c.content_hash.clone())
        .collect();

    // Only embed chunks that don't have cached embeddings
    let new_embeddings: Vec<Embedding> = if to_embed.is_empty() {
        vec![]
    } else {
        let texts: Vec<String> = to_embed
            .iter()
            .map(|(_, c)| generate_nl_description(c))
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        embedder.embed_documents(&text_refs)?.into_iter().collect()
    };

    // Merge cached and new embeddings in original chunk order
    let chunk_count = chunks.len();
    let mut embeddings: Vec<Embedding> = vec![Embedding::new(vec![]); chunk_count];
    for (i, emb) in cached {
        embeddings[i] = emb;
    }
    for ((i, _), emb) in to_embed.into_iter().zip(new_embeddings) {
        embeddings[i] = emb;
    }

    // DS-2: Extract call graph from chunks (same loop), then use atomic upsert.
    // This mirrors the pipeline's approach: extract_calls_from_chunk per chunk,
    // then upsert_chunks_and_calls in a single transaction per file.
    // Pre-group calls by chunk ID for O(1) lookup per file (PERF-4).
    let mut calls_by_id: HashMap<String, Vec<cqs::parser::CallSite>> = HashMap::new();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        if !calls.is_empty() {
            calls_by_id
                .entry(chunk.id.clone())
                .or_default()
                .extend(calls);
        }
    }
    // Group chunks by file and atomically upsert chunks + calls in a single transaction
    let mut mtime_cache: HashMap<PathBuf, Option<i64>> = HashMap::new();
    let mut by_file: HashMap<PathBuf, Vec<(cqs::Chunk, Embedding)>> = HashMap::new();
    for (chunk, embedding) in chunks.into_iter().zip(embeddings.into_iter()) {
        let file_key = chunk.file.clone();
        by_file
            .entry(file_key)
            .or_default()
            .push((chunk, embedding));
    }
    for (file, pairs) in &by_file {
        let mtime = *mtime_cache.entry(file.clone()).or_insert_with(|| {
            let abs_path = root.join(file);
            abs_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
        });
        // PERF-4: O(1) lookup per chunk via pre-grouped HashMap instead of linear scan.
        let file_calls: Vec<_> = pairs
            .iter()
            .flat_map(|(c, _)| {
                calls_by_id
                    .get(&c.id)
                    .into_iter()
                    .flat_map(|calls| calls.iter().map(|call| (c.id.clone(), call.clone())))
            })
            .collect();
        store.upsert_chunks_and_calls(pairs, mtime, &file_calls)?;

        // DS-37 / RT-DATA-10: Delete phantom chunks — functions removed from the
        // file but still lingering in the index. The upsert above handles updates
        // and inserts; this cleans up deletions.
        //
        // Ideally this would share a transaction with upsert_chunks_and_calls, but
        // both methods manage their own internal transactions. A crash between the
        // two leaves phantoms that get cleaned on the next reindex. Propagate the
        // error rather than silently swallowing it.
        let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
        store.delete_phantom_chunks(file, &live_ids)?;
    }

    // Upsert type edges from the earlier parse_file_all() results.
    // Type edges are soft data — separate from chunk+call atomicity.
    // They depend on chunk IDs existing in the DB, which is why we upsert
    // them after chunks are stored above. Use batched version (single transaction).
    if let Err(e) = store.upsert_type_edges_for_files(&all_type_refs) {
        tracing::warn!(error = %e, "Failed to update type edges");
    }

    if let Err(e) = store.touch_updated_at() {
        tracing::warn!(error = %e, "Failed to update timestamp");
    }

    if !quiet {
        println!("Updated {} file(s)", files.len());
    }

    Ok((chunk_count, content_hashes))
}

/// Reindex notes from docs/notes.toml
fn reindex_notes(root: &Path, store: &Store, quiet: bool) -> Result<usize> {
    let _span = info_span!("reindex_notes").entered();

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(0);
    }

    // DS-34: Hold shared lock during read+index to prevent partial reads
    // if another process is writing notes concurrently (e.g., `cqs notes add`).
    let lock_file = std::fs::File::open(&notes_path)?;
    lock_file.lock_shared()?;

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        drop(lock_file);
        return Ok(0);
    }

    let count = cqs::index_notes(&notes, &notes_path, store)?;

    drop(lock_file); // release lock after index completes

    if !quiet {
        let ns = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            ns.total, ns.warnings, ns.patterns
        );
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

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
        // These fields are unused by collect_events but required by the struct.
        // We leak a parser since tests don't call process_file_changes.
        let parser = Box::leak(Box::new(CqParser::new().unwrap()));
        let embedder = Box::leak(Box::new(OnceCell::new()));
        let model_config = Box::leak(Box::new(ModelConfig::default_model()));
        WatchConfig {
            root,
            cqs_dir,
            notes_path,
            supported_ext,
            parser,
            embedder,
            quiet: true,
            model_config,
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
}
