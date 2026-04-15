//! File operations for indexing
//!
//! Provides file enumeration and index locking.

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

#[cfg(unix)]
/// Derive the daemon socket path for a given cqs_dir.
///
/// Unix domain sockets don't work on WSL 9P mounts (/mnt/c/), so the socket
/// is placed on the native Linux filesystem ($XDG_RUNTIME_DIR or /tmp).
/// The filename is derived from a hash of cqs_dir to support multiple projects.
///
/// SEC-V1.25-3: The `DefaultHasher` used below is NOT a security property.
/// It is collision-avoidance only (per-project socket naming). Access control
/// for the socket relies entirely on filesystem permissions — the socket is
/// created with mode 0o600 so only the owning user can connect. Do not treat
/// the hash as a secret or unguessable token.
///
/// #972: the implementation lives in `cqs::daemon_translate::daemon_socket_path`
/// so integration tests can compute the same path. This wrapper keeps the
/// existing `super::daemon_socket_path(...)` call sites inside `cli/`.
pub(crate) fn daemon_socket_path(cqs_dir: &Path) -> PathBuf {
    cqs::daemon_translate::daemon_socket_path(cqs_dir)
}

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
/// Checks whether a process with the given PID exists on Windows.
///
/// Uses the `tasklist` command with PID filtering to determine if a process is currently running.
///
/// # Arguments
///
/// * `pid` - The process ID to check for existence
///
/// # Returns
///
/// `true` if a process with the given PID exists, `false` otherwise. Returns `false` if the `tasklist` command fails to execute.

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

/// Open or create the lock file without truncating.
///
/// Does NOT truncate — another process's PID remains readable until we acquire
/// the lock and overwrite it. This prevents the race where truncate clears a
/// live holder's PID before we even attempt the lock.
fn open_lock_file(lock_path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(lock_path)
            .context("Failed to create lock file")
    }

    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .context("Failed to create lock file")
    }
}

/// Write our PID to the lock file (truncate + write + sync).
///
/// Called only after successfully acquiring the OS lock.
fn write_pid(file: &mut std::fs::File) -> Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(())
}

/// Try to acquire the index lock non-blockingly.
///
/// Returns `Some(file)` if the lock was acquired, `None` if another process holds it.
/// Unlike [`acquire_index_lock`], this does NOT bail — callers can skip work when locked.
pub(crate) fn try_acquire_index_lock(cqs_dir: &Path) -> Result<Option<std::fs::File>> {
    let lock_path = cqs_dir.join("index.lock");
    let lock_file = open_lock_file(&lock_path)?;

    match lock_file.try_lock() {
        Ok(()) => {
            let mut file = lock_file;
            write_pid(&mut file)?;
            Ok(Some(file))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, path = %lock_path.display(), "Lock I/O error (not contention)");
            Err(anyhow::anyhow!("Lock error: {}", e))
        }
    }
}

/// Acquire file lock to prevent concurrent indexing
/// Writes PID to lock file for stale lock detection
pub(crate) fn acquire_index_lock(cqs_dir: &Path) -> Result<std::fs::File> {
    let lock_path = cqs_dir.join("index.lock");
    let mut retried = false;

    loop {
        let lock_file = open_lock_file(&lock_path)?;

        match lock_file.try_lock() {
            Ok(()) => {
                let mut file = lock_file;
                write_pid(&mut file)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn daemon_socket_path_deterministic() {
        let p1 = daemon_socket_path(Path::new("/mnt/c/Projects/cqs/.cqs"));
        let p2 = daemon_socket_path(Path::new("/mnt/c/Projects/cqs/.cqs"));
        assert_eq!(p1, p2, "Same cqs_dir should produce the same socket path");
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_path_differs_per_project() {
        let p1 = daemon_socket_path(Path::new("/mnt/c/ProjectA/.cqs"));
        let p2 = daemon_socket_path(Path::new("/mnt/c/ProjectB/.cqs"));
        assert_ne!(p1, p2, "Different projects should get different sockets");
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_path_ends_with_sock() {
        let p = daemon_socket_path(Path::new("/tmp/test/.cqs"));
        assert!(
            p.extension().is_some_and(|e| e == "sock"),
            "Socket path should end with .sock"
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_path_not_on_project_dir() {
        let p = daemon_socket_path(Path::new("/mnt/c/Projects/cqs/.cqs"));
        assert!(
            !p.starts_with("/mnt/c"),
            "Socket should be on native filesystem, not /mnt/c/"
        );
    }
}
