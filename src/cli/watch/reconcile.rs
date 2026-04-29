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
use cqs::store::{FileFingerprint, FingerprintPolicy, Store};

/// Normalize a relative path to forward slashes before insertion into the
/// `pending_files` queue (#1245). The chunks table stores origins via
/// `crate::normalize_path` (slash-only), and the inotify path emits
/// `PathBuf` with whatever separators the OS produces — Windows file
/// events come back with `\\`. Without this normalization a single file
/// edited from a Windows tool then walked by reconcile is double-queued
/// under two separators, and the `process_file_changes` drain reindexes
/// the same chunk twice in a single tick. On Linux/WSL/macOS the path is
/// already clean.
pub(super) fn normalize_pending_path(p: &Path) -> PathBuf {
    match p.to_str() {
        Some(s) if s.contains('\\') => PathBuf::from(s.replace('\\', "/")),
        _ => p.to_path_buf(),
    }
}

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
///
/// `max_pending` caps the total queue size — DS-V1.30.1-D2: respect the
/// same backpressure ceiling the inotify path enforces (events.rs:108)
/// so a bulk `git checkout` of 50k files doesn't drown the next
/// `process_file_changes` cycle. Files skipped at the cap are picked up
/// by the next reconcile pass — the walk is idempotent.
pub(super) fn run_daemon_reconcile(
    store: &Store,
    root: &Path,
    parser: &CqParser,
    no_ignore: bool,
    pending_files: &mut HashSet<PathBuf>,
    max_pending: usize,
) -> usize {
    let _span = tracing::info_span!("daemon_reconcile", max_pending).entered();
    // OB-V1.30.1-7: capture elapsed time for the terminal log lines so
    // operators can correlate reconcile cadence with GC overhead in
    // journalctl. Pattern matches the HNSW build sites already in tree.
    let start = std::time::Instant::now();

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
    let mut skipped_at_cap = 0usize;
    for rel in disk_files {
        // DS-V1.30.1-D2: respect the same cap as the inotify path so a
        // bulk branch switch (50k files) doesn't drown the next
        // `process_file_changes` cycle. Files we skip here are picked
        // up by the next reconcile pass — the walk is idempotent.
        if pending_files.len() >= max_pending {
            skipped_at_cap += 1;
            continue;
        }
        // Stored origins are typically relative; normalize to forward
        // slashes for cross-platform matching parity with the rest of the
        // store layer.
        //
        // RB-1: explicit `to_str()` instead of `to_string_lossy()`. Non-UTF-8
        // path bytes get U+FFFD substitution under `to_string_lossy`, and the
        // indexer's own lossy conversion may emit a different replacement
        // (or skip the file entirely), so the lookup-key never matches the
        // stored origin and the file gets requeued forever — every reconcile
        // pass (default 30 s) wastes a parse + rewarn loop on WSL `/mnt/c/`
        // mounts where filenames can carry stray bytes from Windows tools.
        // Skipping with a warn is strictly better than re-queuing.
        //
        // PF-V1.30.1-4: skip the `replace('\\', "/")` allocation on POSIX
        // paths (the common case on Linux/WSL/macOS). Backslashes only
        // appear on native Windows; on Linux the path is already clean.
        // Use `Cow::Borrowed` to reuse `&str` for the `HashMap` lookup.
        let origin: std::borrow::Cow<'_, str> = match rel.to_str() {
            Some(s) if s.contains('\\') => std::borrow::Cow::Owned(s.replace('\\', "/")),
            Some(s) => std::borrow::Cow::Borrowed(s),
            None => {
                tracing::warn!(
                    path = %rel.display(),
                    "Reconcile: skipping non-UTF-8 path (will not be indexed until renamed)"
                );
                continue;
            }
        };
        match indexed.get(origin.as_ref()) {
            None => {
                // ADDED: no chunks for this file in the index. Queue.
                // PF-V1.30.1-9 / #1245: keep the queue keyed by the same
                // slash-normalized form the chunks table uses, so a
                // Windows-side reconcile and a WSL-side watcher don't
                // double-queue the same file under both separators.
                let normalized = normalize_pending_path(&rel);
                if pending_files.insert(normalized) {
                    added += 1;
                    queued += 1;
                }
            }
            Some(stored_fp) => {
                // MODIFIED: same path indexed, but disk content may have
                // diverged. Use the v23 reconcile fingerprint (mtime+size
                // fast path; BLAKE3 tiebreak on coarse-mtime FSes or
                // content-identical-mtime-bumped flips). #1219.
                let lookup_path: PathBuf = if rel.is_absolute() {
                    rel.clone()
                } else {
                    root.join(&rel)
                };
                let needs_reindex = match FileFingerprint::read_disk(
                    &lookup_path,
                    stored_fp,
                    FingerprintPolicy::MtimeOrHash,
                ) {
                    // EH-V1.30.1-7 / TC-ADV-1.30.1-6: stat failures
                    // (permission flip, transient AV scan, deleted-since-
                    // walk) leave the file to the GC pass — we don't want
                    // to trigger a reindex burst on a file we can't even
                    // read.
                    None => {
                        tracing::debug!(
                            path = %lookup_path.display(),
                            "Reconcile: read_disk returned None, leaving file to GC"
                        );
                        false
                    }
                    Some(disk_fp) => !stored_fp.matches(&disk_fp, FingerprintPolicy::MtimeOrHash),
                };
                if needs_reindex {
                    let normalized = normalize_pending_path(&rel);
                    if pending_files.insert(normalized) {
                        modified += 1;
                        queued += 1;
                    }
                }
            }
        }
    }

    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    if skipped_at_cap > 0 {
        tracing::warn!(
            queued,
            skipped_at_cap,
            cap = max_pending,
            elapsed_ms,
            "Reconcile: hit pending-files cap; skipped files will be picked up on next reconcile pass"
        );
    } else if queued > 0 {
        tracing::info!(
            queued,
            added,
            modified,
            elapsed_ms,
            "Reconcile: queued divergent files for reindex"
        );
    } else {
        tracing::debug!(elapsed_ms, "Reconcile: no divergence detected");
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
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
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
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
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

        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
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

    /// Build a `[1.0; EMBEDDING_DIM]` placeholder embedding for the seed
    /// chunks. We don't care about retrieval quality in the reconcile
    /// tests — `upsert_chunks_batch` just needs *some* embedding per
    /// chunk for the row to land. Inlined here because `cqs::test_helpers`
    /// is `#[cfg(test)]`-gated on the lib side and not visible from the
    /// binary test target.
    fn placeholder_embedding(seed: f32) -> cqs::embedder::Embedding {
        let mut v = vec![seed.max(1e-6); cqs::EMBEDDING_DIM];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        cqs::embedder::Embedding::new(v)
    }

    /// PR 6 of #1182: bulk-delta reconcile pass — the issue's "47-file
    /// `git checkout` diff" acceptance check.
    ///
    /// Models a `git checkout` of a sibling branch under WSL `/mnt/c/`,
    /// where the 9P bridge silently drops every inotify event for the
    /// 47-file delta. The watch loop's event queue stays empty; the
    /// daemon never knows the working tree changed. Only the periodic
    /// (Layer 2) walk or the git-hook-triggered (Layer 1) walk closes
    /// the gap.
    ///
    /// Seed N files with a deliberately-old `source_mtime` (2023-11-14),
    /// then run reconcile against the live disk where each file's mtime
    /// is *now* (2026+). Every file must be queued — otherwise the
    /// state machine would advertise `state == fresh` while the index
    /// silently lagged behind disk by N files.
    ///
    /// Composes the reconcile + state-machine pieces: after reconcile
    /// fills `pending_files`, a `WatchSnapshot` computed from the same
    /// count must report `state == Stale` with `modified_files == N`.
    /// That's the contract `cqs status --watch-fresh` and
    /// `cqs eval --require-fresh` ride on top of.
    #[test]
    fn reconcile_detects_bulk_modify_burst() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::watch_status::{FreshnessState, WatchSnapshot, WatchSnapshotInput};
        use std::marker::PhantomData;

        // 47 files mirrors the issue's acceptance test scenario.
        // Big enough to model a real branch switch; small enough to
        // run in milliseconds on every CI cycle.
        const N: usize = 47;
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Stored mtime is in milliseconds since epoch (matches what
        // `cqs::duration_to_mtime_millis` produces). 2023-11-14 — well
        // before any file we're about to write to disk.
        let stored_mtime_ms: i64 = 1_700_000_000_000;

        let mut pairs: Vec<(Chunk, _)> = Vec::with_capacity(N);
        for i in 0..N {
            let rel = format!("src/f{i}.rs");
            let abs = dir.path().join(&rel);
            let content = format!("fn f{i}() {{ /* {i} */ }}");
            fs::write(&abs, &content).unwrap();
            let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
            pairs.push((
                Chunk {
                    id: format!("src/f{i}.rs:1:{}", &hash[..8]),
                    file: PathBuf::from(&rel),
                    language: Language::Rust,
                    chunk_type: ChunkType::Function,
                    name: format!("f{i}"),
                    signature: format!("fn f{i}()"),
                    content,
                    doc: None,
                    line_start: 1,
                    line_end: 1,
                    content_hash: hash,
                    parent_id: None,
                    window_idx: None,
                    parent_type_name: None,
                    parser_version: 0,
                },
                placeholder_embedding(i as f32),
            ));
        }

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(&pairs, Some(stored_mtime_ms))
            .expect("seed N chunks at stored_mtime");

        // Disk mtimes are "now" (post-write), comfortably newer than
        // `stored_mtime_ms`. Reconcile must classify every file as
        // MODIFIED and queue it.
        let mut pending = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
        assert_eq!(
            queued, N,
            "all {N} bulk-modified files must be queued (got {queued})"
        );
        assert_eq!(pending.len(), N);

        // Compose the state-machine piece: with `pending_files.len() ==
        // N` the snapshot must report Stale with the same count
        // surfaced as `modified_files`. This is what
        // `cqs status --watch-fresh` and `cqs eval --require-fresh`
        // observe through the `Arc<RwLock<WatchSnapshot>>`.
        let snap = WatchSnapshot::compute(WatchSnapshotInput {
            pending_files_count: pending.len(),
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 0,
            dropped_this_cycle: 0,
            last_event: std::time::Instant::now(),
            last_synced_at: None,
            active_slot: None,
            _marker: PhantomData,
        });
        assert_eq!(snap.state, FreshnessState::Stale);
        assert_eq!(snap.modified_files, N as u64);
        assert!(!snap.is_fresh());
    }

    /// PR 6 of #1182: complement to `reconcile_detects_bulk_modify_burst`
    /// — when disk mtimes are *not* newer than what was indexed, reconcile
    /// must keep the state Fresh. This pins the false-positive case the
    /// `git checkout` workflow depends on: just opening files in an editor
    /// without saving must not trigger a 47-file rebuild burst.
    #[test]
    fn reconcile_skips_unchanged_files() {
        use cqs::parser::{Chunk, ChunkType, Language};

        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Write a single file, then store its current disk mtime as the
        // index's `source_mtime` so reconcile sees `disk_mtime ==
        // stored_mtime`. After AC-V1.30.1-1 the predicate is `disk !=
        // stored`, so equality (the unchanged-file case) keeps the file
        // out of the queue.
        let rel = "src/quiet.rs";
        let abs = dir.path().join(rel);
        fs::write(&abs, b"fn quiet() {}").unwrap();
        let disk_mtime_ms = abs
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let stored_mtime_ms = cqs::duration_to_mtime_millis(disk_mtime_ms);

        let content = "fn quiet() {}".to_string();
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: format!("{rel}:1:{}", &hash[..8]),
            file: PathBuf::from(rel),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "quiet".to_string(),
            signature: "fn quiet()".to_string(),
            content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(
                &[(chunk, placeholder_embedding(0.0))],
                Some(stored_mtime_ms),
            )
            .expect("seed chunk at disk mtime");

        let mut pending = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
        assert_eq!(
            queued, 0,
            "file with disk_mtime == stored_mtime must not requeue"
        );
        assert!(pending.is_empty());
    }

    /// DS-V1.30.1-D2: cap shared with the inotify path so a bulk
    /// git-checkout doesn't drown the next process_file_changes
    /// cycle. Tests the strict pre-queue clamp: files we'd otherwise
    /// queue are skipped once `pending_files.len() >= max_pending`.
    #[test]
    fn run_daemon_reconcile_respects_max_pending_cap() {
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let store = open_store(&cqs_dir);

        let mut pending: HashSet<PathBuf> = HashSet::new();
        // Pre-fill 5 entries so `pending.len() >= cap=5` immediately.
        for i in 0..5 {
            pending.insert(PathBuf::from(format!("preexisting_{i}.rs")));
        }
        // Create 20 files on disk.
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        for i in 0..20 {
            fs::write(src_dir.join(format!("file_{i}.rs")), "fn x(){}").unwrap();
        }
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            5, // cap is already met
        );
        assert_eq!(queued, 0, "cap already met → no new entries queued");
        assert_eq!(pending.len(), 5, "pending must not exceed cap");
    }

    /// AC-V1.30.1-1: `git checkout HEAD~5 -- foo.rs` restores the file
    /// with its commit-time mtime, which is *older* than the indexed
    /// `source_mtime`. The strict `disk > stored` predicate would skip
    /// this file silently. Reconcile must use `disk != stored` so any
    /// divergence — forward or backward in time — queues a reindex.
    #[test]
    fn run_daemon_reconcile_queues_older_disk_mtime() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use std::time::{Duration, SystemTime};

        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Write the file with new content (post-checkout state).
        let rel = "src/foo.rs";
        let abs = dir.path().join(rel);
        fs::write(&abs, "fn rewound() {}").unwrap();

        // Rewind the disk mtime to a week ago to simulate `git checkout`
        // restoring a commit-time mtime older than what we'll seed as
        // the stored mtime. `set_modified` is stable since Rust 1.75
        // (cqs MSRV is 1.95) — same pattern used in
        // `src/store/migrations.rs` and `src/cli/batch/mod.rs`.
        let week_ago = SystemTime::now() - Duration::from_secs(7 * 24 * 60 * 60);
        let f = std::fs::OpenOptions::new().write(true).open(&abs).unwrap();
        f.set_modified(week_ago).unwrap();
        drop(f);

        // Seed the index with a HIGHER stored_mtime than the rewound
        // disk mtime — simulates "indexed at HEAD (today), then file
        // rewound by checkout to last week's commit". Use a "now" stored
        // mtime in milliseconds; even if the test runs millis after the
        // rewind, `now > week_ago` by a comfortable margin.
        let stored_mtime_ms = cqs::duration_to_mtime_millis(
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );
        let content = "fn original() {}".to_string(); // any content; only mtime drives the predicate
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: format!("{rel}:1:{}", &hash[..8]),
            file: PathBuf::from(rel),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "original".to_string(),
            signature: "fn original()".to_string(),
            content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(
                &[(chunk, placeholder_embedding(0.0))],
                Some(stored_mtime_ms),
            )
            .expect("seed chunk at stored mtime");

        let mut pending: HashSet<PathBuf> = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );

        assert_eq!(queued, 1, "older-mtime divergent file must be queued");
        assert!(pending.contains(&PathBuf::from(rel)));
    }

    /// #1219: BLAKE3 tiebreak avoids unnecessary re-embed when mtime
    /// bumped but content is identical — `git checkout`, formatter
    /// passes, and `touch` all push mtime forward without changing
    /// bytes. Pre-v23 reconcile saw `disk_mtime != stored_mtime` and
    /// re-queued every chunk in the file (3-5k chunks per branch flip).
    /// The v23 fingerprint reads `source_size` and `source_content_hash`
    /// and the MtimeOrHash policy falls through to BLAKE3 when
    /// mtime/size disagrees; matching hashes keep the file out of the
    /// queue.
    ///
    /// This is the load-bearing optimization case: the test pins that a
    /// formatter-pass-style mtime bump on otherwise-identical content
    /// does NOT requeue the file once v23 fingerprints are stamped.
    #[test]
    fn run_daemon_reconcile_blake3_skips_mtime_only_bump_with_identical_content() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::store::FileFingerprint;
        use std::time::{Duration, SystemTime};

        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let rel = "src/touched.rs";
        let abs = dir.path().join(rel);
        let bytes = b"fn touched() {}";
        fs::write(&abs, bytes).unwrap();

        // Seed the index with a stored mtime *behind* the disk: simulates
        // an indexed-then-`git-checkout` flip that bumps mtime without
        // changing content. Then bump disk mtime forward by setting it
        // explicitly (so we know exactly what reconcile will read).
        let stored_mtime_ms = cqs::duration_to_mtime_millis(
            (SystemTime::now() - Duration::from_secs(60))
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );
        let chunk_content = "fn touched() {}".to_string();
        let hash_str = blake3::hash(chunk_content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: format!("{rel}:1:{}", &hash_str[..8]),
            file: PathBuf::from(rel),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "touched".to_string(),
            signature: "fn touched()".to_string(),
            content: chunk_content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash_str,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(
                &[(chunk, placeholder_embedding(0.0))],
                Some(stored_mtime_ms),
            )
            .expect("seed chunk at older stored mtime");
        // Stamp v23 fingerprint with the same content the file currently
        // holds — the mtime is older, but size + hash match disk.
        let content_hash_bytes = *blake3::hash(bytes).as_bytes();
        let stored_fp = FileFingerprint {
            mtime: Some(stored_mtime_ms),
            size: Some(bytes.len() as u64),
            content_hash: Some(content_hash_bytes),
        };
        store
            .set_file_fingerprint(&PathBuf::from(rel), &stored_fp)
            .expect("stamp v23 fingerprint");

        let mut pending: HashSet<PathBuf> = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
        assert_eq!(
            queued, 0,
            "mtime-only bump with identical content must not requeue under v23 fingerprint"
        );
        assert!(pending.is_empty());
    }

    /// #1219: BLAKE3 tiebreak catches genuine divergence when mtime
    /// disagrees AND content differs. Mirror of the mtime-only-bump
    /// optimization above: same mtime+size match short-circuit avoidance,
    /// but disk content actually differs from stored hash → must queue.
    #[test]
    fn run_daemon_reconcile_blake3_queues_when_hash_diverges() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::store::FileFingerprint;
        use std::time::{Duration, SystemTime};

        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        // Disk holds the new content. Stored hash is for OLD content
        // (different bytes), and stored mtime is older than disk — the
        // mtime mismatch already triggers the divergence check; the hash
        // tiebreak confirms we should still queue.
        let rel = "src/changed.rs";
        let abs = dir.path().join(rel);
        let new_bytes = b"fn after_change() {}";
        fs::write(&abs, new_bytes).unwrap();
        let stored_mtime_ms = cqs::duration_to_mtime_millis(
            (SystemTime::now() - Duration::from_secs(60))
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );
        let old_bytes: &[u8] = b"fn before_change() {}"; // different bytes, different size
        let chunk_content = "fn after_change() {}".to_string();
        let hash_str = blake3::hash(chunk_content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: format!("{rel}:1:{}", &hash_str[..8]),
            file: PathBuf::from(rel),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "after_change".to_string(),
            signature: "fn after_change()".to_string(),
            content: chunk_content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash_str,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(
                &[(chunk, placeholder_embedding(0.0))],
                Some(stored_mtime_ms),
            )
            .expect("seed chunk at older stored mtime");
        let stored_fp = FileFingerprint {
            mtime: Some(stored_mtime_ms),
            size: Some(old_bytes.len() as u64),
            content_hash: Some(*blake3::hash(old_bytes).as_bytes()),
        };
        store
            .set_file_fingerprint(&PathBuf::from(rel), &stored_fp)
            .expect("stamp v23 fingerprint");

        let mut pending: HashSet<PathBuf> = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
        assert_eq!(
            queued, 1,
            "real divergence must queue (got queued={queued})"
        );
        assert!(pending.contains(&PathBuf::from(rel)));
    }

    /// #1219: identical mtime+size+hash → reconcile must keep the file
    /// out of the queue. The fast-path short-circuit (mtime+size both
    /// match → unchanged, no hash read needed) is the steady-state
    /// optimization that keeps reconcile near-free on quiet repos.
    #[test]
    fn run_daemon_reconcile_blake3_match_skips_unchanged() {
        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::store::FileFingerprint;

        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();

        let rel = "src/quiet.rs";
        let abs = dir.path().join(rel);
        let bytes = b"fn quiet() {}";
        fs::write(&abs, bytes).unwrap();
        let disk_mtime_ms = cqs::duration_to_mtime_millis(
            abs.metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );

        let chunk_content = "fn quiet() {}".to_string();
        let hash_str = blake3::hash(chunk_content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: format!("{rel}:1:{}", &hash_str[..8]),
            file: PathBuf::from(rel),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "quiet".to_string(),
            signature: "fn quiet()".to_string(),
            content: chunk_content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash_str,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let store = open_store(&cqs_dir);
        store
            .upsert_chunks_batch(&[(chunk, placeholder_embedding(0.0))], Some(disk_mtime_ms))
            .expect("seed chunk");
        let stored_fp = FileFingerprint {
            mtime: Some(disk_mtime_ms),
            size: Some(bytes.len() as u64),
            content_hash: Some(*blake3::hash(bytes).as_bytes()),
        };
        store
            .set_file_fingerprint(&PathBuf::from(rel), &stored_fp)
            .expect("stamp v23 fingerprint");

        let mut pending: HashSet<PathBuf> = HashSet::new();
        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
        assert_eq!(
            queued, 0,
            "matching v23 fingerprint must keep the file out of the queue"
        );
        assert!(pending.is_empty());
    }

    /// #1245: separator dedup. Pre-seeding the queue with a backslash
    /// path (simulating a Windows event source) must NOT cause reconcile
    /// to insert the slash-form sibling as a separate entry on the same
    /// pass. The chunks table stores origins via `normalize_path`
    /// (slash-only), so reconcile must key its queue insertions on the
    /// same form.
    #[test]
    fn run_daemon_reconcile_dedups_against_backslash_pending_entry() {
        let dir = TempDir::new().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        fs::create_dir_all(&cqs_dir).unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("dup.rs"), b"fn dup() {}").unwrap();

        let store = open_store(&cqs_dir);
        let mut pending = HashSet::new();
        // Pre-seed with the slash form already normalized — what a Windows
        // event source would push after #1245's events.rs change.
        pending.insert(PathBuf::from("src/dup.rs"));

        let queued = run_daemon_reconcile(
            &store,
            dir.path(),
            &parser(),
            false,
            &mut pending,
            usize::MAX,
        );
        assert_eq!(
            queued, 0,
            "reconcile must dedup against the existing slash-form entry"
        );
        assert_eq!(pending.len(), 1, "queue must contain exactly one entry");
    }

    /// #1245: `normalize_pending_path` directly — backslashes get rewritten,
    /// already-clean paths pass through. Pin the path-mangling rules so a
    /// future tweak doesn't silently change behavior.
    #[test]
    fn normalize_pending_path_rewrites_backslashes() {
        assert_eq!(
            normalize_pending_path(Path::new(r"src\foo.rs")),
            PathBuf::from("src/foo.rs"),
        );
        assert_eq!(
            normalize_pending_path(Path::new("src/foo.rs")),
            PathBuf::from("src/foo.rs"),
        );
        assert_eq!(
            normalize_pending_path(Path::new(r"a\b\c\d.rs")),
            PathBuf::from("a/b/c/d.rs"),
        );
    }
}
