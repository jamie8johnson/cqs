//! Periodic full-tree reconciliation. (#1182 — Layer 2)
//!
//! `cqs watch` keeps the index live by reacting to filesystem events from
//! `notify::Watcher` (inotify on Linux, poll on WSL). Three classes of
//! changes routinely *miss* events:
//!
//!   1. **Bulk git operations** — `checkout`, `reset --hard`, `merge`,
//!      `rebase`. These touch many files in one syscall burst; inotify
//!      coalesces or drops events under load.
//!   2. **WSL `/mnt/c/`** — the 9P bridge is lossy. Even single-file saves
//!      sometimes don't reach the watcher; bulk operations almost never do.
//!   3. **External writes** — build artifacts, code generators,
//!      copy-from-script. Not always under the watched path or with
//!      predictable mtime semantics.
//!
//! This module closes those classes by walking the working tree on a
//! cadence (default 30 s, configurable via `CQS_WATCH_RECONCILE_SECS`)
//! and queueing divergent files into the existing
//! `WatchState::pending_files` set. The next debounce tick drains the
//! queue through `process_file_changes`, so reconciliation reuses every
//! existing reindex correctness path — no parallel code branch.
//!
//! ## Divergence kinds
//!
//! Three classes of disk vs. index disagreement:
//!
//!   - **Added**: file exists on disk, no chunks indexed for it. Queue.
//!   - **Modified**: file exists on disk, indexed `source_mtime` is older
//!     than the disk mtime. Queue.
//!   - **Missing**: indexed but not on disk. **Not** this module's
//!     concern — handled by `run_daemon_periodic_gc`'s
//!     `Store::prune_missing` pass on the same idle-tick mechanism.
//!
//! ## Cost
//!
//! Per tick: one tree walk + one `SELECT DISTINCT origin, source_mtime`
//! (~3-5k rows for a typical repo) + per-file `metadata()` for divergent
//! candidates. Walk dominates: sub-second on Linux SSD, ~1 s on WSL 9P
//! for a 17k-chunk corpus. The walk only happens when the watch loop has
//! been idle for `daemon_periodic_gc_idle_secs()` (default 60 s), so a
//! long burst of edits never triggers a reconcile mid-burst.
//!
//! Disable with `CQS_WATCH_RECONCILE=0`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cqs::parser::Parser as CqParser;
use cqs::store::Store;

/// Walk the project tree and queue any files that diverge from the
/// indexed state into `pending_files`. Returns the count of files queued
/// so the watch loop can log a summary line.
///
/// Returns 0 if the disk walk fails or the indexed-origins query fails —
/// reconciliation is best-effort by design. Errors land in `tracing::warn!`
/// so operators can spot persistent failures in `journalctl`.
///
/// `pending_files` is the watch loop's debounce queue; once a file is
/// inserted it gets reindexed on the next idle tick like any other event-
/// driven change. The watch loop's existing dedup (`HashSet`) means
/// queueing a file already in `pending_files` is free.
pub(super) fn run_daemon_reconcile(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    no_ignore: bool,
    pending_files: &mut HashSet<PathBuf>,
) -> usize {
    let _span = tracing::info_span!("daemon_reconcile").entered();

    // Walk disk → set of relative paths visible to indexing.
    let exts = parser.supported_extensions();
    let disk_files = match cqs::enumerate_files(root, &exts, no_ignore) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Reconcile: enumerate_files failed");
            return 0;
        }
    };

    // One SELECT pulls every indexed source-file origin + its stored
    // mtime. Map keyed by origin string for cheap lookups in the loop.
    let indexed = match store.indexed_file_origins() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Reconcile: indexed_file_origins failed");
            return 0;
        }
    };

    let mut added = 0usize;
    let mut modified = 0usize;
    let mut queued = 0usize;
    for rel in disk_files {
        // Stored origins are typically relative; normalize to forward
        // slashes for cross-platform matching parity with the rest of the
        // store layer.
        let origin = rel.to_string_lossy().replace('\\', "/");
        match indexed.get(&origin) {
            None => {
                // ADDED: no chunks for this file in the index. Queue.
                if pending_files.insert(rel.clone()) {
                    added += 1;
                    queued += 1;
                }
            }
            Some(stored_mtime) => {
                // MODIFIED: same path indexed, but mtime moved forward.
                // `None` stored mtime → treat as stale (legacy schema).
                let lookup_path: PathBuf = if rel.is_absolute() {
                    rel.clone()
                } else {
                    root.join(&rel)
                };
                let disk_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(cqs::duration_to_mtime_millis),
                    Err(_) => None,
                };
                let needs_reindex = match (stored_mtime, disk_mtime) {
                    (Some(stored), Some(disk)) => disk > *stored,
                    (None, _) => true,        // legacy/null stored mtime
                    (Some(_), None) => false, // can't read disk mtime → leave to GC
                };
                if needs_reindex && pending_files.insert(rel.clone()) {
                    modified += 1;
                    queued += 1;
                }
            }
        }
    }

    if queued > 0 {
        tracing::info!(
            queued,
            added,
            modified,
            "Reconcile: queued divergent files for reindex"
        );
    } else {
        tracing::debug!("Reconcile: no divergence detected");
    }

    queued
}

/// Reconciliation-disable knob. `CQS_WATCH_RECONCILE=0` opts out
/// (operator parity with `CQS_DAEMON_PERIODIC_GC=0`). Any other value or
/// unset → enabled. Read once per call site so an operator can flip it
/// via `systemctl --user set-environment` without daemon restart.
pub(super) fn reconcile_enabled() -> bool {
    std::env::var("CQS_WATCH_RECONCILE").as_deref() != Ok("0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ModelInfo;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    fn parser() -> CqParser {
        CqParser::new().expect("CqParser::new")
    }

    fn open_store(cqs_dir: &Path) -> Store {
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&index_path).expect("open store");
        store.init(&ModelInfo::default()).expect("init store");
        store
    }

    /// Empty index + non-empty disk should queue every disk file as ADDED.
    #[test]
    fn added_files_queue_when_index_empty() {
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("a.rs"), b"fn a() {}").unwrap();
        fs::write(src_dir.join("b.rs"), b"fn b() {}").unwrap();

        let store = open_store(&cqs_dir);
        let mut pending = HashSet::new();
        let queued = run_daemon_reconcile(&store, dir.path(), &parser(), false, &mut pending);
        assert_eq!(queued, 2);
        assert_eq!(pending.len(), 2);
    }

    /// Empty disk + empty index → zero queued.
    #[test]
    fn empty_repo_queues_nothing() {
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();

        let store = open_store(&cqs_dir);
        let mut pending = HashSet::new();
        let queued = run_daemon_reconcile(&store, dir.path(), &parser(), false, &mut pending);
        assert_eq!(queued, 0);
        assert!(pending.is_empty());
    }

    /// Reconcile is a deduplicator — files already in `pending_files` from
    /// inotify must not be double-counted.
    #[test]
    fn reconcile_dedups_against_existing_pending() {
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("a.rs"), b"fn a() {}").unwrap();

        let store = open_store(&cqs_dir);
        let mut pending = HashSet::new();
        // Pre-seed the queue as if inotify already saw the file.
        pending.insert(PathBuf::from("src/a.rs"));

        let queued = run_daemon_reconcile(&store, dir.path(), &parser(), false, &mut pending);
        // The file was already pending — `insert` returned false, so
        // `queued` stays 0 even though the file was divergent.
        assert_eq!(queued, 0);
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn reconcile_enabled_default_true() {
        // Use a unique env-var name guard since other tests may have set
        // CQS_WATCH_RECONCILE.
        let prev = std::env::var("CQS_WATCH_RECONCILE").ok();
        // SAFETY: tests run sequentially within a process; we restore the
        // previous value below.
        unsafe { std::env::remove_var("CQS_WATCH_RECONCILE") };
        assert!(reconcile_enabled());
        // SAFETY: see above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CQS_WATCH_RECONCILE", v),
                None => std::env::remove_var("CQS_WATCH_RECONCILE"),
            }
        }
    }

    #[test]
    fn reconcile_enabled_zero_disables() {
        let prev = std::env::var("CQS_WATCH_RECONCILE").ok();
        // SAFETY: see `reconcile_enabled_default_true`.
        unsafe { std::env::set_var("CQS_WATCH_RECONCILE", "0") };
        assert!(!reconcile_enabled());
        // SAFETY: see above.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CQS_WATCH_RECONCILE", v),
                None => std::env::remove_var("CQS_WATCH_RECONCILE"),
            }
        }
    }
}
