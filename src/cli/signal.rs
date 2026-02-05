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
    /// User interrupted with Ctrl+C
    Interrupted = 130,
}

/// Global flag indicating user requested interruption
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Install Ctrl+C handler for graceful shutdown
///
/// First Ctrl+C sets INTERRUPTED flag, allowing current batch to finish.
/// Second Ctrl+C force-exits with code 130.
pub fn setup_signal_handler() {
    if let Err(e) = ctrlc::set_handler(|| {
        if INTERRUPTED.swap(true, Ordering::AcqRel) {
            // Second Ctrl+C: force exit
            std::process::exit(ExitCode::Interrupted as i32);
        }
        eprintln!("\nInterrupted. Finishing current batch...");
    }) {
        eprintln!("Warning: Failed to set Ctrl+C handler: {e}");
    }
}

/// Check if user requested interruption via Ctrl+C
pub fn check_interrupted() -> bool {
    INTERRUPTED.load(Ordering::Acquire)
}
