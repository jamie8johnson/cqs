//! Shared file-freshness key for `index.db` staleness detection.
//!
//! A long-lived reader (the daemon's `BatchContext`, a cached
//! `CrossProjectContext`, or an LRU-resident `ReferenceIndex`) caches data
//! derived from an `index.db` and must notice when another process or thread
//! rewrites that file so it can drop the stale derivation. Every such reader
//! needs the SAME freshness key — so it lives here, single-source, rather than
//! being re-derived at each site (where a hardening applied to one copy silently
//! left the others behind).
//!
//! Two discriminators, combined, cover the rewrite shapes:
//!
//! 1. [`FileIdentity`] — `(dev, inode, size, mtime)` on unix, `(size, mtime)`
//!    elsewhere. Catches replacement-via-rename and checkpoint (the inode/size
//!    move) and in-place size/mtime changes.
//! 2. [`DataVersionProbe`] — a long-lived `PRAGMA data_version` connection.
//!    Catches WAL-mode incremental commits that land in `index.db-wal` and
//!    leave the main file's identity untouched until checkpoint — the
//!    false-negative class identity alone cannot see.

use std::path::Path;
use std::time::SystemTime;

use tokio::runtime::Runtime;

/// Opaque identity of an `index.db` file used to detect that it has been
/// replaced or rewritten between two observations.
///
/// Combines inode (unix), size, and mtime. This catches:
///
/// - **Replacement via rename** (e.g. `cqs index --force` writes a fresh
///   `index.db.tmp` then renames it over `index.db`): the new inode
///   differs, so the identity changes even if size/mtime happened to
///   match.
/// - **Checkpoint after WAL writes**: a `wal_checkpoint(TRUNCATE)` folds the
///   WAL back into the main file, moving size and mtime.
/// - **In-place size change**: size differs.
/// - **Overwrite that kept the size**: mtime differs (modulo the
///   filesystem's mtime resolution).
///
/// ## Why not mtime alone?
///
/// WSL DrvFS / NTFS report mtime at 1-second resolution. A tight
/// `cqs index --force` followed by a daemon query burst could share the
/// same mtime bucket, causing a reader to keep serving results from the
/// orphaned inode. Mixing in inode and size closes that sub-second race:
/// the rename-over gives a new inode immediately, regardless of whether the
/// mtime ticked. A same-size in-place rewrite that lands a new inode
/// (checkpoint-truncate, copy-rename) is caught the same way.
///
/// ## What identity does NOT catch
///
/// A WAL-mode incremental commit writes to `index.db-wal` and leaves the main
/// file's identity unchanged until checkpoint. [`DataVersionProbe`] covers that
/// blind spot; pair the two for full coverage.
///
/// On non-unix platforms the inode fields are omitted and the struct falls back
/// to `(size, mtime)`; replacement on Windows still changes the mtime and/or
/// the size, so this is weaker than unix but strictly better than mtime alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    inode: u64,
    size: u64,
    mtime: Option<SystemTime>,
}

impl FileIdentity {
    /// Read the identity fields for `path`, returning `None` if the
    /// metadata stat fails (path missing, permission denied, etc.).
    ///
    /// `None` means "can't tell" — a caller comparing against a captured
    /// identity should treat that as "keep the cached value" rather than
    /// forcing a reload on a transient glitch.
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        // mtime is best-effort — some exotic filesystems don't record
        // it. Falling back to `None` here still leaves inode + size as
        // useful discriminators.
        let mtime = meta.modified().ok();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                dev: meta.dev(),
                inode: meta.ino(),
                size: meta.len(),
                mtime,
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {
                size: meta.len(),
                mtime,
            })
        }
    }
}

/// Long-lived `PRAGMA data_version` probe connection.
///
/// `data_version` is a per-connection counter that SQLite bumps when *another*
/// connection (including one in the same process — e.g. the watch loop's
/// read-write Store, or a concurrent `cqs ref update`) commits a change to the
/// database. It moves on WAL commits that never touch the main `index.db` file,
/// which is exactly the blind spot of the [`FileIdentity`] check: under WAL,
/// incremental reindex writes land in `index.db-wal` and the main file's
/// identity is unchanged until checkpoint.
///
/// The classic pitfall: the counter is only meaningful when queried repeatedly
/// on the SAME connection — a fresh connection per check re-baselines every
/// time and never observes a change. A pool (which hands out a different
/// connection each acquire) has the same defect. So the connection here must be
/// a dedicated, long-lived handle — held as long as the reader caching from
/// `index.db`, and re-opened when the file is replaced via rename-over (the old
/// fd then points at the orphaned inode and its data_version never moves
/// again).
pub struct DataVersionProbe {
    conn: sqlx::SqliteConnection,
    /// Last observed `PRAGMA data_version` value on `conn`.
    last: i64,
}

impl DataVersionProbe {
    /// Open a fresh probe connection against `index_path` and read its
    /// baseline `PRAGMA data_version`. Returns `None` (with a `warn!`) when
    /// the open or the query fails — staleness detection then falls back to
    /// identity-only rather than panicking or silently skipping the check.
    ///
    /// `rt` drives the async sqlx open; callers pass the runtime their Store
    /// already owns (`Arc::clone(store.runtime())`) so the probe stays on the
    /// same worker pool.
    pub fn open(rt: &Runtime, index_path: &Path) -> Option<Self> {
        use sqlx::ConnectOptions;
        let result = rt.block_on(async {
            // Mirror the Store's read-only open shape (filename + read_only +
            // WAL) so the probe sees the same journal-mode view of the DB.
            let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(index_path)
                .read_only(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .connect()
                .await?;
            let last: i64 = sqlx::query_scalar("PRAGMA data_version")
                .fetch_one(&mut conn)
                .await?;
            Ok::<_, sqlx::Error>(DataVersionProbe { conn, last })
        });
        match result {
            Ok(probe) => Some(probe),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %index_path.display(),
                    "Failed to open data_version probe — falling back to identity-only staleness detection"
                );
                None
            }
        }
    }

    /// Query `PRAGMA data_version` on this long-lived connection and compare
    /// against the last observed value, updating it. Returns `true` when
    /// another connection has committed since the previous observation — WAL or
    /// not.
    ///
    /// On query failure: warns and returns `Err` so the caller can drop the
    /// probe (the next check re-opens it, re-baselining). Never panics.
    pub fn changed(&mut self, rt: &Runtime) -> Result<bool, sqlx::Error> {
        let v: i64 = rt.block_on(
            sqlx::query_scalar::<_, i64>("PRAGMA data_version").fetch_one(&mut self.conn),
        )?;
        let changed = v != self.last;
        self.last = v;
        Ok(changed)
    }

    /// Explicitly close the underlying connection so sqlite finalizes the
    /// handle now instead of whenever Drop gets around to it. Best-effort —
    /// the fd is dead-weight either way. Called when re-opening against a
    /// replaced file (rename-over).
    pub fn close(self, rt: &Runtime) {
        use sqlx::Connection;
        let _ = rt.block_on(self.conn.close());
    }
}

#[cfg(test)]
mod tests {
    /// Completeness guard for the `index.db` freshness key: every long-lived
    /// reader that caches data derived from an index must route its staleness
    /// check through the shared [`super::FileIdentity`] — and none may
    /// re-introduce the bare `(mtime, size)` / `(SystemTime, u64)` tuple key
    /// that the two stragglers carried before centralization.
    ///
    /// Asserted on source text (not behavior): the failing case is a fourth
    /// site that hand-rolls its own weaker key. A behavioral test can't see a
    /// site it doesn't know to exercise; this enumerates the peer-set and
    /// fails when a member diverges, so the NEXT hardening of `FileIdentity`
    /// propagates by construction rather than leaving a silent straggler.
    #[test]
    fn freshness_discriminator_sites_route_through_shared_key() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");

        // The peer-set: every reader file that holds a long-lived `index.db`
        // freshness key. Adding a new such reader means adding it here (and
        // routing it through `FileIdentity`) — that is the intended friction.
        let sites = [
            "src/store/calls/cross_project.rs",
            "src/reference.rs",
            "src/cli/batch/mod.rs",
        ];

        // The forbidden shape: a freshness helper whose return type is the
        // bare `(SystemTime, u64)` tuple (the pre-centralization
        // `stat_identity` signature, in either qualification). String-literals
        // and doc comments are inert to the compiler, but they shouldn't carry
        // this either — a reviewer copying one would re-seed the straggler.
        let forbidden = [
            "Option<(std::time::SystemTime, u64)>",
            "Option<(SystemTime, u64)>",
        ];

        let mut offenders: Vec<String> = Vec::new();
        for rel in sites {
            let path = std::path::Path::new(manifest_dir).join(rel);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

            // (a) Must reference the shared key by name.
            if !src.contains("FileIdentity") {
                offenders.push(format!(
                    "{rel}: a freshness-discriminator file no longer mentions \
                     `FileIdentity` — it must route its staleness key through \
                     the shared constructor"
                ));
            }
            // (b) Must not re-introduce the bare `(mtime, size)` tuple key.
            for bad in forbidden {
                if src.contains(bad) {
                    offenders.push(format!(
                        "{rel}: contains the bare tuple freshness key `{bad}` — \
                         use `FileIdentity::from_path` instead so the hardened \
                         key (inode + dev + size + mtime, plus the data_version \
                         probe) is single-source"
                    ));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "freshness-key completeness guard failed:\n{}",
            offenders.join("\n")
        );
    }
}
