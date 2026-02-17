//! File operations for indexing
//!
//! Provides file enumeration and index locking.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Enumerate files to index (delegates to library implementation)
pub(crate) fn enumerate_files(
    root: &Path,
    parser: &cqs::Parser,
    no_ignore: bool,
) -> Result<Vec<PathBuf>> {
    let exts = parser.supported_extensions();
    cqs::enumerate_files(root, &exts, no_ignore)
}

/// Check if a process with the given PID exists
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is safe - it only checks if process exists without
    // sending any signal.
    i32::try_from(pid).is_ok_and(|p| unsafe { libc::kill(p, 0) == 0 })
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| {
            let output = String::from_utf8_lossy(&o.stdout);
            // tasklist /FI "PID eq N" does exact filtering.
            // "INFO:" appears when no process matches; its absence means a match.
            !output.contains("INFO:")
        })
        .unwrap_or(false)
}

/// Acquire file lock to prevent concurrent indexing
/// Writes PID to lock file for stale lock detection
pub(crate) fn acquire_index_lock(cqs_dir: &Path) -> Result<std::fs::File> {
    use std::io::Write;

    let lock_path = cqs_dir.join("index.lock");
    let mut retried = false;

    loop {
        // Try to open/create the lock file with restrictive permissions (0600 on Unix).
        // Lock file contains PID which could leak process information.
        #[cfg(unix)]
        let lock_file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&lock_path)
                .context("Failed to create lock file")?
        };

        #[cfg(not(unix))]
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .context("Failed to create lock file")?;

        match lock_file.try_lock() {
            Ok(()) => {
                // Write our PID to the lock file
                let mut file = lock_file;
                writeln!(file, "{}", std::process::id())?;
                file.sync_all()?;
                return Ok(file);
            }
            Err(_) => {
                // Lock is held - check if the owning process is still alive
                if !retried {
                    if let Ok(content) = std::fs::read_to_string(&lock_path) {
                        if let Ok(pid) = content.trim().parse::<u32>() {
                            if !process_exists(pid) {
                                // Stale lock - process is dead, remove and retry once
                                tracing::warn!(
                                    "Removing stale lock (PID {} no longer exists)",
                                    pid
                                );
                                drop(lock_file);
                                std::fs::remove_file(&lock_path)?;
                                retried = true;
                                continue;
                            }
                        }
                    }
                }
                bail!(
                    "Another cqs process is indexing (see .cqs/index.lock). \
                     Hint: Wait for it to finish, or delete .cqs/index.lock if the process crashed."
                )
            }
        }
    }
}
