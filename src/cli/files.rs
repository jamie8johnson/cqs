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
    // PB-V1.30.1-3: previous impl matched the localized "INFO:" prefix
    // emitted only on English Windows. German `INFORMATION:`, French
    // `INFORMATIONS:`, Japanese `情報:`, etc. silently bypassed the
    // stale-PID detection, producing persistent stale-lock errors for
    // every non-English Windows user. CSV format is locale-independent:
    // tasklist /FO CSV /NH emits exactly one row per match and an empty
    // (or whitespace-only) stdout when no process is found. The PID
    // column substring `,"<pid>",` defends against substring collisions
    // (e.g., PID `12` matching PID `1234`).
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH", "/FO", "CSV"])
        .output()
        .map(|o| {
            let output = String::from_utf8_lossy(&o.stdout);
            output.trim().contains(&format!(",\"{}\",", pid))
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
/// Writes PID to lock file for stale lock detection.
///
/// # Concurrency contract by platform (P3.35)
///
/// `index.lock` uses Rust 1.89's `File::try_lock` (the std wrapper around
/// `flock` on Unix and `LockFileEx` on Windows). The two platforms enforce
/// fundamentally different contracts under the same API:
///
/// - **Linux / macOS — advisory.** `flock` only blocks other callers that
///   also call `flock`/`File::try_lock`. A non-cqs writer (a test fixture
///   that opens `index.db` directly, a stray `sqlite3` shell, an editor
///   "save and reload") can ignore the lock and corrupt the DB. The lock
///   protects cqs-vs-cqs concurrency only.
/// - **Windows — mandatory.** `LockFileEx` is enforced by the kernel; any
///   process that opens `index.db` while cqs holds the lock can see
///   `ERROR_SHARING_VIOLATION`. Third-party tools (DB browsers, backup
///   agents, antivirus on-access scanners) may fail with confusing errors.
/// - **WSL `/mnt/c/` (drvfs).** Looks like Linux for the syscall but the
///   underlying file is on NTFS. Treat the call as Linux-advisory for cqs
///   semantics, but expect Windows-side processes to see the file as
///   mandatorily locked.
///
/// On Windows we emit a one-shot `tracing::warn!` at first acquisition so
/// operators can correlate third-party "sharing violation" errors with cqs.
///
/// P2 #31 (post-v1.27.0 audit): does NOT remove the lock file inode on
/// stale-PID detection. Removing the inode races with peers in three windows:
///   1. PID lookup is approximate on Linux (zombies / PID namespaces /
///      pid recycling). `process_exists(stale_pid)` can return false even
///      though a freshly-spawned cqs process just claimed that PID. If we
///      then unlink the lock file, both processes get fresh inodes and the
///      kernel locks them independently — two writers, same DB.
///   2. Between `process_exists` and `remove_file` the genuine owner could
///      have crashed and been replaced by a new owner that legitimately
///      acquired the lock. Deleting *their* file gives the next caller a
///      fresh inode they can lock without contention. Two writers.
///   3. On WSL `/mnt/c` (NTFS over 9P) `flock` is purely advisory and PID
///      lookup is unreliable across Windows-side cqs invocations launched
///      via `powershell.exe`.
///
/// Instead, on stale-PID detection we drop the failed lock_file (releasing
/// the underlying fd) and re-attempt `try_lock` against a freshly-opened
/// handle. The kernel locks per-inode, not per-path, so this picks up any
/// transient release without ever unlinking the file. If the second attempt
/// also fails we return a clearer error mentioning the PID and the manual
/// remediation path.
pub(crate) fn acquire_index_lock(cqs_dir: &Path) -> Result<std::fs::File> {
    // P3.35: emit a one-shot warning on Windows so operators can correlate
    // third-party "sharing violation" errors with cqs holding index.lock.
    #[cfg(windows)]
    {
        static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!(
                "index.lock is mandatory on Windows — third-party tools opening \
                 index.db may fail with sharing violations while cqs is running"
            );
        });
    }

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
                    let stale_pid = std::fs::read_to_string(&lock_path)
                        .ok()
                        .and_then(|c| c.trim().parse::<u32>().ok());
                    if let Some(pid) = stale_pid {
                        if !process_exists(pid) {
                            // Stale lock by best-effort PID check: drop the
                            // failed handle and try once more against a
                            // freshly-opened fd. NEVER unlink the file —
                            // see the function-level doc comment.
                            tracing::warn!(
                                pid,
                                "Lock file PID does not appear to exist; retrying lock without unlinking"
                            );
                            drop(lock_file);
                            retried = true;
                            continue;
                        }
                    }
                }
                let pid_msg = match std::fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|c| c.trim().parse::<u32>().ok())
                {
                    Some(pid) => format!(" (PID {pid} may be stale)"),
                    None => String::new(),
                };
                bail!(
                    "Another cqs process holds the index lock at {}{pid_msg}. \
                     Hint: wait for it to finish, or manually delete the lock file \
                     only if you are confident no other cqs process is running.",
                    lock_path.display()
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

    /// PB-V1.30.1-3: pure parser-shape test for the CSV PID-column
    /// substring check. Mirrors the post-fix logic in `process_exists`
    /// (Windows-only) so we can verify the column-bounded match on Linux
    /// CI without spawning `tasklist`. Avoids substring collisions like
    /// PID `12` matching PID `1234`.
    fn csv_contains_pid(output: &str, pid: u32) -> bool {
        output.trim().contains(&format!(",\"{}\",", pid))
    }

    #[test]
    fn csv_parser_matches_exact_pid_column() {
        // Real tasklist /FO CSV /NH output (sampled from a Windows host):
        let sample = r#""cqs.exe","1234","Console","1","12,345 K"
"#;
        assert!(csv_contains_pid(sample, 1234), "1234 must match exactly");
    }

    #[test]
    fn csv_parser_rejects_substring_pid_collision() {
        let sample = r#""cqs.exe","1234","Console","1","12,345 K"
"#;
        // PID 12 must NOT match the row carrying PID 1234.
        assert!(
            !csv_contains_pid(sample, 12),
            "PID 12 must not match PID 1234 column",
        );
        // PID 234 must NOT match either — different boundary, same lesson.
        assert!(
            !csv_contains_pid(sample, 234),
            "PID 234 must not match PID 1234 column",
        );
    }

    #[test]
    fn csv_parser_returns_false_on_empty_output() {
        // tasklist emits an empty stdout (or whitespace) when no match.
        assert!(!csv_contains_pid("", 1234));
        assert!(!csv_contains_pid("   \r\n", 1234));
    }

    #[test]
    fn csv_parser_locale_independence() {
        // The whole point of PB-V1.30.1-3: a German "INFORMATION:" or
        // French "INFORMATIONS:" line must NOT be confused with a match
        // because the parser only checks the CSV PID column. Empty
        // stdout (no match) regardless of localized informational text.
        let german =
            "INFORMATION: Es laufen keine Tasks, die den angegebenen Kriterien entsprechen.\n";
        assert!(!csv_contains_pid(german, 1234));
        let french = "INFORMATIONS: Aucune tâche en cours d'exécution ne correspond aux critères spécifiés.\n";
        assert!(!csv_contains_pid(french, 1234));
    }
}
