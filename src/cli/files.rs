//! File operations for indexing
//!
//! Provides file enumeration and index locking.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;

use cqs::Parser as CqParser;

/// Maximum file size to index (1MB)
const MAX_FILE_SIZE: u64 = 1_048_576;

/// Strip Windows UNC path prefix (\\?\) if present.
///
/// Windows `canonicalize()` returns UNC paths that can cause issues with
/// path comparison and display. This strips the prefix for consistency.
#[cfg(windows)]
fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

/// No-op on non-Windows platforms
#[cfg(not(windows))]
fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    path
}

/// Enumerate files to index
///
/// Note on I/O efficiency: WalkBuilder's DirEntry caches metadata from the initial
/// stat() during directory traversal, so e.metadata() doesn't re-stat. The
/// canonicalize() call does require a separate syscall for symlink resolution,
/// but this is unavoidable for correct path validation.
pub(super) fn enumerate_files(
    root: &Path,
    parser: &CqParser,
    no_ignore: bool,
) -> Result<Vec<PathBuf>> {
    let root = strip_unc_prefix(root.canonicalize().context("Failed to canonicalize root")?);

    let walker = WalkBuilder::new(&root)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .ignore(!no_ignore)
        .hidden(!no_ignore)
        .follow_links(false)
        .build();

    let files: Vec<PathBuf> = walker
        .filter_map(|e| {
            e.map_err(|err| {
                tracing::debug!(error = %err, "Failed to read directory entry during walk");
            })
            .ok()
        })
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| {
            // Skip files over size limit
            e.metadata()
                .map(|m| m.len() <= MAX_FILE_SIZE)
                .unwrap_or(false)
        })
        .filter(|e| {
            // Only supported extensions
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| parser.supported_extensions().contains(&ext))
                .unwrap_or(false)
        })
        .filter_map({
            // Track count of canonicalization failures to log at appropriate level
            let failure_count = std::sync::atomic::AtomicUsize::new(0);
            move |e| {
                // Validate path stays within project root and convert to relative
                let path = match e.path().canonicalize() {
                    Ok(p) => p,
                    Err(err) => {
                        let count =
                            failure_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if count < 3 {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "Failed to canonicalize path, skipping"
                            );
                        } else {
                            tracing::debug!(
                                path = %e.path().display(),
                                error = %err,
                                "Failed to canonicalize path, skipping"
                            );
                        }
                        return None;
                    }
                };
                if path.starts_with(&root) {
                    // Store relative path for portability and glob matching
                    Some(path.strip_prefix(&root).unwrap_or(&path).to_path_buf())
                } else {
                    tracing::warn!("Skipping path outside project: {}", e.path().display());
                    None
                }
            }
        })
        .collect();

    Ok(files)
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
pub(super) fn acquire_index_lock(cq_dir: &Path) -> Result<std::fs::File> {
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
