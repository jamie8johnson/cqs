//! Signal handling for graceful shutdown
//!
//! Provides Ctrl+C handling with two-phase shutdown:
//! - First Ctrl+C: Set interrupted flag, allow current work to finish
//! - Second Ctrl+C: Force exit with code 130

use std::sync::atomic::{AtomicBool, Ordering};

/// Exit codes for CLI commands
#[repr(i32)]
pub enum ExitCode {
    /// Search returned no results
    NoResults = 2,
    /// CI gate failed (risk threshold exceeded)
    GateFailed = 3,
    /// User interrupted with Ctrl+C
    Interrupted = 130,
}

/// Global flag indicating user requested interruption
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Install Ctrl+C handler for graceful shutdown.
///
/// First signal sets `INTERRUPTED`, allowing current batch to finish.
/// Second signal force-exits with code 130.
///
/// DS-V1.30.2-D5 (#1044): with `ctrlc`'s `termination` feature, this
/// handler catches more than just Ctrl+C:
///
/// - **Unix**: SIGINT, SIGTERM, SIGHUP. The watch loop already
///   installs its own `libc::signal(SIGTERM, ...)` *after* this
///   handler in `cmd_watch`, so our SIGTERM-specific path
///   (`SHUTDOWN_REQUESTED`) wins on Unix as before — `ctrlc`'s
///   coverage there is belt-and-braces for non-watch commands.
/// - **Windows**: `CTRL_C_EVENT`, `CTRL_BREAK_EVENT` (sent by
///   `Stop-Process` / `taskkill /B`), `CTRL_CLOSE_EVENT` (console
///   window closed), `CTRL_LOGOFF_EVENT` (user logout),
///   `CTRL_SHUTDOWN_EVENT` (system shutdown). This is the
///   load-bearing change for #1044: native Windows `cqs watch`
///   deployments can now drain cleanly on every interactive shutdown
///   path instead of only Ctrl+C from the launching console. The
///   only Windows path *not* covered is `SERVICE_CONTROL_STOP` from
///   a Windows Service wrapper — there is no service wrapper shipped
///   today, so that's out of scope until one ships.
pub fn setup_signal_handler() {
    if let Err(e) = ctrlc::set_handler(|| {
        if INTERRUPTED.swap(true, Ordering::AcqRel) {
            // Second signal: force exit.
            std::process::exit(ExitCode::Interrupted as i32);
        }
        eprintln!("\nInterrupted. Finishing current batch...");
    }) {
        tracing::warn!(error = %e, "Failed to set signal handler");
    }
}

/// Check if user requested interruption via Ctrl+C
pub fn check_interrupted() -> bool {
    INTERRUPTED.load(Ordering::Acquire)
}

/// Reset the interrupted flag.
///
/// Call at the start of each top-level operation so a prior Ctrl+C
/// (e.g. during `cqs watch`) doesn't poison subsequent work.
pub fn reset_interrupted() {
    INTERRUPTED.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reset_interrupted_clears_flag() {
        // Set the flag manually
        INTERRUPTED.store(true, Ordering::Release);
        assert!(check_interrupted());
        // Reset should clear it
        reset_interrupted();
        assert!(!check_interrupted());
    }
}
