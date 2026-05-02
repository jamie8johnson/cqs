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

/// Env var that controls whether a failed migration-time DB backup is a hard
/// error (default) or a warn-and-continue.
///
/// Default is **on** (require a backup). The v18→v19 migration permanently
/// drops orphan `sparse_vectors` rows via an `INSERT … INNER JOIN chunks`
/// filter and then `DROP TABLE sparse_vectors`; a subsequent non-transactional
/// commit-time I/O failure with no backup on disk leaves the user with a
/// partially-migrated DB and no recovery path short of `cqs index --force`.
/// Opt-in to destructive behaviour is the standard stance — users who truly
/// can't spare disk (tight-quota WSL 9P mounts, read-only parent dirs, CI
/// rebuilding from source) can set `CQS_MIGRATE_REQUIRE_BACKUP=0` to proceed
/// without a snapshot and accept the data-loss risk.
///
/// Accepted values for opt-out: `0` or `false` (case-insensitive). Any other
/// value (including unset) keeps the default hard-error behaviour.
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
/// Filename format: `{db_stem}.bak-v{from}-v{to}-{unix_ts}-{pid}-{rand_hex}.db`.
/// The filename lives in the same directory as `db_path` so the backup shares
/// the DB's filesystem — `atomic_replace`'s cheap rename path works without
/// falling back to cross-device copy.
///
/// DS-V1.33-5: includes `std::process::id()` and `crate::temp_suffix()` so
/// two CLI processes running migrations concurrently (rare but realistic on
/// a build farm or under a CI matrix) cannot collide on the same backup
/// filename. Without per-process disambiguation, second-resolution timestamps
/// (and the `0` fallback on a clock anomaly) make collisions deterministic.
/// The `prune_old_backups` regex tolerates arbitrary middle content so the
/// only-newest-N-mtime sort still works without changes.
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
    let pid = std::process::id();
    let rand_hex = crate::temp_suffix();
    dir.join(format!(
        "{}.bak-v{}-v{}-{}-{}-{:016x}.db",
        stem, from, to, ts, pid, rand_hex
    ))
}

/// Take a filesystem snapshot of `index.db` (+ `-wal`/`-shm` if present)
/// before a migration runs.
///
/// Returns:
/// - `Ok(Some(backup_db_path))` on a successful copy — the caller can pass
///   this to `restore_from_backup` on migration failure.
/// - `Ok(None)` if the backup step failed *and* the user has explicitly set
///   `CQS_MIGRATE_REQUIRE_BACKUP=0` (opt-out). The migration proceeds
///   without a recovery snapshot — the warning is logged at `warn!`.
/// - `Err(StoreError::Io)` if the backup step failed and the env var is
///   unset or anything other than `0`/`false`. The caller must abort the
///   migration. This is the default stance: destructive migrations (v18→v19
///   drops the old `sparse_vectors` table) without a recovery snapshot are
///   a data-loss hazard on a subsequent commit failure.
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
            // DS2-8: require-backup is now the default. Opt-out via
            // CQS_MIGRATE_REQUIRE_BACKUP=0 for environments where the user
            // accepts the data-loss risk (e.g. tight-quota WSL 9P mounts, CI
            // rebuilding from source). The v18→v19 migration is destructive
            // (INNER JOIN INSERT + DROP TABLE sparse_vectors), so the default
            // must protect the DB when we can't take a snapshot.
            let allow_no_backup = std::env::var(REQUIRE_BACKUP_ENV)
                .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
                .unwrap_or(false);
            // Best-effort cleanup of any partial backup files in both branches.
            remove_triplet(&backup_db);
            if allow_no_backup {
                tracing::warn!(
                    error = %e,
                    db = %db_path.display(),
                    "Migration backup failed; proceeding without snapshot \
                     because CQS_MIGRATE_REQUIRE_BACKUP=0 is set \
                     (data-loss risk on subsequent migration failure)"
                );
                Ok(None)
            } else {
                tracing::error!(
                    error = %e,
                    db = %db_path.display(),
                    "Migration backup failed; aborting to protect DB. \
                     Set CQS_MIGRATE_REQUIRE_BACKUP=0 to proceed without a \
                     snapshot and accept the data-loss risk on a subsequent \
                     migration failure."
                );
                Err(e)
            }
        }
    }
}

/// Restore a DB file (+ WAL/SHM sidecars) from a backup. Called on migration
/// failure to leave the DB in its pre-migrate state. Uses `atomic_replace` so
/// the restore itself is crash-safe — the caller sees pre-migrate or
/// post-migrate state, never a partially-restored file.
///
/// # P2.59 — caller contract (must close pool first)
///
/// **Caller MUST close every SQLite pool open against `db_path` BEFORE
/// calling.** SQLite's in-process pool holds file descriptors against the
/// old inode that the atomic replace unlinks; queries through those
/// descriptors after restore see the unlinked-old inode while new processes
/// (and any pool reopened after this returns) see the restored backup.
/// Two-state divergence is silent — the WAL/SHM sidecars copied alongside
/// the main DB land on the new inode while the live pool's mmap'd sidecars
/// belong to the old.
///
/// In-process callers that need to keep working after restore must drop the
/// returning `Store` and reopen a fresh handle.
///
/// Enforcement: the only production caller is [`super::migrations::migrate`],
/// which takes its `SqlitePool` by value and runs
/// `PRAGMA wal_checkpoint(TRUNCATE)` + `pool.close().await` before invoking
/// this function on the failure path. Test coverage:
/// `test_migrate_failure_closes_pool_before_restore_no_phantom_inode` opens
/// a fresh pool against the same path after a forced migration failure and
/// asserts the restored bytes are visible — proves the file replace landed
/// on the inode the path resolves to, not an orphaned descriptor.
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
    candidates.sort_by_key(|c| std::cmp::Reverse(c.1));
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

    // ============================================================================
    // DS2-8: `CQS_MIGRATE_REQUIRE_BACKUP` defaults to on.
    //
    // Serialised via a module-local mutex because `std::env::set_var` is
    // process-global; running the two default-on and opt-out cases in
    // parallel would race on the env var.
    // ============================================================================

    /// Process-global mutex for the CQS_MIGRATE_REQUIRE_BACKUP env-var tests.
    /// `std::env::set_var` mutates process-global state, so two tests that
    /// flip the env var in opposite directions must not run in parallel.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restore `CQS_MIGRATE_REQUIRE_BACKUP` to whatever value (including
    /// "unset") it had before the test started. RAII via Drop so a panic
    /// inside the test body doesn't leak a bogus value into neighbours.
    struct EnvGuard {
        prev: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                prev: std::env::var(REQUIRE_BACKUP_ENV).ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(REQUIRE_BACKUP_ENV, v),
                None => std::env::remove_var(REQUIRE_BACKUP_ENV),
            }
        }
    }

    /// Build a fresh in-memory SqlitePool for tests that don't care about
    /// persistent state — used to exercise `backup_before_migrate`'s
    /// signature without creating a real on-disk DB.
    async fn in_memory_pool() -> SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap()
    }

    /// DS2-8 happy path: when the env var is **unset**, a backup failure is
    /// promoted to `Err`. The previous default was "warn and proceed", which
    /// silently ran the destructive v18→v19 migration without a recovery
    /// snapshot; the fix flips that so unset = require = hard error.
    #[test]
    fn backup_failure_with_env_unset_returns_err_by_default() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _guard = EnvGuard::new();
        std::env::remove_var(REQUIRE_BACKUP_ENV);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = in_memory_pool().await;
            let dir = tempfile::tempdir().unwrap();
            // Point at a DB path whose source file does NOT exist — the
            // copy step inside copy_triplet will fail with ENOENT, which
            // exercises the Err branch of backup_before_migrate.
            let missing_db = dir.path().join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            match result {
                Err(StoreError::Io(_)) => {}
                Ok(v) => panic!(
                    "expected Err(Io) when CQS_MIGRATE_REQUIRE_BACKUP is unset \
                     and backup fails, got Ok({:?})",
                    v
                ),
                Err(other) => panic!("expected Err(Io), got: {:?}", other),
            }
        });
    }

    /// DS2-8 opt-out path: when the user sets `CQS_MIGRATE_REQUIRE_BACKUP=0`,
    /// a backup failure is downgraded to `Ok(None)` and the migration proceeds
    /// without a snapshot. Matches the documented escape hatch for tight-quota
    /// filesystems and CI that can rebuild from source.
    #[test]
    fn backup_failure_with_env_opt_out_returns_ok_none() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _guard = EnvGuard::new();
        std::env::set_var(REQUIRE_BACKUP_ENV, "0");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = in_memory_pool().await;
            let dir = tempfile::tempdir().unwrap();
            let missing_db = dir.path().join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            match result {
                Ok(None) => {}
                Ok(Some(p)) => panic!(
                    "expected Ok(None) on backup failure with opt-out, got Ok(Some({}))",
                    p.display()
                ),
                Err(e) => panic!(
                    "expected Ok(None) when CQS_MIGRATE_REQUIRE_BACKUP=0 is set, \
                     got Err({:?})",
                    e
                ),
            }
        });
    }

    /// DS2-8: `CQS_MIGRATE_REQUIRE_BACKUP=false` (the string, not `0`) should
    /// also opt out — the env-var parse is case-insensitive. Guards against a
    /// regression where only the literal `0` was accepted.
    #[test]
    fn backup_failure_with_env_false_string_returns_ok_none() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _guard = EnvGuard::new();
        std::env::set_var(REQUIRE_BACKUP_ENV, "FALSE");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = in_memory_pool().await;
            let dir = tempfile::tempdir().unwrap();
            let missing_db = dir.path().join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            assert!(
                matches!(result, Ok(None)),
                "CQS_MIGRATE_REQUIRE_BACKUP=FALSE must opt out, got {:?}",
                result
            );
        });
    }

    /// DS2-8: any value that is not `0`/`false` (e.g. `1`, `true`, or junk)
    /// keeps the default require-backup behaviour. This protects against a
    /// typo turning into silent data-loss risk.
    #[test]
    fn backup_failure_with_env_garbage_value_still_returns_err() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _guard = EnvGuard::new();
        std::env::set_var(REQUIRE_BACKUP_ENV, "yes-please");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = in_memory_pool().await;
            let dir = tempfile::tempdir().unwrap();
            let missing_db = dir.path().join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            assert!(
                matches!(result, Err(StoreError::Io(_))),
                "non-opt-out env values must keep the default hard-error \
                 behaviour, got {:?}",
                result
            );
        });
    }
}
