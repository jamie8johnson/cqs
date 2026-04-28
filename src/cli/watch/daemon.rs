//! Daemon thread spawn — the long-running socket accept loop that runs
//! when `cqs watch --serve` is set. Carved out of `cmd_watch` so the
//! main control flow reads as orchestration and not 160 lines of
//! per-connection plumbing.
//!
//! Unix-only: the daemon socket uses `std::os::unix::net::UnixListener`,
//! which is not available on Windows. Callers gate the entire
//! `--serve` path on `#[cfg(unix)]`.

#![cfg(unix)]

use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use cqs::embedder::{Embedder, ModelConfig};

use super::{
    daemon_should_exit, handle_socket_client, max_concurrent_daemon_clients, write_daemon_error,
};

/// Spawn the daemon's accept-loop thread. Returns the join handle so
/// `cmd_watch`'s shutdown path can drain the loop on Ctrl+C / SIGTERM.
///
/// `daemon_watch_snapshot` (#1182): the `Arc<RwLock<WatchSnapshot>>` the
/// outer `cmd_watch` scope shares with the watch loop. The daemon plugs
/// this into the BatchContext via [`crate::cli::batch::BatchContext::adopt_watch_snapshot`]
/// so `dispatch_status` reads the loop's most-recently-published snapshot
/// instead of the default `unknown`.
pub(super) fn spawn_daemon_thread(
    listener: UnixListener,
    daemon_embedder: Arc<OnceLock<Arc<Embedder>>>,
    daemon_model_config: ModelConfig,
    daemon_runtime: Arc<tokio::runtime::Runtime>,
    daemon_watch_snapshot: cqs::watch_status::SharedWatchSnapshot,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // BatchContext created inside the thread — RefCell is !Send
        // but thread-local ownership is fine. `mut` so we can adopt the
        // shared watch-snapshot handle below before wrapping in Arc<Mutex<…>>.
        let mut ctx = match crate::cli::batch::create_context_with_runtime(Some(daemon_runtime)) {
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
        // #1182: install the shared watch-snapshot handle before locking
        // the ctx into the Arc<Mutex<...>>. After this swap, the daemon's
        // `dispatch_status` handler reads through the same Arc the watch
        // loop publishes into.
        ctx.adopt_watch_snapshot(daemon_watch_snapshot);

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
                        let _ =
                            write_daemon_error(&mut s, "daemon busy (too many concurrent clients)");
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
    })
}
