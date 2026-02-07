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
    cqs::enumerate_files(root, parser, no_ignore)
}

/// Check if a process with the given PID exists
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is safe - it only checks if process exists without
    // sending any signal. The pid is u32 cast to i32 which is valid for PIDs.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Acquire file lock to prevent concurrent indexing
/// Writes PID to lock file for stale lock detection
pub(crate) fn acquire_index_lock(cq_dir: &Path) -> Result<std::fs::File> {
    use fs4::fs_std::FileExt;
    use std::io::Write;

    let lock_path = cq_dir.join("index.lock");

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

    match lock_file.try_lock_exclusive() {
        Ok(()) => {
            // Write our PID to the lock file
            let mut file = lock_file;
            writeln!(file, "{}", std::process::id())?;
            file.sync_all()?;
            Ok(file)
        }
        Err(_) => {
            // Lock is held - check if the owning process is still alive
            if let Ok(content) = std::fs::read_to_string(&lock_path) {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    if !process_exists(pid) {
                        // Stale lock - process is dead, remove and retry
                        tracing::warn!("Removing stale lock (PID {} no longer exists)", pid);
                        drop(lock_file);
                        std::fs::remove_file(&lock_path)?;
                        // Recursive retry (once)
                        return acquire_index_lock(cq_dir);
                    }
                }
            }
            bail!(
                "Another cqs process is indexing (see .cq/index.lock). \
                 Hint: Wait for it to finish, or delete .cq/index.lock if the process crashed."
            )
        }
    }
}
