//! Daemon runtime helpers: shutdown signal flags, shared tokio runtime
//! builder, SIGTERM handler.
//!
//! Carved out of `watch.rs`. The unix-only items here drive the daemon
//! drain path (SIGTERM → flag → accept-loop break → main loop joins
//! the socket thread). `build_shared_runtime` is cross-platform and
//! powers `Store` / `EmbeddingCache` / `QueryCache` from a single pool.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// RM-V1.25-9: Set on SIGTERM so the watch loop drains and exits
/// cleanly instead of being hard-killed mid-write when systemd
/// sends `stop`. The cross-platform `ctrlc::set_handler` (with the
/// `termination` feature, since #1044 / DS-V1.30.2-D5) also raises
/// SIGTERM into our `INTERRUPTED` flag — we keep this dedicated
/// libc::signal path because the daemon socket accept loop polls
/// `daemon_should_exit` (the OR of `SHUTDOWN_REQUESTED` and
/// `INTERRUPTED`) and the SIGTERM handler must be async-signal-safe
/// in the strict POSIX sense; the `ctrlc` path runs the closure on
/// a separate thread, which the daemon-shutdown protocol can't rely
/// on for ordering.
#[cfg(unix)]
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
pub(super) fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Acquire)
}

/// RM-V1.25-8: observable from both SIGTERM (SHUTDOWN_REQUESTED) and
/// Ctrl+C (`check_interrupted`). The socket accept loop polls this so
/// the watch main loop can tell the daemon thread to drain without
/// having to route a separate shutdown channel.
#[cfg(unix)]
pub(super) fn daemon_should_exit() -> bool {
    is_shutdown_requested() || super::check_interrupted()
}

/// Signal handler — async-signal-safe: only a relaxed atomic store.
#[cfg(unix)]
extern "C" fn on_sigterm(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

/// Build the tokio runtime that the daemon shares across `Store`,
/// `EmbeddingCache`, and `QueryCache` (#968).
///
/// Uses `multi_thread` with `worker_threads = min(num_cpus, 4)` by default to match
/// `Store::open`'s pre-968 default (that was the heaviest of the three).
/// One shared pool replaces three separate per-struct runtimes that
/// previously idled ~6–12 OS threads in the daemon with no overlap.
///
/// Override via `CQS_DAEMON_WORKER_THREADS` for large hosts where the
/// `min(_, 4)` cap leaves cores idle under heavy concurrent client load.
pub(super) fn build_shared_runtime() -> std::io::Result<Arc<tokio::runtime::Runtime>> {
    let worker_threads = std::env::var("CQS_DAEMON_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(4)
        });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .thread_name("cqs-shared-rt")
        .build()?;
    tracing::debug!(
        worker_threads,
        "Built shared tokio runtime for Store/EmbeddingCache/QueryCache"
    );
    Ok(Arc::new(rt))
}
/// Install a SIGTERM handler so `systemctl stop cqs-watch` triggers a
/// clean drain via `SHUTDOWN_REQUESTED` rather than a hard kill. The
/// existing ctrlc-based SIGINT handler already flips `check_interrupted`;
/// this adds SIGTERM as a second shutdown path.
#[cfg(unix)]
pub(super) fn install_sigterm_handler() {
    // SAFETY: libc::signal is async-signal-safe to call before any
    // threads start depending on the old disposition. We call it at
    // the top of cmd_watch, prior to spawning the socket thread.
    unsafe {
        let prev = libc::signal(libc::SIGTERM, on_sigterm as *const () as libc::sighandler_t);
        if prev == libc::SIG_ERR {
            let e = std::io::Error::last_os_error();
            tracing::warn!(error = %e, "Failed to install SIGTERM handler; watch will rely on SIGINT only");
        } else {
            tracing::debug!("SIGTERM handler installed for clean daemon shutdown");
        }
    }
}
