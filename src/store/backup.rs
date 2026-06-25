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
//! Before any DDL runs, we snapshot `index.db` to a sibling
//! `{stem}.bak-v{from}-v{to}-{unix_ts}.db` file via `VACUUM INTO`, which writes
//! a single transactionally-consistent DB file (no WAL/SHM sidecars) even
//! under concurrent writers. If any migration step fails, the DB is restored
//! from the backup via `crate::fs::atomic_replace`; the caller sees either
//! pre-migrate or post-migrate state, never a partial write.
//!
//! Backups are pruned on success: the newest two (including the one just
//! written) are kept, older ones are deleted.
//!
//! `src/hnsw/persist.rs` uses an identical save-with-backup-and-rollback
//! pattern for HNSW graph files.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use super::helpers::StoreError;
use super::Store;

impl<Mode> Store<Mode> {
    /// Take a transactionally-consistent, single-file snapshot of this store's
    /// database into `dst`, reusing the same `VACUUM INTO` path the
    /// migration backup uses (no torn-page window under a concurrent daemon
    /// writer; no `-wal`/`-shm` sidecars on the output).
    ///
    /// Sync wrapper over the async [`vacuum_into`] primitive, driven on the
    /// store's own runtime. `dst` must be on a filesystem with space for the
    /// full DB; the caller is responsible for cleaning it up (e.g. via a
    /// `TempPath`). Used by the UMAP projection to stage the embedding read
    /// onto fast local disk when the live index sits on a slow mmap fs (WSL
    /// 9P / NFS / SMB), where random-page SQLite reads collapse.
    ///
    /// A `wal_checkpoint(FULL)` runs first to bound WAL growth before the
    /// snapshot read transaction; snapshot consistency itself comes from
    /// VACUUM INTO's read transaction, not the checkpoint.
    pub fn snapshot_to(&self, dst: &Path) -> Result<(), StoreError> {
        let _span = tracing::info_span!("store_snapshot_to").entered();
        self.block_on(async {
            if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(FULL)")
                .execute(&self.pool)
                .await
            {
                tracing::warn!(
                    error = %e,
                    "wal_checkpoint before snapshot failed (non-fatal)"
                );
            }
            vacuum_into(&self.pool, dst).await
        })
    }
}

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

/// Default number of version-tagged backups to retain in the DB's parent
/// directory. The most recent `keep_backups()` (by mtime) survive; older ones
/// are pruned on every successful migrate.
///
/// Value of 3 = the backup from the current migrate run + the two prior
/// runs' backups. That gives the user two additional recovery points if a
/// migration bug is discovered after a subsequent migrate has completed.
pub(crate) const KEEP_BACKUPS_DEFAULT: usize = 3;

/// Resolve the backup-retention count honoring `CQS_MIGRATE_KEEP_BACKUPS`.
///
/// Unlike most size knobs, `0` is a valid override here — "prune every
/// backup after a successful migrate" is a legitimate choice on tight-quota
/// mounts (the just-written backup only guards the current run; once migrate
/// commits it is no longer needed). So this resolver accepts `0` verbatim
/// rather than treating it as "unset" the way `parse_env_usize` does.
/// Missing / empty / unparseable falls back to [`KEEP_BACKUPS_DEFAULT`].
pub(crate) fn keep_backups() -> usize {
    match std::env::var("CQS_MIGRATE_KEEP_BACKUPS") {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    env = "CQS_MIGRATE_KEEP_BACKUPS",
                    value = %v,
                    "Invalid env var (must be a non-negative usize), using default {KEEP_BACKUPS_DEFAULT}"
                );
                KEEP_BACKUPS_DEFAULT
            }
        },
        Err(_) => KEEP_BACKUPS_DEFAULT,
    }
}

/// Build the backup path for a given migration span.
///
/// Filename format: `{db_stem}.bak-v{from}-v{to}-{unix_ts}-{pid}-{rand_hex}.db`.
/// The filename lives in the same directory as `db_path` so the backup shares
/// the DB's filesystem — `atomic_replace`'s cheap rename path works without
/// falling back to cross-device copy.
///
/// Includes `std::process::id()` and `crate::temp_suffix()` so two CLI
/// processes running migrations concurrently (rare but realistic on a build
/// farm or under a CI matrix) cannot collide on the same backup filename.
/// Without per-process disambiguation, second-resolution timestamps (and the
/// `0` fallback on a clock anomaly) make collisions deterministic. The
/// `prune_old_backups` regex tolerates arbitrary middle content so the
/// only-newest-N-mtime sort still works.
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

/// Take a transactionally-consistent snapshot of `index.db` before a
/// migration runs.
///
/// Returns:
/// - `Ok(Some(backup_db_path))` on a successful snapshot — the caller can pass
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
/// 1. `PRAGMA wal_checkpoint(FULL)` drains the WAL into the main DB. This is
///    not what makes the snapshot consistent (VACUUM INTO does that under its
///    own read transaction); it bounds WAL growth so the post-snapshot
///    migration tx starts from a quiesced file.
/// 2. `VACUUM INTO 'backup_db'` writes a single self-contained DB file under a
///    read transaction. Unlike the previous `wal_checkpoint` + `fs::copy`
///    pair, this is consistent under concurrent writers: a live daemon
///    committing (and triggering its own autocheckpoint) between the
///    checkpoint and the snapshot can no longer rewrite pages mid-copy into a
///    torn backup. The output has no `-wal`/`-shm` sidecars — it is one file.
///
/// VACUUM INTO requires the target path to not already exist. The
/// per-process-disambiguated filename makes collisions essentially
/// impossible, but a leftover backup from an earlier crashed run could still
/// occupy the path, so any pre-existing file (and stale sidecars) is removed
/// first — matching the overwrite tolerance the prior `copy_triplet` had.
pub(crate) async fn backup_before_migrate(
    pool: &SqlitePool,
    db_path: &Path,
    from: i32,
    to: i32,
) -> Result<Option<PathBuf>, StoreError> {
    let _span = tracing::info_span!("backup_before_migrate", from, to).entered();

    // Drain the WAL into the main DB to bound WAL growth before the migration
    // tx. PASSIVE would skip blocked writers; FULL waits until all readers are
    // past the checkpoint. We're about to take an exclusive write txn for
    // the migration anyway — a brief wait is the right trade. Consistency of
    // the snapshot itself comes from VACUUM INTO's read transaction, not this.
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

    match vacuum_into(pool, &backup_db).await {
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
            // Require-backup is the default. Opt-out via
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

/// Restore the main DB from a backup. Called on migration failure to leave the
/// DB in its pre-migrate state. Uses `atomic_replace` so the write is atomic.
///
/// The backup is a `VACUUM INTO` snapshot — a single self-contained `.db` file
/// with no `-wal`/`-shm` sidecars. The restore sequence is kill-safe: the live
/// sidecars (from the failed migration) are unlinked first, then the main DB
/// is replaced from the backup, and any stale destination sidecar is cleared.
/// A kill at any point leaves (restored main + no sidecars) — SQLite recreates
/// fresh sidecars from the restored main on next open. This avoids a
/// "post-migrate WAL contaminates restored pre-migrate main" failure mode.
///
/// # Caller contract (must close pool first)
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

    // Delete any live `-wal` / `-shm` sidecars BEFORE restoring the main
    // DB. The live sidecars belong to the failed-migration state — if they
    // survive the restore, SQLite's next open replays their frames against
    // the restored main (a different "lineage"), producing silent
    // corruption the integrity check can't detect.
    //
    // Removing first makes the failure ordering safe: a kill any time after
    // this point and before the sidecar copy leaves (restored main +
    // missing sidecars) — SQLite creates fresh sidecars on next open and
    // the state is canonical pre-migrate.
    for ext in ["-wal", "-shm"] {
        let live_side = sidecar_path(db_path, ext);
        if live_side.exists() {
            if let Err(e) = std::fs::remove_file(&live_side) {
                tracing::warn!(
                    error = %e,
                    path = %live_side.display(),
                    "Failed to remove live sidecar before restore — \
                     restore continues but sidecar may be stale"
                );
            }
        }
    }

    copy_triplet(backup_db, db_path)?;
    tracing::info!(
        db = %db_path.display(),
        backup = %backup_db.display(),
        "Restored DB from backup after migration failure"
    );
    Ok(())
}

/// Prune `*.bak-v*.db` files in the DB's parent directory, keeping the
/// newest `keep_backups()` by mtime. Logs each removal at `info!`.
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
            // Return Err so the caller's `if let Err(e) =
            // prune_old_backups(...)` warn fires at the migration site. A
            // silent `Ok(())` here would let persistent permission glitches
            // accumulate `.bak-v*.db` files without bound. The caller still
            // treats this as non-fatal (the DB is at the correct version),
            // so returning Err only changes log-stream visibility, not
            // migration outcome.
            return Err(StoreError::Io(e));
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

    // Sort newest-first. The newest `keep_backups()` survive; the rest are pruned.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.1));
    for (path, _) in candidates.into_iter().skip(keep_backups()) {
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

/// Snapshot the live DB into `dst` via `VACUUM INTO`.
///
/// `VACUUM INTO` reads the source database under its own read transaction and
/// writes a fresh, defragmented, single-file copy. Because the read
/// transaction provides a stable snapshot, the result is consistent even while
/// other connections (a live `cqs watch` daemon) keep committing — there is no
/// torn-page window the old `wal_checkpoint` + `fs::copy` had. The output is a
/// single `.db` file with no `-wal`/`-shm` sidecars.
///
/// SQLite requires the `INTO` target to not already exist. Any pre-existing
/// file at `dst` (and stale sidecars from an aborted prior run) is removed
/// first so this is robust to leftover backups; the per-process-disambiguated
/// filename makes a genuine concurrent collision essentially impossible.
async fn vacuum_into(pool: &SqlitePool, dst: &Path) -> Result<(), StoreError> {
    // Clear any leftover file at the target — VACUUM INTO refuses to overwrite.
    if dst.exists() {
        remove_triplet(dst);
    }

    // Bind the destination path as a parameter so paths with quotes/spaces are
    // handled without manual escaping. SQLite accepts a bound expression for
    // the VACUUM INTO target.
    let dst_str = dst.to_string_lossy();
    sqlx::query("VACUUM INTO ?")
        .bind(dst_str.as_ref())
        .execute(pool)
        .await?;
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
    use crate::store::ModelInfo;
    use std::assert_matches;

    /// `Store::snapshot_to` produces a single-file, integrity-clean snapshot
    /// (no `-wal`/`-shm` sidecars) that re-opens as a valid DB carrying the
    /// source's metadata. Pins the public snapshot primitive the UMAP slow-fs
    /// staging path relies on, through the `Store` method (the `vacuum_into`
    /// tests below cover the async primitive directly).
    #[test]
    fn store_snapshot_to_produces_consistent_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).expect("open store");
        store
            .init(&ModelInfo::new("test/model", 8))
            .expect("init store");

        let snapshot = dir.path().join("snapshot.db");
        store.snapshot_to(&snapshot).expect("snapshot must succeed");

        assert!(snapshot.exists(), "snapshot file must exist");
        assert!(
            !sidecar_path(&snapshot, "-wal").exists(),
            "snapshot must not carry a -wal sidecar"
        );
        assert!(
            !sidecar_path(&snapshot, "-shm").exists(),
            "snapshot must not carry a -shm sidecar"
        );

        // The snapshot re-opens and reports the source's model/dim.
        let reopened = Store::open(&snapshot).expect("snapshot must re-open");
        assert_eq!(reopened.dim(), 8, "snapshot must preserve the source dim");
    }

    /// `keep_backups()` returns the compiled default when unset, the env
    /// value when set (including `0`, which is a valid "prune all" choice),
    /// and the default on a garbage value.
    #[test]
    fn keep_backups_honors_env_including_zero() {
        std::env::remove_var("CQS_MIGRATE_KEEP_BACKUPS");
        assert_eq!(keep_backups(), KEEP_BACKUPS_DEFAULT);
        std::env::set_var("CQS_MIGRATE_KEEP_BACKUPS", "7");
        assert_eq!(keep_backups(), 7);
        // Zero is honored verbatim, unlike the parse_env_usize "reject zero".
        std::env::set_var("CQS_MIGRATE_KEEP_BACKUPS", "0");
        assert_eq!(keep_backups(), 0);
        std::env::set_var("CQS_MIGRATE_KEEP_BACKUPS", "garbage");
        assert_eq!(keep_backups(), KEEP_BACKUPS_DEFAULT);
        std::env::remove_var("CQS_MIGRATE_KEEP_BACKUPS");
    }

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

    /// `restore_from_backup` must delete the live `-wal` and `-shm` BEFORE
    /// restoring the main DB. The live sidecars belong to the
    /// failed-migration state; if they survive the restore, SQLite's next
    /// open replays their frames against the restored main and silently
    /// corrupts the database.
    ///
    /// This test arranges a backup with no sidecars (the cleanly-closed
    /// case) + a live state with a stale -wal/-shm. After
    /// `restore_from_backup` runs, both live sidecars must be gone (deleted
    /// before restore) and the main must match the backup.
    #[test]
    fn restore_from_backup_deletes_live_sidecars_before_restore() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("snapshot.bak.db");
        std::fs::write(&backup, b"pre-migrate-main").unwrap();
        // Backup has no -wal/-shm (cleanly checkpointed before backup).

        let live = dir.path().join("index.db");
        std::fs::write(&live, b"failed-migration-main").unwrap();
        // Live state has stale sidecars from the failed migration's tx.
        std::fs::write(sidecar_path(&live, "-wal"), b"failed-migration-wal").unwrap();
        std::fs::write(sidecar_path(&live, "-shm"), b"failed-migration-shm").unwrap();

        restore_from_backup(&live, &backup).unwrap();

        assert_eq!(
            std::fs::read(&live).unwrap(),
            b"pre-migrate-main",
            "main must reflect backup contents"
        );
        assert!(
            !sidecar_path(&live, "-wal").exists(),
            "live -wal from failed migration must be removed (would corrupt restored main on next open)"
        );
        assert!(
            !sidecar_path(&live, "-shm").exists(),
            "live -shm from failed migration must be removed"
        );
    }

    /// Sibling case: backup has its own sidecars (not cleanly closed at
    /// snapshot time). The live sidecars must still be removed first;
    /// the backup's sidecars then land via `copy_triplet`.
    #[test]
    fn restore_from_backup_replaces_live_sidecars_with_backup_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("snapshot.bak.db");
        std::fs::write(&backup, b"pre-migrate-main").unwrap();
        std::fs::write(sidecar_path(&backup, "-wal"), b"pre-migrate-wal").unwrap();
        std::fs::write(sidecar_path(&backup, "-shm"), b"pre-migrate-shm").unwrap();

        let live = dir.path().join("index.db");
        std::fs::write(&live, b"failed-migration-main").unwrap();
        std::fs::write(sidecar_path(&live, "-wal"), b"failed-migration-wal").unwrap();
        std::fs::write(sidecar_path(&live, "-shm"), b"failed-migration-shm").unwrap();

        restore_from_backup(&live, &backup).unwrap();

        assert_eq!(std::fs::read(&live).unwrap(), b"pre-migrate-main");
        assert_eq!(
            std::fs::read(sidecar_path(&live, "-wal")).unwrap(),
            b"pre-migrate-wal",
            "backup -wal should land at live path"
        );
        assert_eq!(
            std::fs::read(sidecar_path(&live, "-shm")).unwrap(),
            b"pre-migrate-shm"
        );
    }

    // ============================================================================
    // `CQS_MIGRATE_REQUIRE_BACKUP` defaults to on.
    //
    // Serialised via a module-local mutex because `std::env::set_var` is
    // process-global; running the default-on and opt-out cases in parallel
    // would race on the env var.
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

    /// Build an on-disk WAL pool for the VACUUM INTO snapshot tests.
    async fn wal_pool(db_path: &Path) -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(db_path)
                    .create_if_missing(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    /// `backup_before_migrate` must produce a single, self-contained,
    /// integrity-clean snapshot even while another connection commits
    /// concurrently. The old `wal_checkpoint` + `fs::copy` path could tear the
    /// backup when a live daemon's autocheckpoint rewrote pages mid-copy;
    /// VACUUM INTO reads under a stable transaction so the snapshot is
    /// consistent and has no `-wal`/`-shm` sidecars.
    #[test]
    fn vacuum_into_snapshot_is_consistent_under_concurrent_writes() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");

        rt.block_on(async {
            let pool = wal_pool(&db_path).await;
            // Seed some rows so the snapshot has content to verify.
            for i in 0..200i64 {
                sqlx::query("INSERT INTO t (id, v) VALUES (?, ?)")
                    .bind(i)
                    .bind(format!("seed-{i}"))
                    .execute(&pool)
                    .await
                    .unwrap();
            }

            // Concurrent writer: hammers the DB on a second connection while
            // the backup runs, so a torn-copy path would be exercised.
            let writer_pool = pool.clone();
            let writer = tokio::spawn(async move {
                for i in 1000..2000i64 {
                    let _ = sqlx::query("INSERT OR REPLACE INTO t (id, v) VALUES (?, ?)")
                        .bind(i)
                        .bind(format!("concurrent-{i}"))
                        .execute(&writer_pool)
                        .await;
                }
            });

            let result = backup_before_migrate(&pool, &db_path, 27, 28)
                .await
                .expect("backup must succeed");
            let backup = result.expect("backup path returned");

            let _ = writer.await;
            pool.close().await;

            // The snapshot is one file — no -wal/-shm sidecars.
            assert!(backup.exists(), "snapshot file must exist");
            assert!(
                !sidecar_path(&backup, "-wal").exists(),
                "VACUUM INTO snapshot must not have a -wal sidecar"
            );
            assert!(
                !sidecar_path(&backup, "-shm").exists(),
                "VACUUM INTO snapshot must not have a -shm sidecar"
            );

            // Open the snapshot and run an integrity check.
            let snap_pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&backup)
                        .read_only(true),
                )
                .await
                .unwrap();
            let (integrity,): (String,) = sqlx::query_as("PRAGMA integrity_check")
                .fetch_one(&snap_pool)
                .await
                .unwrap();
            assert_eq!(integrity, "ok", "snapshot integrity_check must be ok");

            // The seeded rows are present in the snapshot.
            let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM t WHERE v LIKE 'seed-%'")
                .fetch_one(&snap_pool)
                .await
                .unwrap();
            assert_eq!(count, 200, "all seeded rows must be in the snapshot");
            snap_pool.close().await;
        });
    }

    /// `vacuum_into` tolerates a pre-existing file at the target path (a
    /// leftover from an aborted prior run) — SQLite refuses to overwrite, so
    /// the helper clears it first.
    #[test]
    fn vacuum_into_overwrites_preexisting_target() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let backup = dir.path().join("leftover.db");

        rt.block_on(async {
            let pool = wal_pool(&db_path).await;
            sqlx::query("INSERT INTO t (id, v) VALUES (1, 'x')")
                .execute(&pool)
                .await
                .unwrap();

            // A stale file already occupies the target path.
            std::fs::write(&backup, b"garbage-leftover").unwrap();
            std::fs::write(sidecar_path(&backup, "-wal"), b"stale").unwrap();

            vacuum_into(&pool, &backup)
                .await
                .expect("vacuum_into must overwrite a pre-existing target");
            pool.close().await;

            // The leftover sidecar is gone and the snapshot is a valid DB.
            assert!(!sidecar_path(&backup, "-wal").exists());
            let snap_pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&backup)
                        .read_only(true),
                )
                .await
                .unwrap();
            let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM t")
                .fetch_one(&snap_pool)
                .await
                .unwrap();
            assert_eq!(count, 1);
            snap_pool.close().await;
        });
    }

    /// When the env var is **unset**, a backup failure is promoted to `Err`
    /// (unset = require = hard error), so the destructive v18→v19 migration
    /// never runs without a recovery snapshot.
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
            // Point the DB path inside a directory that does NOT exist so
            // the backup's `VACUUM INTO` target can't be created — this
            // exercises the Err branch of backup_before_migrate.
            let missing_db = dir.path().join("nonexistent_dir").join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            // VACUUM INTO into a missing directory surfaces a SQLite error
            // (StoreError::Database); the contract under default require-backup
            // is simply "backup failed → Err", not a specific variant.
            match result {
                Err(_) => {}
                Ok(v) => panic!(
                    "expected Err when CQS_MIGRATE_REQUIRE_BACKUP is unset \
                     and backup fails, got Ok({:?})",
                    v
                ),
            }
        });
    }

    /// When the user sets `CQS_MIGRATE_REQUIRE_BACKUP=0`, a backup failure is
    /// downgraded to `Ok(None)` and the migration proceeds without a
    /// snapshot. The documented escape hatch for tight-quota filesystems and
    /// CI that can rebuild from source.
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
            let missing_db = dir.path().join("nonexistent_dir").join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            assert_matches!(
                result,
                Ok(None),
                "backup failure with CQS_MIGRATE_REQUIRE_BACKUP=0 opt-out must yield Ok(None)"
            );
        });
    }

    /// `CQS_MIGRATE_REQUIRE_BACKUP=false` (the string, not `0`) also opts
    /// out — the env-var parse is case-insensitive.
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
            let missing_db = dir.path().join("nonexistent_dir").join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            assert!(
                matches!(result, Ok(None)),
                "CQS_MIGRATE_REQUIRE_BACKUP=FALSE must opt out, got {:?}",
                result
            );
        });
    }

    /// Any value that is not `0`/`false` (e.g. `1`, `true`, or junk) keeps
    /// the default require-backup behaviour, so a typo doesn't turn into a
    /// silent data-loss risk.
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
            let missing_db = dir.path().join("nonexistent_dir").join("does_not_exist.db");

            let result = backup_before_migrate(&pool, &missing_db, 18, 19).await;
            assert!(
                result.is_err(),
                "non-opt-out env values must keep the default hard-error \
                 behaviour, got {:?}",
                result
            );
        });
    }
}
