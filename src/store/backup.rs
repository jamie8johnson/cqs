//! Filesystem snapshots of `index.db` taken before schema migrations run.
//!
//! `migrate()` wraps all DDL/DML in a single `pool.begin()` transaction. SQLite
//! rolls back the transaction if any step fails, which covers the happy path.
//! It does NOT cover:
//!
//! 1. A commit-time I/O failure mid-WAL-write (disk full, fs quota, network-FS
//!    disconnect, user pulling USB). The in-memory pool state can think
//!    rollback completed while the on-disk file sees partial pages.
//! 2. A bug *inside* a migration function that writes logically-inconsistent
//!    state — the transaction commits cleanly but the data is wrong.
//!
//! Before any DDL runs, we snapshot `index.db` (and its WAL/SHM sidecars if
//! present) to a sibling `{stem}.bak-v{from}-v{to}-{unix_ts}.db` file via
//! `crate::fs::atomic_replace`. If any migration step fails, the DB is
//! restored from the backup atomically; the caller sees either pre-migrate
//! or post-migrate state, never a partial write.
//!
//! Backups are pruned on success: the newest two (including the one just
//! written) are kept, older ones are deleted.
//!
//! Precedent: `src/hnsw/persist.rs:389-406` uses an identical
//! save-with-backup-and-rollback pattern for HNSW graph files.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use super::helpers::StoreError;

/// Env var that promotes a backup failure from "warn and continue" to "hard
/// error". Default is off — users on tight-disk WSL 9P deployments should not
/// be blocked from migrating by a backup that we couldn't write. Fleet
/// operators who want stricter guarantees can set it to `1`.
pub(crate) const REQUIRE_BACKUP_ENV: &str = "CQS_MIGRATE_REQUIRE_BACKUP";

/// Number of version-tagged backups to retain in the DB's parent directory.
/// The most recent `KEEP_BACKUPS` (by mtime) survive; older ones are pruned
/// on every successful migrate.
///
/// Value of 3 = the backup from the current migrate run + the two prior
/// runs' backups. That gives the user two additional recovery points if a
/// migration bug is discovered after a subsequent migrate has completed.
pub(crate) const KEEP_BACKUPS: usize = 3;

/// Build the backup path for a given migration span.
///
/// Filename format: `{db_stem}.bak-v{from}-v{to}-{unix_ts}.db`.
/// The filename lives in the same directory as `db_path` so the backup shares
/// the DB's filesystem — `atomic_replace`'s cheap rename path works without
/// falling back to cross-device copy.
pub(crate) fn backup_path_for(db_path: &Path, from: i32, to: i32) -> PathBuf {
    let dir = db_path.parent().unwrap_or(Path::new("."));
    let stem = db_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "index".to_string());
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("{}.bak-v{}-v{}-{}.db", stem, from, to, ts))
}

/// Take a filesystem snapshot of `index.db` (+ `-wal`/`-shm` if present)
/// before a migration runs.
///
/// Returns:
/// - `Ok(Some(backup_db_path))` on a successful copy — the caller can pass
///   this to `restore_from_backup` on migration failure.
/// - `Ok(None)` if the backup step failed *but* `CQS_MIGRATE_REQUIRE_BACKUP`
///   is unset. The migration proceeds without a recovery snapshot — the
///   warning is logged at `warn!`.
/// - `Err(StoreError::Io)` if the backup step failed *and*
///   `CQS_MIGRATE_REQUIRE_BACKUP=1`. The caller must abort the migration.
///
/// Implementation:
/// 1. `PRAGMA wal_checkpoint(FULL)` drains the WAL into the main DB so the
///    backup captures a point-in-time consistent state.
/// 2. Copy `db_path` via `atomic_replace` (fsync temp, rename, fsync parent).
/// 3. If `-wal`/`-shm` exist, copy them too (absent on cleanly-closed DBs).
pub(crate) async fn backup_before_migrate(
    pool: &SqlitePool,
    db_path: &Path,
    from: i32,
    to: i32,
) -> Result<Option<PathBuf>, StoreError> {
    let _span = tracing::info_span!("backup_before_migrate", from, to).entered();

    // Drain the WAL into the main DB so the backup is a consistent snapshot.
    // PASSIVE would skip blocked writers; FULL waits until all readers are
    // past the checkpoint. We're about to take an exclusive write txn for
    // the migration anyway — a brief wait is the right trade.
    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(FULL)")
        .execute(pool)
        .await
    {
        tracing::warn!(
            error = %e,
            "wal_checkpoint before migration backup failed (non-fatal)"
        );
    }

    let backup_db = backup_path_for(db_path, from, to);

    match copy_triplet(db_path, &backup_db) {
        Ok(()) => {
            tracing::info!(
                backup = %backup_db.display(),
                from,
                to,
                "Migration backup written"
            );
            Ok(Some(backup_db))
        }
        Err(e) => {
            let require = std::env::var(REQUIRE_BACKUP_ENV)
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if require {
                tracing::error!(
                    error = %e,
                    db = %db_path.display(),
                    "Migration backup failed and CQS_MIGRATE_REQUIRE_BACKUP=1 is set; aborting"
                );
                // Best-effort cleanup of any partial backup files.
                remove_triplet(&backup_db);
                Err(e)
            } else {
                tracing::warn!(
                    error = %e,
                    db = %db_path.display(),
                    "Migration backup failed; proceeding without snapshot \
                     (set CQS_MIGRATE_REQUIRE_BACKUP=1 to fail instead)"
                );
                remove_triplet(&backup_db);
                Ok(None)
            }
        }
    }
}

/// Restore a DB file (+ WAL/SHM sidecars) from a backup. Called on migration
/// failure to leave the DB in its pre-migrate state. Uses `atomic_replace` so
/// the restore itself is crash-safe — the caller sees pre-migrate or
/// post-migrate state, never a partially-restored file.
pub(crate) fn restore_from_backup(db_path: &Path, backup_db: &Path) -> Result<(), StoreError> {
    let _span = tracing::info_span!("restore_from_backup").entered();
    copy_triplet(backup_db, db_path)?;
    tracing::info!(
        db = %db_path.display(),
        backup = %backup_db.display(),
        "Restored DB from backup after migration failure"
    );
    Ok(())
}

/// Prune `*.bak-v*.db` files in the DB's parent directory, keeping the
/// newest `KEEP_BACKUPS` by mtime. Logs each removal at `info!`.
///
/// The WAL/SHM sidecars (if any) for a pruned backup are removed too so the
/// directory doesn't fill with orphan `.bak-v*.db-wal` files.
pub(crate) fn prune_old_backups(db_path: &Path) -> Result<(), StoreError> {
    let _span = tracing::info_span!("prune_old_backups").entered();
    let dir = match db_path.parent() {
        Some(d) => d,
        None => return Ok(()),
    };
    let stem = match db_path.file_stem() {
        Some(s) => s.to_string_lossy().into_owned(),
        None => return Ok(()),
    };
    let prefix = format!("{}.bak-v", stem);

    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) => {
            tracing::warn!(
                error = %e,
                dir = %dir.display(),
                "Failed to read DB dir for backup pruning (non-fatal)"
            );
            return Ok(());
        }
    };

    let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.starts_with(&prefix) || !name.ends_with(".db") {
            continue;
        }
        // Only consider the .db file itself for sorting; sidecars are
        // removed alongside the .db in the prune pass below.
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "Failed to stat backup file for pruning (skipping)"
                );
                continue;
            }
        };
        candidates.push((path, mtime));
    }

    // Sort newest-first. The newest KEEP_BACKUPS survive; the rest are pruned.
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in candidates.into_iter().skip(KEEP_BACKUPS) {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "Failed to remove old backup (non-fatal)"
            );
            continue;
        }
        // Remove sidecars too if they happen to exist.
        for ext in ["-wal", "-shm"] {
            let sidecar = sidecar_path(&path, ext);
            if sidecar.exists() {
                let _ = std::fs::remove_file(&sidecar);
            }
        }
        tracing::info!(path = %path.display(), "Pruned old migration backup");
    }
    Ok(())
}

/// Copy a DB file and its `-wal`/`-shm` sidecars from `src` to `dst`.
///
/// The main DB is copied first so a crash between the DB and WAL copies
/// leaves a self-consistent backup (checkpoint drained the WAL before this
/// was called). Each file goes through a same-directory temp + `atomic_replace`
/// so the destination never sees a partial write.
///
/// Absent sidecars (common on cleanly-closed DBs, SQLite removes the WAL on
/// `wal_checkpoint(TRUNCATE)`) are simply skipped — the restore path does
/// the same, and SQLite recreates them on the next open.
fn copy_triplet(src: &Path, dst: &Path) -> Result<(), StoreError> {
    // Main DB file first: the sidecars are meaningless without it.
    copy_file_atomic(src, dst)?;

    for ext in ["-wal", "-shm"] {
        let src_side = sidecar_path(src, ext);
        let dst_side = sidecar_path(dst, ext);
        if src_side.exists() {
            copy_file_atomic(&src_side, &dst_side)?;
        } else if dst_side.exists() {
            // The destination had a stale sidecar from a prior state —
            // remove it so the restored DB doesn't see inconsistent data.
            let _ = std::fs::remove_file(&dst_side);
        }
    }
    Ok(())
}

/// Atomically copy `src` -> `dst` by staging to a same-directory temp file
/// then handing off to `atomic_replace` (fsync temp + rename + fsync parent).
fn copy_file_atomic(src: &Path, dst: &Path) -> Result<(), StoreError> {
    let dir = dst.parent().unwrap_or(Path::new("."));
    let name = dst
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup.tmp".to_string());
    let suffix = crate::temp_suffix();
    let pid = std::process::id();
    let tmp_path = dir.join(format!(".{}.{}.{:016x}.tmp", name, pid, suffix));

    if let Err(e) = std::fs::copy(src, &tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(StoreError::Io(e));
    }

    if let Err(e) = crate::fs::atomic_replace(&tmp_path, dst) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(StoreError::Io(e));
    }
    Ok(())
}

/// Best-effort removal of a `.db` backup and its `-wal`/`-shm` sidecars.
/// Used when a partial backup failed and we want to clean up before returning.
fn remove_triplet(db: &Path) {
    let _ = std::fs::remove_file(db);
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar_path(db, ext));
    }
}

/// Build the path to a WAL or SHM sidecar for a given DB path.
///
/// SQLite names sidecars by appending the ext to the full DB filename (not
/// replacing the extension): `index.db` -> `index.db-wal`, `index.db-shm`.
fn sidecar_path(db: &Path, ext: &str) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_appends_suffix_to_full_filename() {
        let db = Path::new("/tmp/proj/index.db");
        assert_eq!(
            sidecar_path(db, "-wal"),
            Path::new("/tmp/proj/index.db-wal")
        );
        assert_eq!(
            sidecar_path(db, "-shm"),
            Path::new("/tmp/proj/index.db-shm")
        );
    }

    #[test]
    fn backup_path_for_builds_expected_stem_format() {
        let db = Path::new("/tmp/proj/index.db");
        let bak = backup_path_for(db, 19, 20);
        let name = bak.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("index.bak-v19-v20-"),
            "backup path should start with '<stem>.bak-v<from>-v<to>-': got {}",
            name
        );
        assert!(
            name.ends_with(".db"),
            "backup path must end in .db: {}",
            name
        );
        assert_eq!(bak.parent().unwrap(), Path::new("/tmp/proj"));
    }

    #[test]
    fn copy_file_atomic_copies_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.db");
        let dst = dir.path().join("dst.db");
        std::fs::write(&src, b"hello").unwrap();
        copy_file_atomic(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
    }

    #[test]
    fn copy_triplet_copies_all_present_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.db");
        std::fs::write(&src, b"main").unwrap();
        std::fs::write(sidecar_path(&src, "-wal"), b"wal").unwrap();
        std::fs::write(sidecar_path(&src, "-shm"), b"shm").unwrap();

        let dst = dir.path().join("d.db");
        copy_triplet(&src, &dst).unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"main");
        assert_eq!(std::fs::read(sidecar_path(&dst, "-wal")).unwrap(), b"wal");
        assert_eq!(std::fs::read(sidecar_path(&dst, "-shm")).unwrap(), b"shm");
    }

    #[test]
    fn copy_triplet_handles_missing_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.db");
        std::fs::write(&src, b"main").unwrap();
        // No -wal/-shm on src.

        let dst = dir.path().join("d.db");
        copy_triplet(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"main");
        assert!(!sidecar_path(&dst, "-wal").exists());
        assert!(!sidecar_path(&dst, "-shm").exists());
    }

    #[test]
    fn copy_triplet_removes_stale_sidecars_on_dst() {
        // If the destination has a pre-existing -wal that the source lacks,
        // restoring without clearing that sidecar would leave the restored
        // DB reading stale pages. copy_triplet must remove it.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("s.db");
        std::fs::write(&src, b"main").unwrap();

        let dst = dir.path().join("d.db");
        std::fs::write(&dst, b"old").unwrap();
        std::fs::write(sidecar_path(&dst, "-wal"), b"stale-wal").unwrap();

        copy_triplet(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"main");
        assert!(
            !sidecar_path(&dst, "-wal").exists(),
            "stale -wal must be removed"
        );
    }
}
