//! Shared filesystem primitives for durable, atomic file writes.
//!
//! The write-to-temp-then-rename pattern is repeated throughout cqs for
//! persisting indexes, configuration, and notes. Historically each site
//! evolved its own copy, and several audits (DS-V1.25-1 and DS-V1.25-4 in
//! particular) caught sites that were missing `sync_all` on the temp file
//! or on the parent directory. `atomic_replace` concentrates that logic in
//! one place so new persistence sites inherit the correct durability
//! semantics by default.

use std::io;
use std::path::Path;

/// Atomically replace `final_path` with the contents of `tmp_path`.
///
/// Sequence:
/// 1. `fsync` the temp file so its bytes are durable on disk before the rename.
/// 2. `rename(tmp_path, final_path)` — atomic on the same filesystem. On
///    cross-device failure (`EXDEV` → `io::ErrorKind::CrossesDevices`, seen
///    on Docker overlayfs, NFS, WSL `/mnt/c`) we fall back to copy into a
///    unique same-directory temp + fsync that copy + rename it into place,
///    then best-effort remove the source temp.
/// 3. Best-effort `fsync` the parent directory so the rename itself is
///    durable against power loss. Some filesystems (tmpfs, certain network
///    FSes) do not support this and return errors we log at debug level.
///
/// The caller is responsible for:
/// - Setting correct permissions on the temp file before calling (the
///   rename preserves them on unix).
/// - Holding any locks required for serialization with concurrent writers.
/// - Cleaning up `tmp_path` on error paths before `atomic_replace` was
///   invoked (this helper only manages temps it creates internally for
///   the cross-device fallback).
///
/// On Windows this helper still goes through `std::fs::rename` which uses
/// `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` under the hood, so
/// rename-over-existing works. The cross-device fallback uses
/// `ErrorKind::CrossesDevices` which is detected identically on Windows.
/// The parent-dir fsync is a no-op on Windows because NTFS journals the
/// rename as part of its own metadata update.
pub fn atomic_replace(tmp_path: &Path, final_path: &Path) -> io::Result<()> {
    // Step 1: fsync the temp file so the data is durable before the rename.
    {
        let f = std::fs::File::open(tmp_path)?;
        f.sync_all()?;
    }

    // Step 2: try the cheap rename first.
    match std::fs::rename(tmp_path, final_path) {
        Ok(()) => {}
        Err(e) if is_cross_device(&e) => {
            // Cross-device: copy into a unique same-dir temp, fsync it,
            // rename atomically, then best-effort clean up the source.
            let dest_tmp = cross_device_tmp_path(final_path);
            std::fs::copy(tmp_path, &dest_tmp)?;
            // fsync the copied file before rename for durability.
            match std::fs::File::open(&dest_tmp) {
                Ok(f) => {
                    if let Err(fsync_err) = f.sync_all() {
                        tracing::debug!(
                            error = %fsync_err,
                            path = %dest_tmp.display(),
                            "fsync of cross-device dest tmp failed (non-fatal)"
                        );
                    }
                }
                Err(open_err) => {
                    tracing::debug!(
                        error = %open_err,
                        path = %dest_tmp.display(),
                        "could not reopen cross-device dest tmp for fsync"
                    );
                }
            }
            if let Err(rn_err) = std::fs::rename(&dest_tmp, final_path) {
                // Best-effort cleanup of both temps before returning error.
                let _ = std::fs::remove_file(&dest_tmp);
                let _ = std::fs::remove_file(tmp_path);
                return Err(rn_err);
            }
            // Source temp is no longer needed.
            let _ = std::fs::remove_file(tmp_path);
        }
        Err(e) => return Err(e),
    }

    // Step 3: fsync the parent directory so the rename is durable. Best
    // effort — some filesystems do not support opening a directory for
    // fsync; we log at debug and continue.
    if let Some(parent) = final_path.parent() {
        match std::fs::File::open(parent) {
            Ok(dir) => {
                if let Err(e) = dir.sync_all() {
                    tracing::debug!(
                        error = %e,
                        parent = %parent.display(),
                        "parent dir fsync failed after atomic_replace (non-fatal)"
                    );
                }
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    parent = %parent.display(),
                    "could not open parent dir for fsync after atomic_replace (non-fatal)"
                );
            }
        }
    }

    Ok(())
}

/// Detect a cross-device rename error.
///
/// On stable Rust 1.85+ the dedicated `ErrorKind::CrossesDevices` is the
/// cleanest way to recognize `EXDEV` from Unix and `ERROR_NOT_SAME_DEVICE`
/// from Windows. We also accept raw `EXDEV` as a belt-and-suspenders check
/// in case the mapping ever regresses — historically some libc versions
/// and Rust versions surfaced this as `Other`.
fn is_cross_device(e: &io::Error) -> bool {
    if matches!(e.kind(), io::ErrorKind::CrossesDevices) {
        return true;
    }
    #[cfg(unix)]
    {
        // libc::EXDEV = 18 on Linux and BSDs.
        if let Some(code) = e.raw_os_error() {
            return code == libc::EXDEV;
        }
    }
    false
}

/// Build a unique temp path in the same directory as `final_path` for the
/// cross-device fallback. Uses the process id plus the shared random
/// `temp_suffix` so two concurrent saves cannot collide.
fn cross_device_tmp_path(final_path: &Path) -> std::path::PathBuf {
    let dir = final_path.parent().unwrap_or(Path::new("."));
    let name = final_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let suffix = crate::temp_suffix();
    let pid = std::process::id();
    dir.join(format!(".{}.xdev.{}.{:016x}.tmp", name, pid, suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn atomic_replace_same_fs_moves_bytes_to_final_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let tmp = dir.path().join(".config.tmp");
        let final_path = dir.path().join("config.toml");
        let payload = b"hello = 1\n";

        std::fs::write(&tmp, payload).unwrap();
        atomic_replace(&tmp, &final_path).unwrap();

        assert!(!tmp.exists(), "temp file should be gone after rename");
        assert!(final_path.exists(), "final file should exist");
        let got = std::fs::read(&final_path).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn atomic_replace_overwrites_existing_final_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let tmp = dir.path().join(".config.tmp");
        let final_path = dir.path().join("config.toml");

        std::fs::write(&final_path, b"old contents").unwrap();
        std::fs::write(&tmp, b"new contents").unwrap();
        atomic_replace(&tmp, &final_path).unwrap();

        let got = std::fs::read(&final_path).unwrap();
        assert_eq!(got, b"new contents");
    }

    #[test]
    fn atomic_replace_missing_tmp_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let tmp = dir.path().join("does_not_exist.tmp");
        let final_path = dir.path().join("config.toml");

        let err = atomic_replace(&tmp, &final_path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!final_path.exists());
    }

    #[test]
    fn atomic_replace_preserves_written_bytes_over_flush() {
        // Regression guard for the DS-V1.25-4 shape: data written via
        // BufWriter but never synced should still end up in the final
        // file because atomic_replace fsyncs the temp before rename.
        let dir = tempfile::TempDir::new().unwrap();
        let tmp = dir.path().join(".ids.tmp");
        let final_path = dir.path().join("ids");
        {
            let f = std::fs::File::create(&tmp).unwrap();
            let mut w = std::io::BufWriter::new(f);
            w.write_all(b"0:a\n1:b\n").unwrap();
            w.flush().unwrap();
            // No explicit sync_all — atomic_replace should do it.
        }
        atomic_replace(&tmp, &final_path).unwrap();
        let got = std::fs::read_to_string(&final_path).unwrap();
        assert_eq!(got, "0:a\n1:b\n");
    }
}
