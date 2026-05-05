// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Staleness checks and pruning for missing/stale files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::store::helpers::sql::max_rows_per_statement;
use crate::store::helpers::{StaleFile, StaleReport, StoreError};
use crate::store::{ReadWrite, Store};

/// Per-file fingerprint stored alongside each chunk for the reconcile path
/// (issue #1219 / EX-V1.30.1-6).
///
/// All three fields are nullable for back-compat with pre-schema-v23 rows
/// whose `source_size` and `source_content_hash` columns are `NULL` until
/// first re-embed populates them. `mtime` may also be `None` for legacy
/// rows where `source_mtime` was stored as `NULL` (FS without modification
/// time).
///
/// Comparison policy lives in [`FingerprintPolicy`]; see [`Self::matches`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFingerprint {
    /// Modification time in milliseconds since the Unix epoch (matches
    /// `cqs::duration_to_mtime_millis`).
    pub mtime: Option<i64>,
    /// File size in bytes from `metadata().len()`.
    pub size: Option<u64>,
    /// BLAKE3 hash of the file's bytes (32 bytes).
    pub content_hash: Option<[u8; 32]>,
}

/// Policy that decides which fields participate in [`FileFingerprint::matches`].
///
/// Default for the watch-loop reconcile path is [`Self::MtimeOrHash`]: stay
/// fast on the common case (mtime+size both match → file unchanged) but
/// fall through to `content_hash` when mtime/size disagrees. This catches
/// the two well-known reconcile bugs (issue #1219):
///
/// - **Content-identical-mtime-bumped.** `git checkout`, formatter passes,
///   and `touch` all bump mtime without changing content. A pure mtime
///   compare would re-embed ~3-5k chunks per branch flip needlessly; the
///   hash tiebreak skips them.
/// - **Coarse-mtime collisions.** WSL DrvFS / NTFS / HFS+ / SMB mount
///   points have ≥1 s mtime resolution. Two saves within one second
///   collide on identical mtimes; size+hash detect the divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintPolicy {
    /// Compare mtime only — pre-v23 behavior. Cheap; misses both bug
    /// classes above. Useful for tests that explicitly want the legacy
    /// shape and for environments where reading every divergent file is
    /// prohibitive.
    MtimeOnly,
    /// Default. mtime+size for the fast path; hash on tiebreak. Disk-side
    /// hash is only computed when mtime/size disagree, so the hot path
    /// (no changes since last walk) stays stat-only.
    MtimeOrHash,
    /// Compare size+hash, ignore mtime entirely. Safest under coarse-mtime
    /// FSes where every walked file's mtime is unreliable. Costs one
    /// `read_to_end` + BLAKE3 per file per walk; intended for opt-in
    /// `cqs index --strict` mode rather than the default 30 s reconcile
    /// cadence.
    HashOnly,
}

impl FileFingerprint {
    /// Decide whether `self` (typically the stored fingerprint) matches
    /// `disk` (the just-read disk fingerprint) under `policy`.
    ///
    /// Returns `true` iff the file is treated as unchanged (skip reindex).
    /// The semantics are not symmetric in spirit — `disk` should be the
    /// freshly-read side — but the implementation only does field
    /// comparisons so swapping arguments produces the same boolean.
    pub fn matches(&self, disk: &Self, policy: FingerprintPolicy) -> bool {
        match policy {
            FingerprintPolicy::MtimeOnly => self.mtime == disk.mtime,
            FingerprintPolicy::HashOnly => {
                // Both sides need a hash to make a strict-content decision.
                // Pre-v23 rows have `content_hash = None`; treat as "no
                // information, assume divergent" — the next reindex will
                // populate the hash and subsequent walks will be quick.
                match (&self.content_hash, &disk.content_hash) {
                    (Some(a), Some(b)) => a == b && self.size == disk.size,
                    _ => false,
                }
            }
            FingerprintPolicy::MtimeOrHash => {
                // Pre-v23 fallback: if the stored fingerprint has only
                // mtime — `source_size` and `source_content_hash` were not
                // recorded yet — fall back to mtime equality. Without this,
                // every legacy chunk row would be classified divergent on
                // every reconcile pass until first re-embed populated the
                // new columns, drowning the watch queue. Once the row has
                // a fingerprint (even one field populated), we use the
                // strict comparison below.
                if self.size.is_none() && self.content_hash.is_none() {
                    return self.mtime == disk.mtime;
                }
                // Fast path: mtime+size both match → unchanged. Most files
                // on most walks; this is the steady-state common case.
                if self.mtime == disk.mtime && self.size == disk.size {
                    return true;
                }
                // mtime or size disagrees. Fall through to content_hash if
                // both sides have one; otherwise treat as divergent.
                match (&self.content_hash, &disk.content_hash) {
                    (Some(stored), Some(disk_h)) => stored == disk_h,
                    _ => false,
                }
            }
        }
    }

    /// Read a disk-side fingerprint at the granularity demanded by `policy`,
    /// using `stored` to decide whether the (relatively expensive) hash
    /// step is needed. mtime+size always populate; `content_hash` only
    /// when:
    ///
    /// - `policy == HashOnly` (always hash),
    /// - `policy == MtimeOrHash` AND mtime+size disagree with `stored`
    ///   (tiebreak needed),
    /// - never under `MtimeOnly`.
    ///
    /// Returns `None` only if the file can't be `metadata()`'d (deleted,
    /// permission-denied, transient AV scan). Hash-read failures populate
    /// the mtime+size fields but leave `content_hash` as `None`, which
    /// `matches` treats as "no information" → queue for reindex.
    pub fn read_disk(
        path: &Path,
        stored: &FileFingerprint,
        policy: FingerprintPolicy,
    ) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(crate::duration_to_mtime_millis);
        let size = Some(meta.len());

        let needs_hash = match policy {
            FingerprintPolicy::MtimeOnly => false,
            FingerprintPolicy::HashOnly => true,
            FingerprintPolicy::MtimeOrHash => {
                // Legacy row (pre-v23): stored has only mtime, so `matches`
                // uses mtime equality — no hash needed. Skipping the hash
                // here matters: every reconcile walk for a legacy index
                // would otherwise BLAKE3 the entire tree on the first
                // size-mismatch (stored=None vs disk=Some(N)) until first
                // re-embed.
                if stored.size.is_none() && stored.content_hash.is_none() {
                    false
                } else {
                    stored.mtime != mtime || stored.size != size
                }
            }
        };

        // RB-V1.36-5 / P2-7: streaming blake3 with bounded RSS. Pre-fix
        // slurped the whole file into memory before hashing — a 5 GB SQL
        // dump that grew between index and watch tried to fingerprint
        // whole. Hasher::update_reader keeps the working set at ~64 KiB.
        let content_hash = if needs_hash {
            std::fs::File::open(path).ok().and_then(|f| {
                let mut hasher = blake3::Hasher::new();
                if hasher.update_reader(std::io::BufReader::new(f)).is_ok() {
                    Some(*hasher.finalize().as_bytes())
                } else {
                    None
                }
            })
        } else {
            None
        };

        Some(FileFingerprint {
            mtime,
            size,
            content_hash,
        })
    }
}

/// Decide whether a chunk origin refers to a file the indexer still owns.
///
/// Used by all four staleness helpers in this module. The check is: does
/// the origin's canonicalized absolute path appear in `existing_files`
/// (which `enumerate_files` produces with the worktree-skip + gitignore
/// filters applied).
///
/// **No `exists()` fallback.** A bare `Path::exists()` probe would keep
/// any file that is on disk regardless of whether the indexer should own
/// it — that's how `.claude/worktrees/agent-X/...` chunks survived GC even
/// though `enumerate_files` correctly skipped them. The fallback was
/// originally added to compensate for a path-form mismatch (chunks store
/// origin relative; `existing_files` may hold canonicalized absolutes).
/// Canonicalizing both sides removes the form mismatch, which is the only
/// reason the fallback existed.
///
/// PF-V1.29-8: origins are stored as slash-normalized absolute paths (via
/// `normalize_path`), while `existing_files` holds OS-native `PathBuf`s
/// (backslashes on Windows). That form mismatch forced every origin
/// through the slow `dunce::canonicalize` path on Windows. Callers now
/// pre-compute a parallel `HashSet<String>` of slash-normalized entries
/// once per prune and pass it in — the fast string hash probe always
/// hits for real matches, and `dunce::canonicalize` only fires for true
/// misses (e.g. stale relative origins from old schema versions).
fn origin_exists(
    origin: &str,
    existing_files: &HashSet<PathBuf>,
    existing_normalized: Option<&HashSet<String>>,
    root: &Path,
) -> bool {
    // Fast string path (PF-V1.29-8): origins and the pre-normalized set
    // are both in slash form, so on Windows this replaces the former
    // "miss → dunce::canonicalize(absolute)" path with a hash probe.
    if let Some(set) = existing_normalized {
        if set.contains(origin) {
            return true;
        }
    }

    let origin_path = PathBuf::from(origin);
    // Legacy cheap path: exact PathBuf match as stored. Retained as a
    // no-cost fallback for callers that pass an already-matching HashSet
    // without pre-normalizing.
    if existing_files.contains(&origin_path) {
        return true;
    }
    let absolute = if origin_path.is_absolute() {
        origin_path
    } else {
        root.join(&origin_path)
    };
    // Canonicalized match: `enumerate_files` canonicalizes via dunce, so
    // we have to canonicalize here to compare apples to apples. If the
    // file no longer exists (canonicalize fails), it definitely shouldn't
    // count as owned by the indexer — drop it.
    match dunce::canonicalize(&absolute) {
        Ok(canonical) => existing_files.contains(&canonical),
        Err(_) => false,
    }
}

/// Pre-compute the slash-normalized string form of each entry in
/// `existing_files`. Built once at the top of each prune/stale entry
/// function so `origin_exists` can do a direct string lookup (no
/// PathBuf allocation, no dunce syscall) on the common-case hit.
fn build_normalized_set(existing_files: &HashSet<PathBuf>) -> HashSet<String> {
    existing_files
        .iter()
        .map(|p| crate::normalize_path(p))
        .collect()
}

/// Result of running all GC prune operations atomically.
#[derive(Debug, Clone)]
pub struct PruneAllResult {
    /// Chunks deleted for files no longer on disk.
    pub pruned_chunks: u32,
    /// Orphan `function_calls` rows removed.
    pub pruned_calls: u64,
    /// Orphan `type_edges` rows removed.
    pub pruned_type_edges: u64,
    /// Orphan `llm_summaries` rows removed.
    pub pruned_summaries: usize,
}

impl Store<ReadWrite> {
    /// Delete chunks for files that no longer exist
    /// Batches deletes in groups of 100 to balance memory usage and query efficiency.
    /// Uses Rust HashSet for existence check rather than SQL WHERE NOT IN because:
    /// - Existing files often number 10k+, exceeding SQLite's parameter limit (~999)
    /// - Sending full file list to SQLite would require chunked queries anyway
    /// - HashSet lookup is O(1), and we already have the set from enumerate_files()
    pub fn prune_missing(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_missing", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            // DS2-1: acquire the write transaction BEFORE reading origins.
            // Reading outside the tx creates a TOCTOU window where a
            // concurrent `cqs watch` upsert adds a chunk for a file between
            // the SELECT and the DELETE. Because that file also isn't in
            // the caller's `existing_files` snapshot (gathered before this
            // call), our stale origin list would flag it as missing and
            // wipe the just-added row on DELETE. Serialising the read
            // under the write lock closes it — same fix class as P2 #32
            // that hardened `prune_all`.
            let (_guard, mut tx) = self.begin_write().await?;

            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&mut *tx)
            .await?;

            // AC / CQ-V1.25-4 / CQ-V1.25-6 / PB-V1.25-7: reconcile stored origins
            // against current filesystem state via `origin_exists`. The previous
            // `ends_with` heuristic retained 81% of chunks as orphans whenever a
            // worktree or subdirectory tail-matched a root file name.
            //
            // PF-V1.29-8: pre-compute the slash-normalized string set once so
            // `origin_exists` hits the cheap string path for Windows origins
            // (which are stored with `/` while `existing_files` holds `\`).
            let existing_normalized = build_normalized_set(existing_files);
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| {
                    !origin_exists(origin, existing_files, Some(&existing_normalized), root)
                })
                .map(|(origin,)| origin)
                .collect();

            if missing.is_empty() {
                return Ok(0);
            }

            // Single-bind IN-list batched at the modern SQLite variable
            // limit. Single transaction wraps ALL batches — partial prune
            // on crash would leave the index inconsistent with disk.
            const BATCH_SIZE: usize = max_rows_per_statement(1);
            let mut deleted = 0u32;

            for batch in missing.chunks(BATCH_SIZE) {
                let placeholder_str = crate::store::helpers::make_placeholders(batch.len());

                // Delete from FTS first
                let fts_query = format!(
                    "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin IN ({}))",
                    placeholder_str
                );
                let mut fts_stmt = sqlx::query(&fts_query);
                for origin in batch {
                    fts_stmt = fts_stmt.bind(origin);
                }
                fts_stmt.execute(&mut *tx).await?;

                // Delete from chunks
                let chunks_query =
                    format!("DELETE FROM chunks WHERE origin IN ({})", placeholder_str);
                let mut chunks_stmt = sqlx::query(&chunks_query);
                for origin in batch {
                    chunks_stmt = chunks_stmt.bind(origin);
                }
                let result = chunks_stmt.execute(&mut *tx).await?;
                deleted += result.rows_affected() as u32;

                // E.2 (P1 #17): `function_calls` has no FK to `chunks` (it is
                // keyed by `file`, not chunk ID), so deleting chunks does not
                // cascade. Without this DELETE, prune leaves orphan call-graph
                // rows that surface as ghost callers in
                // `cqs callers`/`callees`/`dead`. Use the same chunked-IN
                // pattern over `file` values so we stay under the SQLite
                // parameter limit.
                let calls_query = format!(
                    "DELETE FROM function_calls WHERE file IN ({})",
                    placeholder_str
                );
                let mut calls_stmt = sqlx::query(&calls_query);
                for origin in batch {
                    calls_stmt = calls_stmt.bind(origin);
                }
                calls_stmt.execute(&mut *tx).await?;
            }

            // DS-1/DS-6: Delete orphan sparse_vectors inside the same transaction.
            if deleted > 0 {
                let sparse_result = sqlx::query(
                    "DELETE FROM sparse_vectors WHERE chunk_id NOT IN \
                     (SELECT id FROM chunks)",
                )
                .execute(&mut *tx)
                .await?;
                let pruned_sparse = sparse_result.rows_affected();
                if pruned_sparse > 0 {
                    tracing::debug!(pruned_sparse, "Pruned orphan sparse vectors in prune_missing tx");
                }
            }

            tx.commit().await?;

            if deleted > 0 {
                tracing::info!(deleted, files = missing.len(), "Pruned chunks for missing files");
            }

            Ok(deleted)
        })
    }

    /// Run all prune operations in a single SQLite transaction.
    /// Ensures concurrent readers never see an inconsistent state where chunks
    /// are deleted but orphan call graph / type edge / summary entries remain.
    /// Without this, the window between `prune_missing` and `prune_stale_calls`
    /// exposes stale `function_calls` rows referencing deleted chunks.
    //
    // P2 #32 (recovery wave): the previous Phase 1 ran the SELECT DISTINCT
    // outside the write transaction. If `cqs watch` (which only takes
    // `try_acquire_index_lock`) interleaved an `upsert_chunks_and_calls` call
    // for a freshly-added file between Phase 1 and Phase 2, that file's origin
    // was missing from `existing_files` (caller-passed snapshot) yet present in
    // `chunks` — the Phase 2 DELETE would wipe the just-added rows. Closed by
    // snapshotting inside the write transaction so we observe a post-watch-
    // reindex-consistent view.
    pub fn prune_all(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<PruneAllResult, StoreError> {
        let _span = tracing::info_span!("prune_all", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            // Phase 2 begins immediately: take the write lock first so the
            // distinct-origin scan happens against the same snapshot the
            // DELETEs will operate on (P2 #32 TOCTOU close).
            let (_guard, mut tx) = self.begin_write().await?;

            // Phase 1 (now inside the tx): identify missing origins via the
            // tx's read snapshot. Any concurrent watch reindex that committed
            // *before* our begin_write is reflected here; any reindex that
            // committed *after* will be queued behind our write lock. Either
            // way the missing-set lines up with what's actually deletable.
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&mut *tx)
            .await?;

            // Same filesystem-existence reconciliation as `prune_missing`.
            // PF-V1.29-8: pre-compute the slash-normalized string set once
            // so the per-origin check hits the cheap string path.
            let existing_normalized = build_normalized_set(existing_files);
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| {
                    !origin_exists(origin, existing_files, Some(&existing_normalized), root)
                })
                .map(|(origin,)| origin)
                .collect();

            // 2a. Delete chunks for missing files (single-bind IN-list,
            // batched at the modern SQLite variable limit).
            const BATCH_SIZE: usize = max_rows_per_statement(1);
            let mut pruned_chunks = 0u32;

            for batch in missing.chunks(BATCH_SIZE) {
                let placeholder_str = crate::store::helpers::make_placeholders(batch.len());

                // Delete from FTS first (referential)
                let fts_query = format!(
                    "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin IN ({}))",
                    placeholder_str
                );
                let mut fts_stmt = sqlx::query(&fts_query);
                for origin in batch {
                    fts_stmt = fts_stmt.bind(origin);
                }
                fts_stmt.execute(&mut *tx).await?;

                // Delete from chunks
                let chunks_query =
                    format!("DELETE FROM chunks WHERE origin IN ({})", placeholder_str);
                let mut chunks_stmt = sqlx::query(&chunks_query);
                for origin in batch {
                    chunks_stmt = chunks_stmt.bind(origin);
                }
                let result = chunks_stmt.execute(&mut *tx).await?;
                pruned_chunks += result.rows_affected() as u32;
            }

            // 2b. Delete orphan function_calls (file no longer in chunks)
            let calls_result = sqlx::query(
                "DELETE FROM function_calls WHERE file NOT IN (SELECT DISTINCT origin FROM chunks)",
            )
            .execute(&mut *tx)
            .await?;
            let pruned_calls = calls_result.rows_affected();

            // 2c. Delete orphan type_edges (source_chunk_id no longer in chunks)
            let types_result = sqlx::query(
                "DELETE FROM type_edges WHERE source_chunk_id NOT IN (SELECT id FROM chunks)",
            )
            .execute(&mut *tx)
            .await?;
            let pruned_type_edges = types_result.rows_affected();

            // 2d. Delete orphan LLM summaries (content_hash no longer in any chunk)
            let summaries_result = sqlx::query(
                "DELETE FROM llm_summaries WHERE content_hash NOT IN \
                 (SELECT DISTINCT content_hash FROM chunks)",
            )
            .execute(&mut *tx)
            .await?;
            let pruned_summaries = summaries_result.rows_affected() as usize;

            // 2e. DS-1/DS-6: Delete orphan sparse_vectors inside the same transaction.
            // Previously these were pruned in a separate call after commit, leaving a
            // window where stale sparse vectors could inflate the SPLADE index.
            let sparse_result = sqlx::query(
                "DELETE FROM sparse_vectors WHERE chunk_id NOT IN \
                 (SELECT id FROM chunks)",
            )
            .execute(&mut *tx)
            .await?;
            let pruned_sparse = sparse_result.rows_affected() as usize;
            if pruned_sparse > 0 {
                tracing::debug!(pruned_sparse, "Pruned orphan sparse vectors in prune_all tx");
            }

            tx.commit().await?;

            if pruned_chunks > 0 {
                tracing::info!(pruned_chunks, files = missing.len(), "Pruned chunks for missing files");
            }
            if pruned_calls > 0 {
                tracing::info!(pruned_calls, "Pruned stale call graph entries");
            }
            if pruned_type_edges > 0 {
                tracing::info!(pruned_type_edges, "Pruned stale type edges");
            }
            if pruned_summaries > 0 {
                tracing::info!(pruned_summaries, "Pruned orphan LLM summaries");
            }

            Ok(PruneAllResult {
                pruned_chunks,
                pruned_calls,
                pruned_type_edges,
                pruned_summaries,
            })
        })
    }

    /// Delete chunks for files whose path is now matched by a `.gitignore`
    /// matcher. Used by the daemon's startup GC pass to clean up rows that
    /// were indexed before v1.26.0 added gitignore-respect to `cqs watch`,
    /// or that pre-date a `.gitignore` change.
    ///
    /// Walks each distinct origin in `chunks` (with `source_type='file'`),
    /// resolves it against `root` to obtain an absolute path, and asks the
    /// matcher whether the path or any parent is ignored. Matching paths are
    /// deleted in batches of 100 in a single transaction (same shape as
    /// `prune_missing`). Notes and other non-file source types are
    /// untouched.
    ///
    /// `max_paths` caps how many distinct origins this call examines per
    /// invocation. Pass `None` for "no cap" (startup-time full sweep);
    /// the periodic idle-time GC passes a small bound (e.g. 1000) so a
    /// single tick never holds the write transaction longer than necessary.
    /// Returns the number of chunk rows actually deleted.
    pub fn prune_gitignored(
        &self,
        matcher: &ignore::gitignore::Gitignore,
        root: &Path,
        max_paths: Option<usize>,
    ) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_gitignored", max_paths = ?max_paths).entered();
        self.rt.block_on(async {
            // DS2-2: acquire the write transaction BEFORE reading origins.
            // Same TOCTOU fix as DS2-1 / P2 #32: a concurrent `cqs watch`
            // upsert landing between the SELECT and the DELETE creates a
            // chunk for a path the matcher will flag as ignored; the stale
            // origin list then points DELETE at a just-inserted row. The
            // matcher walk below is pure CPU over the already-fetched
            // `rows` Vec (microseconds on ~10k origins) and is safe to
            // hold the write lock across — single writer, no re-entry.
            let (_guard, mut tx) = self.begin_write().await?;

            // Phase 1 (now inside the tx): collect distinct origins via the
            // tx's read snapshot so reads and deletes serialise as one unit.
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&mut *tx)
            .await?;

            let cap = max_paths.unwrap_or(usize::MAX);
            let mut ignored: Vec<String> = Vec::new();
            for (origin,) in rows.into_iter() {
                if ignored.len() >= cap {
                    break;
                }
                let origin_path = PathBuf::from(&origin);
                let absolute = if origin_path.is_absolute() {
                    origin_path
                } else {
                    root.join(&origin_path)
                };
                // `matched_path_or_any_parents` walks up the path's parents
                // so that `.claude/worktrees/agent-x/src/lib.rs` is treated
                // as ignored when `.claude/` is in `.gitignore`. The
                // leaf-only `matched()` would miss this case — same logic
                // as `collect_events` in `cli/watch.rs`.
                if matcher
                    .matched_path_or_any_parents(&absolute, false)
                    .is_ignore()
                {
                    ignored.push(origin);
                }
            }

            if ignored.is_empty() {
                return Ok(0);
            }

            // Phase 2: batched delete in the SAME transaction started above.
            // Same shape as `prune_missing` so a partial prune on crash
            // leaves the index consistent with the remaining rows in `chunks`.
            const BATCH_SIZE: usize = max_rows_per_statement(1);
            let mut deleted = 0u32;

            for batch in ignored.chunks(BATCH_SIZE) {
                let placeholder_str = crate::store::helpers::make_placeholders(batch.len());

                // Delete from FTS first
                let fts_query = format!(
                    "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin IN ({}))",
                    placeholder_str
                );
                let mut fts_stmt = sqlx::query(&fts_query);
                for origin in batch {
                    fts_stmt = fts_stmt.bind(origin);
                }
                fts_stmt.execute(&mut *tx).await?;

                // Delete from chunks
                let chunks_query =
                    format!("DELETE FROM chunks WHERE origin IN ({})", placeholder_str);
                let mut chunks_stmt = sqlx::query(&chunks_query);
                for origin in batch {
                    chunks_stmt = chunks_stmt.bind(origin);
                }
                let result = chunks_stmt.execute(&mut *tx).await?;
                deleted += result.rows_affected() as u32;
            }

            // Sweep orphan sparse_vectors inside the same transaction so a
            // crash between DELETE chunks and DELETE sparse_vectors doesn't
            // leave the SPLADE index over-counted (same fix as DS-1/DS-6 in
            // `prune_missing`).
            if deleted > 0 {
                let sparse_result = sqlx::query(
                    "DELETE FROM sparse_vectors WHERE chunk_id NOT IN \
                     (SELECT id FROM chunks)",
                )
                .execute(&mut *tx)
                .await?;
                let pruned_sparse = sparse_result.rows_affected();
                if pruned_sparse > 0 {
                    tracing::debug!(
                        pruned_sparse,
                        "Pruned orphan sparse vectors in prune_gitignored tx"
                    );
                }
            }

            tx.commit().await?;

            if deleted > 0 {
                tracing::info!(
                    deleted,
                    paths = ignored.len(),
                    "Pruned chunks for gitignored paths"
                );
            }

            Ok(deleted)
        })
    }
}

impl<Mode> Store<Mode> {
    /// Count files that are stale (mtime changed) or missing from disk.
    /// Compares stored source_mtime against current filesystem state.
    /// Only checks files with source_type='file' (not notes or other sources).
    /// Returns `(stale_count, missing_count)`.
    pub fn count_stale_files(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<(u64, u64), StoreError> {
        let _span = tracing::debug_span!("count_stale_files").entered();
        let report = self.list_stale_files(existing_files, root)?;
        Ok((report.stale.len() as u64, report.missing.len() as u64))
    }

    /// List files that are stale (mtime changed) or missing from disk.
    /// Like `count_stale_files()` but returns full details for display.
    /// Requires `existing_files` from `enumerate_files()` (~100ms for 10k files).
    pub fn list_stale_files(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<StaleReport, StoreError> {
        let _span = tracing::debug_span!("list_stale_files").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
                "SELECT DISTINCT origin, source_mtime FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            let total_indexed = rows.len() as u64;
            let mut stale = Vec::new();
            let mut missing = Vec::new();

            for (origin, stored_mtime) in rows {
                let path = PathBuf::from(&origin);

                // Filesystem existence check — same logic as prune_*. Replaces
                // the previous macOS case-fold + set-contains special case.
                if !origin_exists(&origin, existing_files, None, root) {
                    missing.push(path);
                    continue;
                }

                let stored = match stored_mtime {
                    Some(m) => m,
                    None => {
                        // NULL mtime → treat as stale (can't verify freshness)
                        stale.push(StaleFile {
                            file: path,
                            stored_mtime: 0,
                            current_mtime: 0,
                        });
                        continue;
                    }
                };

                // Resolve the path against `root` for metadata lookup so
                // relative origins work regardless of current directory.
                let lookup_path: PathBuf = if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(&path)
                };
                // P3 #135: previously this code threaded `metadata()` errors
                // through `.ok()`, so a permission-denied or busy-file failure
                // yielded `None` and the file was silently treated as fresh.
                // Surface the failure and treat the file as stale with a
                // sentinel `current_mtime = -1` so an operator can spot the
                // unreadable origin in `cqs stats --json`.
                let current_mtime = match lookup_path.metadata().and_then(|m| m.modified()) {
                    Ok(t) => t
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(crate::duration_to_mtime_millis),
                    Err(e) => {
                        tracing::warn!(
                            path = %lookup_path.display(),
                            error = %e,
                            "Failed to read mtime for indexed file — treating as stale"
                        );
                        stale.push(StaleFile {
                            file: path,
                            stored_mtime: stored,
                            current_mtime: -1,
                        });
                        continue;
                    }
                };

                if let Some(current) = current_mtime {
                    if current > stored {
                        stale.push(StaleFile {
                            file: path,
                            stored_mtime: stored,
                            current_mtime: current,
                        });
                    }
                }
            }

            Ok(StaleReport {
                stale,
                missing,
                total_indexed,
            })
        })
    }

    /// #1182 (Layer 2): list every indexed source-file origin paired with
    /// its stored `source_mtime`. Returned as a map keyed by origin string
    /// so a watch-loop reconciler can:
    ///   1. Walk the disk → set of current files
    ///   2. Look up each disk file in this map
    ///   3. Queue for reindex when missing (added) OR `disk_mtime > stored`
    ///      (modified). Files in this map but not on disk are handled by
    ///      `prune_missing` in the existing GC pass.
    ///
    /// `source_mtime` may be `None` (legacy rows pre-v18 schema). Treat
    /// those as needing reindex — same posture as `list_stale_files`,
    /// which surfaces them as stale-with-mtime=0.
    ///
    /// One SELECT, returns ~one row per source file. On a 17k-chunk corpus
    /// with ~3k unique source files this is sub-50 ms. Filter
    /// `source_type='file'` is critical — notes and other sources have
    /// their own mtime semantics.
    pub fn indexed_file_origins(&self) -> Result<HashMap<String, FileFingerprint>, StoreError> {
        let _span = tracing::debug_span!("indexed_file_origins").entered();
        self.rt.block_on(async {
            // GROUP BY origin (one row per file). PF-V1.30.1-8 / #1227: the
            // previous `SELECT DISTINCT origin, source_mtime` returned one
            // row per *distinct (origin, mtime) pair*, which over-counted
            // when an in-flight reindex briefly held two mtimes for the
            // same file. `MAX(source_mtime)` deterministically picks the
            // newer fingerprint; ties are deduped to one row per origin.
            //
            // The fingerprint columns (`source_size`, `source_content_hash`)
            // were added in schema v23 (#1219) and are nullable on
            // pre-migration rows. `MAX` over a NULL column yields NULL,
            // which `read_disk` and `matches` already treat as "no
            // information, assume divergent" — first re-embed populates
            // them and subsequent walks short-circuit on size match.
            // Tuple shape: (origin, mtime, size, content_hash) — all nullable
            // except origin since v23 columns lag pre-migration rows.
            type FpRow = (String, Option<i64>, Option<i64>, Option<Vec<u8>>);
            let rows: Vec<FpRow> = sqlx::query_as(
                "SELECT origin, \
                        MAX(source_mtime) AS mtime, \
                        MAX(source_size) AS size, \
                        MAX(source_content_hash) AS content_hash \
                 FROM chunks \
                 WHERE source_type = 'file' \
                 GROUP BY origin",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|(origin, mtime, size, hash)| {
                    let content_hash = hash.and_then(|bytes| {
                        // Defensive: any pre-migration row with a non-32-byte
                        // BLOB drops to None rather than truncating silently.
                        // Should never happen — we only ever insert 32-byte
                        // BLAKE3 — but the schema declares BLOB so be strict.
                        <[u8; 32]>::try_from(bytes.as_slice()).ok()
                    });
                    let fingerprint = FileFingerprint {
                        mtime,
                        size: size.and_then(|s| u64::try_from(s).ok()),
                        content_hash,
                    };
                    (origin, fingerprint)
                })
                .collect())
        })
    }

    /// #1229: batched per-origin fingerprint lookup. Mirror of
    /// [`indexed_file_origins`] but bounded by the supplied `origins`
    /// list rather than the full file table — lets `run_daemon_reconcile`
    /// stream a 1M-file walk in 1k-row batches without materializing the
    /// whole-tree origin map (`Vec<PathBuf>` + `HashMap<String,
    /// FileFingerprint>` was ~12 MB transient on a 100k-file repo,
    /// scaling linearly to ~120 MB on 1M-file).
    ///
    /// Same `MAX(...)` deduplication semantics as `indexed_file_origins`:
    /// if a row briefly carries two mtimes for the same origin during an
    /// in-flight reindex (RM-17 / #1227), `MAX` picks the newer one and
    /// dedupes to one entry per origin.
    ///
    /// Origins not present in the index are simply absent from the map —
    /// the caller treats those as `Added` (no stored fingerprint) just as
    /// `indexed.get(origin)` returning `None` does in the eager path.
    pub fn fingerprints_for_origins(
        &self,
        origins: &[&str],
    ) -> Result<HashMap<String, FileFingerprint>, StoreError> {
        let _span =
            tracing::debug_span!("fingerprints_for_origins", count = origins.len()).entered();
        if origins.is_empty() {
            return Ok(HashMap::new());
        }
        self.rt.block_on(async {
            use crate::store::helpers::sql::max_rows_per_statement;
            // Each origin binds a single placeholder, so the per-row
            // budget is the parameter limit divided by 1.
            const BATCH_SIZE: usize = max_rows_per_statement(1);
            type FpRow = (String, Option<i64>, Option<i64>, Option<Vec<u8>>);
            let mut out: HashMap<String, FileFingerprint> = HashMap::with_capacity(origins.len());
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT origin, \
                            MAX(source_mtime) AS mtime, \
                            MAX(source_size) AS size, \
                            MAX(source_content_hash) AS content_hash \
                     FROM chunks \
                     WHERE source_type = 'file' AND origin IN ({}) \
                     GROUP BY origin",
                    placeholders
                );
                let mut query = sqlx::query_as::<_, FpRow>(&sql);
                for origin in batch {
                    query = query.bind(*origin);
                }
                for (origin, mtime, size, hash) in query.fetch_all(&self.pool).await? {
                    let content_hash =
                        hash.and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok());
                    out.insert(
                        origin,
                        FileFingerprint {
                            mtime,
                            size: size.and_then(|s| u64::try_from(s).ok()),
                            content_hash,
                        },
                    );
                }
            }
            Ok(out)
        })
    }

    /// Check if specific origins are stale (mtime changed on disk).
    /// Lightweight per-query check: only examines the given origins, not the
    /// entire index. O(result_count), not O(index_size).
    /// `root` is the project root — origins are relative paths joined against it.
    /// Returns the set of stale origin paths.
    pub fn check_origins_stale(
        &self,
        origins: &[&str],
        root: &std::path::Path,
    ) -> Result<HashSet<String>, StoreError> {
        let _span = tracing::info_span!("check_origins_stale", count = origins.len()).entered();
        if origins.is_empty() {
            return Ok(HashSet::new());
        }

        self.rt.block_on(async {
            let mut stale = HashSet::new();

            use crate::store::helpers::sql::max_rows_per_statement;
            const BATCH_SIZE: usize = max_rows_per_statement(1);
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT origin, source_mtime FROM chunks WHERE origin IN ({}) GROUP BY origin",
                    placeholders
                );

                let mut query = sqlx::query_as::<_, (String, Option<i64>)>(&sql);
                for origin in batch {
                    query = query.bind(*origin);
                }
                let rows = query.fetch_all(&self.pool).await?;

                for (origin, stored_mtime) in rows {
                    let stored = match stored_mtime {
                        Some(m) => m,
                        None => {
                            stale.insert(origin);
                            continue;
                        }
                    };

                    // PB-17: Origins in DB always use forward slashes (via normalize_path).
                    debug_assert!(
                        !origin.contains('\\'),
                        "DB origin contains backslash: {origin}"
                    );
                    // PB-23: Normalize the joined path to handle OS-native root
                    // with forward-slash origin (e.g., `C:\proj` + `src/lib.rs`).
                    let path = PathBuf::from(crate::normalize_path(&root.join(&origin)));
                    let current_mtime = path
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(crate::duration_to_mtime_millis);

                    if let Some(current) = current_mtime {
                        if current > stored {
                            stale.insert(origin);
                        }
                    } else {
                        // File deleted or inaccessible — treat as stale
                        stale.insert(origin);
                    }
                }
            }

            Ok(stale)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_utils::make_chunk;
    use crate::parser::{Chunk, ChunkType, Language};
    use crate::test_helpers::{mock_embedding, setup_store};
    use std::collections::HashSet;

    // ===== list_stale_files tests =====

    #[test]
    fn test_list_stale_files_empty_index() {
        let (store, dir) = setup_store();
        let existing = HashSet::new();
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert!(report.stale.is_empty());
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 0);
    }

    #[test]
    fn test_list_stale_files_all_fresh() {
        let (store, dir) = setup_store();

        // Create a real file and index it
        let file_path = dir.path().join("src/fresh.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn fresh() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "fresh".to_string(),
            signature: "fn fresh()".to_string(),
            content: "fn fresh() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "abc".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Get current mtime
        let mtime = crate::duration_to_mtime_millis(
            file_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );

        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(mtime))
            .unwrap();

        let mut existing = HashSet::new();
        existing.insert(file_path);
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert!(report.stale.is_empty());
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    #[test]
    fn test_list_stale_files_detects_modified() {
        let (store, dir) = setup_store();

        let file_path = dir.path().join("src/stale.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn stale() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "stale".to_string(),
            signature: "fn stale()".to_string(),
            content: "fn stale() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "abc".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Store with an old mtime (before the file was created)
        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(1000))
            .unwrap();

        let mut existing = HashSet::new();
        existing.insert(file_path);
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert_eq!(report.stale.len(), 1);
        assert_eq!(report.stale[0].stored_mtime, 1000);
        assert!(report.stale[0].current_mtime > 1000);
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    #[test]
    fn test_list_stale_files_detects_missing() {
        let (store, dir) = setup_store();

        let c = make_chunk("gone", "/nonexistent/file.rs");
        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(1000))
            .unwrap();

        // existing_files doesn't contain the path
        let existing = HashSet::new();
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert!(report.stale.is_empty());
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.total_indexed, 1);
    }

    #[test]
    fn test_list_stale_files_null_mtime() {
        let (store, dir) = setup_store();

        let file_path = dir.path().join("src/null.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn null() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "null".to_string(),
            signature: "fn null()".to_string(),
            content: "fn null() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "abc".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Store with None mtime (will be NULL in DB)
        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], None)
            .unwrap();

        let mut existing = HashSet::new();
        existing.insert(file_path);
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert_eq!(
            report.stale.len(),
            1,
            "NULL mtime should be treated as stale"
        );
    }

    // ===== check_origins_stale tests =====

    #[test]
    fn test_check_origins_stale_empty_list() {
        let (store, _dir) = setup_store();
        let stale = store
            .check_origins_stale(&[], std::path::Path::new("/"))
            .unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn test_check_origins_stale_all_fresh() {
        let (store, dir) = setup_store();

        let file_path = dir.path().join("src/fresh.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn fresh() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "fresh".to_string(),
            signature: "fn fresh()".to_string(),
            content: "fn fresh() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "abc".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let mtime = crate::duration_to_mtime_millis(
            file_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );

        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(mtime))
            .unwrap();

        let stale = store.check_origins_stale(&[&origin], dir.path()).unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn test_check_origins_stale_mixed() {
        let (store, dir) = setup_store();

        // Fresh file
        let fresh_path = dir.path().join("src/fresh.rs");
        std::fs::create_dir_all(fresh_path.parent().unwrap()).unwrap();
        std::fs::write(&fresh_path, "fn fresh() {}").unwrap();

        let fresh_origin = fresh_path.to_string_lossy().to_string();
        let c_fresh = Chunk {
            id: format!("{}:1:fresh", fresh_origin),
            file: fresh_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "fresh".to_string(),
            signature: "fn fresh()".to_string(),
            content: "fn fresh() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "fresh".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let fresh_mtime = crate::duration_to_mtime_millis(
            fresh_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );

        store
            .upsert_chunks_batch(&[(c_fresh, mock_embedding(1.0))], Some(fresh_mtime))
            .unwrap();

        // Stale file (stored with old mtime)
        let stale_path = dir.path().join("src/stale.rs");
        std::fs::write(&stale_path, "fn stale() {}").unwrap();

        let stale_origin = stale_path.to_string_lossy().to_string();
        let c_stale = Chunk {
            id: format!("{}:1:stale", stale_origin),
            file: stale_path,
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "stale".to_string(),
            signature: "fn stale()".to_string(),
            content: "fn stale() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "stale".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        store
            .upsert_chunks_batch(&[(c_stale, mock_embedding(2.0))], Some(1000))
            .unwrap();

        let stale = store
            .check_origins_stale(&[&fresh_origin, &stale_origin], dir.path())
            .unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale.contains(&stale_origin));
        assert!(!stale.contains(&fresh_origin));
    }

    #[test]
    fn test_check_origins_stale_unknown_origin() {
        let (store, _dir) = setup_store();
        let stale = store
            .check_origins_stale(&["nonexistent/file.rs"], std::path::Path::new("/"))
            .unwrap();
        assert!(
            stale.is_empty(),
            "Unknown origin should not appear in stale set"
        );
    }

    /// #1182 (Layer 2): empty index returns empty origin map.
    #[test]
    fn test_indexed_file_origins_empty_store() {
        let (store, _dir) = setup_store();
        let map = store.indexed_file_origins().expect("indexed_file_origins");
        assert!(map.is_empty());
    }

    /// #1182 (Layer 2): inserted file chunks surface as origin → mtime
    /// entries. Pinned mtime so the test isn't sensitive to clock.
    #[test]
    fn test_indexed_file_origins_returns_origin_mtime_pairs() {
        let (store, dir) = setup_store();
        let chunk1 = chunk_at(dir.path(), "src/alpha.rs", "alpha");
        let chunk2 = chunk_at(dir.path(), "src/beta.rs", "beta");
        store
            .upsert_chunks_batch(
                &[(chunk1, mock_embedding(1.0)), (chunk2, mock_embedding(2.0))],
                Some(1_700_000_000_000),
            )
            .unwrap();

        let map = store.indexed_file_origins().expect("indexed_file_origins");
        assert_eq!(
            map.len(),
            2,
            "expected one entry per source file, got {map:?}"
        );
        // `chunk_at` stores the absolute path as the origin (since it
        // joins `dir.path()` and the relative path). Verify by suffix
        // match — exact path varies per tempdir.
        let keys: Vec<&String> = map.keys().collect();
        assert!(keys.iter().any(|k| k.ends_with("alpha.rs")));
        assert!(keys.iter().any(|k| k.ends_with("beta.rs")));
        // All entries pin the bound mtime.
        for v in map.values() {
            assert_eq!(v.mtime, Some(1_700_000_000_000));
        }
    }

    /// #1182 (Layer 2): rows without `source_type='file'` are filtered
    /// out. The current store schema only emits `source_type='file'` for
    /// indexed code chunks, but the explicit WHERE clause guards against
    /// future schemas (e.g. test fixtures or synthetic origins) leaking
    /// into the reconcile loop.
    #[test]
    fn test_indexed_file_origins_only_returns_one_per_source_file() {
        let (store, dir) = setup_store();
        // Two chunks in the same source file → still one origin entry.
        let mut a = chunk_at(dir.path(), "src/main.rs", "func_a");
        a.id = format!("src/main.rs:1:{}", &a.content_hash[..8]);
        let mut b = chunk_at(dir.path(), "src/main.rs", "func_b");
        b.id = format!("src/main.rs:5:{}", &b.content_hash[..8]);
        store
            .upsert_chunks_batch(
                &[(a, mock_embedding(1.0)), (b, mock_embedding(2.0))],
                Some(1_000),
            )
            .unwrap();

        let map = store.indexed_file_origins().expect("indexed_file_origins");
        assert_eq!(map.len(), 1);
        assert!(map.keys().any(|k| k.ends_with("src/main.rs")));
    }

    // ===== prune_all tests (TC-HP-3) =====

    /// Helper: build a Chunk rooted at `dir` with the given relative path.
    fn chunk_at(dir: &std::path::Path, rel: &str, name: &str) -> Chunk {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, format!("fn {name}() {{}}")).unwrap();
        let content = format!("fn {name}() {{}}");
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("{}:1:{}", path.display(), &hash[..8]),
            file: path,
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// TC-HP-3 (happy path): `prune_all` removes chunks for files deleted
    /// from disk, counts reflect the prune, remaining chunks intact.
    #[test]
    fn test_prune_all_happy_path() {
        let (store, dir) = setup_store();

        // Index 3 files
        let c1 = chunk_at(dir.path(), "src/a.rs", "a");
        let c2 = chunk_at(dir.path(), "src/b.rs", "b");
        let c3 = chunk_at(dir.path(), "src/c.rs", "c");
        let files_on_disk = [c1.file.clone(), c2.file.clone(), c3.file.clone()];
        store
            .upsert_chunks_batch(
                &[
                    (c1, mock_embedding(1.0)),
                    (c2, mock_embedding(2.0)),
                    (c3, mock_embedding(3.0)),
                ],
                Some(1000),
            )
            .unwrap();

        // Delete one file from disk
        std::fs::remove_file(&files_on_disk[1]).unwrap();

        // existing_files contains only the two remaining files
        let existing: HashSet<_> = vec![files_on_disk[0].clone(), files_on_disk[2].clone()]
            .into_iter()
            .collect();

        let result = store.prune_all(&existing, dir.path()).unwrap();
        assert_eq!(result.pruned_chunks, 1, "Should prune exactly 1 chunk");
        // No function_calls / type_edges / summaries were inserted, so these
        // counters should be zero.
        assert_eq!(result.pruned_calls, 0);
        assert_eq!(result.pruned_type_edges, 0);
        assert_eq!(result.pruned_summaries, 0);

        // Remaining chunks are intact
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 2);
    }

    /// Regression: the old `ends_with` heuristic treated chunk origin
    /// `cuvs-fork-push/CHANGELOG.md` as matching the root file
    /// `CHANGELOG.md`, leaving the orphan in the DB. With the filesystem
    /// existence check, the nested origin is correctly pruned when the
    /// nested directory does not exist on disk.
    #[test]
    fn test_prune_all_suffix_match_regression() {
        let (store, dir) = setup_store();

        // Root-level file that does exist on disk
        let root_chunk = chunk_at(dir.path(), "CHANGELOG.md", "root_changelog");
        // Synthetic chunk whose origin tail-matches the root file, but whose
        // directory does not exist on disk.
        let mut orphan = make_chunk("orphan_changelog", "cuvs-fork-push/CHANGELOG.md");
        orphan.id = format!(
            "cuvs-fork-push/CHANGELOG.md:1:{}",
            &orphan.content_hash[..8]
        );

        let existing: HashSet<_> = vec![root_chunk.file.clone()].into_iter().collect();

        store
            .upsert_chunks_batch(
                &[
                    (root_chunk, mock_embedding(1.0)),
                    (orphan, mock_embedding(2.0)),
                ],
                Some(1000),
            )
            .unwrap();

        let result = store.prune_all(&existing, dir.path()).unwrap();
        assert_eq!(
            result.pruned_chunks, 1,
            "Expected orphan cuvs-fork-push/CHANGELOG.md to be pruned (would have been retained by the old ends_with heuristic)"
        );

        // Only the root CHANGELOG.md remains
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    /// Regression: `.claude/worktrees/agent-X/src/foo.rs` must be pruned
    /// when only `src/foo.rs` is on disk. The suffix match retained these
    /// as "not missing" because the worktree tail matched the real path.
    #[test]
    fn test_prune_all_worktree_regression() {
        let (store, dir) = setup_store();

        // Legitimate root-level source file
        let real = chunk_at(dir.path(), "src/foo.rs", "foo_real");
        // Worktree duplicate — synthesize without writing to disk so the
        // filesystem check confirms it does not exist.
        let mut worktree = make_chunk("foo_worktree", ".claude/worktrees/agent-X/src/foo.rs");
        worktree.id = format!(
            ".claude/worktrees/agent-X/src/foo.rs:1:{}",
            &worktree.content_hash[..8]
        );

        let existing: HashSet<_> = vec![real.file.clone()].into_iter().collect();

        store
            .upsert_chunks_batch(
                &[(real, mock_embedding(1.0)), (worktree, mock_embedding(2.0))],
                Some(1000),
            )
            .unwrap();

        let result = store.prune_all(&existing, dir.path()).unwrap();
        assert_eq!(
            result.pruned_chunks, 1,
            "Worktree duplicate origin should be pruned"
        );

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    /// Regression for the production bug surfaced by the R@5 audit:
    /// `enumerate_files` correctly skips nested git worktrees (the
    /// directory-with-`.git`-as-file filter at `lib.rs:480-486`), so
    /// worktree-prefixed origins do NOT appear in `existing_files`. But
    /// the worktree files DO exist on disk while the agent runs, and the
    /// old `origin_exists` had a `Path::exists()` fallback that returned
    /// `true` for any extant file regardless of indexer ownership. Result:
    /// chunks for `.claude/worktrees/agent-X/...` survived GC and polluted
    /// search results. Fix: drop the `exists()` fallback; canonicalize on
    /// both sides and require strict membership in `existing_files`.
    #[test]
    fn test_prune_all_drops_worktree_chunks_when_files_exist_on_disk() {
        let (store, dir) = setup_store();

        // The "real" file the indexer owns.
        let real = chunk_at(dir.path(), "src/foo.rs", "foo_real");

        // The worktree carve-out — write the file to disk so `exists()`
        // returns true (this is the production scenario the old fallback
        // mishandled).
        let worktree_rel = ".claude/worktrees/agent-X/src/foo.rs";
        let worktree_path = dir.path().join(worktree_rel);
        std::fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
        std::fs::write(&worktree_path, "fn foo_worktree() {}").unwrap();

        let mut worktree_chunk = make_chunk("foo_worktree", worktree_rel);
        worktree_chunk.id = format!("{}:1:{}", worktree_rel, &worktree_chunk.content_hash[..8]);

        // `existing_files` mirrors what `enumerate_files` produces: the
        // canonical absolute path of the real file, NOT the worktree.
        let real_canonical = dunce::canonicalize(&real.file).unwrap();
        let existing: HashSet<_> = vec![real_canonical].into_iter().collect();

        store
            .upsert_chunks_batch(
                &[
                    (real, mock_embedding(1.0)),
                    (worktree_chunk, mock_embedding(2.0)),
                ],
                Some(1000),
            )
            .unwrap();

        let result = store.prune_all(&existing, dir.path()).unwrap();
        assert_eq!(
            result.pruned_chunks, 1,
            "Worktree chunk must be pruned even when the file exists on disk \
             (origin_exists no longer falls through to Path::exists())"
        );

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    /// TC-HP-3 gap-fill: the happy-path test above asserts
    /// `pruned_calls/type_edges/summaries == 0` because nothing was inserted
    /// into those tables. This test actually populates each of the four
    /// cascade tables and verifies that deleting the source file propagates
    /// through every counter. A refactor that short-circuits any of steps
    /// 2b / 2c / 2d would survive the happy-path test — this one catches it.
    #[test]
    fn test_prune_all_cascade_populates_all_counters() {
        use crate::parser::{CallSite, FunctionCalls, TypeRef};

        let (store, dir) = setup_store();

        // Keeper + victim files.
        let keeper_chunk = chunk_at(dir.path(), "src/keep.rs", "keep");
        let victim_chunk = chunk_at(dir.path(), "src/victim.rs", "victim");
        let keeper_file = keeper_chunk.file.clone();
        let victim_file = victim_chunk.file.clone();
        let victim_chunk_id = victim_chunk.id.clone();
        let victim_content_hash = victim_chunk.content_hash.clone();

        store
            .upsert_chunks_batch(
                &[
                    (keeper_chunk, mock_embedding(1.0)),
                    (victim_chunk, mock_embedding(2.0)),
                ],
                Some(1000),
            )
            .unwrap();

        // function_calls orphan: two call sites from victim.rs. Once the
        // file is gone, both rows become orphans per the `DELETE WHERE file
        // NOT IN (SELECT DISTINCT origin FROM chunks)` query in prune_all.
        store
            .upsert_function_calls(
                &victim_file,
                &[FunctionCalls {
                    name: "victim".to_string(),
                    line_start: 1,
                    calls: vec![
                        CallSite {
                            callee_name: "helper_a".to_string(),
                            line_number: 2,
                        },
                        CallSite {
                            callee_name: "helper_b".to_string(),
                            line_number: 3,
                        },
                    ],
                }],
            )
            .unwrap();

        // type_edges orphan: one edge whose source_chunk_id is the victim
        // chunk. After the chunk is deleted, the edge becomes an orphan.
        store
            .upsert_type_edges(
                &victim_chunk_id,
                &[TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 2,
                    kind: None,
                }],
            )
            .unwrap();

        // llm_summaries orphan: one summary row tied to the victim chunk's
        // content_hash. When the chunk is deleted, no chunk row references
        // that hash any more — the summary becomes an orphan.
        store
            .upsert_summaries_batch(&[(
                victim_content_hash,
                "summary body".to_string(),
                "test-model".to_string(),
                "general".to_string(),
            )])
            .unwrap();

        // Simulate the source file being deleted on disk. `prune_all` filters
        // against existing_files via `origin_exists`, and the filesystem check
        // kicks in when we drop the path from the HashSet — we don't have to
        // `remove_file` because `chunk_at` wrote a placeholder file that we
        // can safely ignore (the check prefers the HashSet hit first).
        std::fs::remove_file(&victim_file).unwrap();
        let existing: HashSet<_> = vec![keeper_file.clone()].into_iter().collect();

        let result = store.prune_all(&existing, dir.path()).unwrap();
        assert_eq!(
            result.pruned_chunks, 1,
            "Victim chunk should be pruned (keeper intact)"
        );
        assert!(
            result.pruned_calls >= 2,
            "Both function_calls rows for victim.rs must be pruned, got {}",
            result.pruned_calls
        );
        // type_edges has FK `source_chunk_id REFERENCES chunks(id) ON DELETE
        // CASCADE`, so the rows disappear when the chunk is deleted in step
        // 2a. The explicit `DELETE FROM type_edges WHERE source_chunk_id NOT
        // IN (SELECT id FROM chunks)` at step 2c finds nothing to prune — the
        // zero counter is correct behavior, not a leak.
        assert_eq!(
            result.pruned_type_edges, 0,
            "type_edges cascade-deletes with chunks — the explicit DELETE sees zero orphans"
        );
        assert!(
            result.pruned_summaries >= 1,
            "llm_summaries rows for victim hash must be pruned, got {}",
            result.pruned_summaries
        );

        // Keeper chunk survives; no other side effects.
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    /// TC-HP-3 gap-fill: baseline "nothing to prune". When every file still
    /// exists on disk, prune_all must return an all-zero `PruneAllResult`.
    /// Regression guard for refactors that change the default branch to
    /// unconditionally prune orphans when there are none.
    #[test]
    fn test_prune_all_nothing_to_prune_returns_zeroes() {
        let (store, dir) = setup_store();

        let c1 = chunk_at(dir.path(), "src/x.rs", "x");
        let c2 = chunk_at(dir.path(), "src/y.rs", "y");
        let files_on_disk = [c1.file.clone(), c2.file.clone()];
        store
            .upsert_chunks_batch(
                &[(c1, mock_embedding(1.0)), (c2, mock_embedding(2.0))],
                Some(1000),
            )
            .unwrap();

        let existing: HashSet<_> = files_on_disk.iter().cloned().collect();
        let result = store.prune_all(&existing, dir.path()).unwrap();
        assert_eq!(result.pruned_chunks, 0);
        assert_eq!(result.pruned_calls, 0);
        assert_eq!(result.pruned_type_edges, 0);
        assert_eq!(result.pruned_summaries, 0);

        // Chunks are untouched.
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 2);
    }

    // ===== mtime semantics tests (issue #975) =====
    //
    // The staleness predicate in `list_stale_files` is `current > stored`,
    // strict-greater-than. Two tests pin the boundary behaviour so any
    // refactor to `current != stored` fails loudly:
    //   - Equal mtime: fresh (not stale).
    //   - Stored mtime newer than disk (backup restore): fresh (not stale).
    // A naive `current != stored` rewrite would report both as stale,
    // triggering a full re-embed on backup-restore and wasting hours.

    /// Equal mtime must be treated as fresh. Tests the boundary of the
    /// `current > stored` predicate — a refactor to `>=` or `!=` would
    /// flip this case and report the file as stale.
    #[test]
    fn test_list_stale_files_mtime_equal_is_fresh() {
        let (store, dir) = setup_store();

        // Create a file and capture its current mtime.
        let file_path = dir.path().join("src/equal.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn equal() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "equal".to_string(),
            signature: "fn equal()".to_string(),
            content: "fn equal() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "abc".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Store with the exact mtime currently on disk.
        let mtime = crate::duration_to_mtime_millis(
            file_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );

        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(mtime))
            .unwrap();

        let mut existing = HashSet::new();
        existing.insert(file_path);
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert!(
            report.stale.is_empty(),
            "Equal stored/current mtime must not be reported as stale, got {:?}",
            report.stale
        );
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    /// Backup-restore case: stored mtime is *newer* than disk. This
    /// happens when a user restores a backup of the DB while the source
    /// files are older than when the DB was last written. Must be
    /// treated as fresh (not stale), because the stored data was
    /// generated from a version of the file that is no older than the
    /// one currently on disk. A naive `current != stored` refactor
    /// would report these as stale and corrupt the index on the next
    /// re-embed pass.
    #[test]
    fn test_list_stale_files_stored_newer_is_fresh() {
        let (store, dir) = setup_store();

        let file_path = dir.path().join("src/backup.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn backup() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "backup".to_string(),
            signature: "fn backup()".to_string(),
            content: "fn backup() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "abc".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Store with an mtime 10_000_000 ms (~2.7 hours) in the future
        // relative to the file on disk. Pins `current > stored` (false)
        // → fresh.
        let disk_mtime = crate::duration_to_mtime_millis(
            file_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );
        let future_mtime = disk_mtime + 10_000_000;

        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(future_mtime))
            .unwrap();

        let mut existing = HashSet::new();
        existing.insert(file_path);
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert!(
            report.stale.is_empty(),
            "Stored mtime newer than disk (backup-restore) must not be reported as stale, got {:?}",
            report.stale
        );
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    // ===== Daemon startup GC tests (#1024) =====
    //
    // These four tests pin the contract for the two prune passes the
    // `cqs watch --serve` startup hook calls:
    //
    //   1. `prune_missing` — drop chunks whose origin no longer exists on
    //      disk (e.g. file deleted while the daemon was down).
    //   2. `prune_gitignored` — drop chunks whose path is now matched by
    //      `.gitignore` (retroactive cleanup of pre-v1.26.0 pollution
    //      that accumulated before `cqs watch` started honoring
    //      `.gitignore`).
    //
    // Together they let the daemon idempotently reach the same chunk-count
    // a fresh `cqs index --force` would produce, without rebuilding from
    // scratch.

    /// Build an `ignore::gitignore::Gitignore` matcher rooted at `root`
    /// from the supplied pattern lines. Mirrors the `gitignore_from_lines`
    /// helper in `src/cli/watch.rs::tests` so the staleness tests can build
    /// matchers without importing from the binary crate.
    fn matcher_from_lines(root: &std::path::Path, lines: &[&str]) -> ignore::gitignore::Gitignore {
        let mut b = ignore::gitignore::GitignoreBuilder::new(root);
        for line in lines {
            b.add_line(None, line).expect("add_line");
        }
        b.build().expect("build gitignore")
    }

    /// Pass 1 — when the source file is gone from disk and absent from
    /// `existing_files`, `prune_missing` must drop its chunks. Mirrors the
    /// "deleted file" half of the daemon-startup pollution motivating
    /// case (worktrees + deleted files).
    #[test]
    fn test_prune_missing_drops_chunks_for_deleted_files() {
        let (store, dir) = setup_store();

        // Seed two chunks: one for an actually-present file, one for a
        // path the test never creates.
        let kept_path = dir.path().join("src/keep.rs");
        std::fs::create_dir_all(kept_path.parent().unwrap()).unwrap();
        std::fs::write(&kept_path, "fn keep() {}").unwrap();
        let kept_origin = kept_path.to_string_lossy().to_string();
        let kept_chunk = Chunk {
            id: format!("{}:1:keep", kept_origin),
            file: kept_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "keep".to_string(),
            signature: "fn keep()".to_string(),
            content: "fn keep() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "keep".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let gone = make_chunk("gone", "/no/such/file.rs");

        store
            .upsert_chunks_batch(
                &[
                    (kept_chunk, mock_embedding(1.0)),
                    (gone, mock_embedding(2.0)),
                ],
                Some(1000),
            )
            .unwrap();

        // Existing files contains only the kept path; the deleted file is
        // omitted. `prune_missing` must drop the orphan chunk.
        let mut existing = HashSet::new();
        existing.insert(kept_path);
        let pruned = store.prune_missing(&existing, dir.path()).unwrap();
        assert_eq!(
            pruned, 1,
            "Should prune exactly 1 chunk for the deleted file"
        );

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1, "Kept chunk must survive");
    }

    /// Pass 1 baseline — when every chunk's source file is still on disk,
    /// `prune_missing` must return 0 and leave the index untouched.
    /// Regression guard: a refactor that flips the existence check would
    /// silently delete the entire index on the next daemon startup.
    #[test]
    fn test_prune_missing_keeps_chunks_for_present_files() {
        let (store, dir) = setup_store();

        // Two real files on disk, two chunks indexed.
        let p1 = dir.path().join("src/a.rs");
        let p2 = dir.path().join("src/b.rs");
        std::fs::create_dir_all(p1.parent().unwrap()).unwrap();
        std::fs::write(&p1, "fn a() {}").unwrap();
        std::fs::write(&p2, "fn b() {}").unwrap();

        let mk = |path: &std::path::Path, name: &str, hash: &str| {
            let origin = path.to_string_lossy().to_string();
            Chunk {
                id: format!("{}:1:{}", origin, hash),
                file: path.to_path_buf(),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {}()", name),
                content: format!("fn {}() {{}}", name),
                doc: None,
                line_start: 1,
                line_end: 1,
                content_hash: hash.to_string(),
                parent_id: None,
                window_idx: None,
                parent_type_name: None,
                parser_version: 0,
            }
        };
        let c1 = mk(&p1, "a", "ahash");
        let c2 = mk(&p2, "b", "bhash");

        store
            .upsert_chunks_batch(
                &[(c1, mock_embedding(1.0)), (c2, mock_embedding(2.0))],
                Some(1000),
            )
            .unwrap();

        let existing: HashSet<_> = vec![p1, p2].into_iter().collect();
        let pruned = store.prune_missing(&existing, dir.path()).unwrap();
        assert_eq!(
            pruned, 0,
            "No files are missing — prune_missing must return 0"
        );

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 2, "Both chunks must survive");
    }

    /// Pass 2 — when `.gitignore` ignores `target/`, a chunk whose origin
    /// is `target/cache.rs` must be pruned. Models the canonical
    /// pre-v1.26.0 pollution case (worktrees, build artifacts, etc.).
    #[test]
    fn test_prune_gitignored_drops_chunks_in_ignored_paths() {
        let (store, dir) = setup_store();

        // Build an "indexed-before-gitignore" chunk under `target/`. The
        // file does not need to exist on disk for this test — the gitignore
        // matcher walks the path string, not the filesystem.
        let target_chunk = make_chunk("cache", "target/cache.rs");
        let kept_path = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(kept_path.parent().unwrap()).unwrap();
        std::fs::write(&kept_path, "pub fn lib() {}").unwrap();
        let kept_origin = kept_path.to_string_lossy().to_string();
        let kept_chunk = Chunk {
            id: format!("{}:1:lib", kept_origin),
            file: kept_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "lib".to_string(),
            signature: "pub fn lib()".to_string(),
            content: "pub fn lib() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "lib".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        store
            .upsert_chunks_batch(
                &[
                    (target_chunk, mock_embedding(1.0)),
                    (kept_chunk, mock_embedding(2.0)),
                ],
                Some(1000),
            )
            .unwrap();

        let matcher = matcher_from_lines(dir.path(), &["target/", ".claude/"]);
        let pruned = store.prune_gitignored(&matcher, dir.path(), None).unwrap();
        assert_eq!(
            pruned, 1,
            "target/cache.rs is now gitignored — must be pruned"
        );

        // The src/lib.rs chunk survives.
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    /// Pass 2 baseline — when `.gitignore` only matches `target/`, a chunk
    /// for `src/lib.rs` must survive. Regression guard: a refactor that
    /// inverts the matcher result (or accidentally treats `Whitelist` as
    /// "ignore") would wipe the entire tracked source tree on first
    /// daemon startup.
    #[test]
    fn test_prune_gitignored_keeps_chunks_in_tracked_paths() {
        let (store, dir) = setup_store();

        let kept_path = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(kept_path.parent().unwrap()).unwrap();
        std::fs::write(&kept_path, "pub fn lib() {}").unwrap();
        let kept_origin = kept_path.to_string_lossy().to_string();
        let kept_chunk = Chunk {
            id: format!("{}:1:lib", kept_origin),
            file: kept_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "lib".to_string(),
            signature: "pub fn lib()".to_string(),
            content: "pub fn lib() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: "lib".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        store
            .upsert_chunks_batch(&[(kept_chunk, mock_embedding(1.0))], Some(1000))
            .unwrap();

        let matcher = matcher_from_lines(dir.path(), &["target/"]);
        let pruned = store.prune_gitignored(&matcher, dir.path(), None).unwrap();
        assert_eq!(pruned, 0, "src/lib.rs is not gitignored — must be kept");

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 1);
    }

    /// TC-ADV-1.30.1-6 / #1243: `FileFingerprint::read_disk` returns
    /// `None` when `metadata()` fails. The `reconcile.rs:188` "leave
    /// to GC" branch is keyed on this contract: if `metadata()` errs
    /// (deleted-since-walk, permission flip on the parent dir,
    /// transient AV scan), reconcile must skip the file rather than
    /// queue it for reindex — otherwise every unreadable file would
    /// re-queue every reconcile pass (default 30 s) until external
    /// state changed.
    ///
    /// A nonexistent path is the cleanest reproduction of the failure
    /// mode: deleted-since-walk, permission-denied on parent, and the
    /// transient AV-scan window all surface the same way at line 133's
    /// `metadata().ok()? -> None`. Pinning the contract here protects
    /// against a future "helpful" refactor that defaults the
    /// fingerprint fields and silently re-queues unreadable files.
    #[test]
    fn test_read_disk_returns_none_on_metadata_err() {
        use crate::store::{FileFingerprint, FingerprintPolicy};
        use std::path::PathBuf;
        let nonexistent = PathBuf::from("/nonexistent/cqs-test-path-must-not-exist-87a4f1ec");
        let stored = FileFingerprint {
            mtime: Some(0),
            size: Some(0),
            content_hash: None,
        };
        let result =
            FileFingerprint::read_disk(&nonexistent, &stored, FingerprintPolicy::MtimeOrHash);
        assert!(
            result.is_none(),
            "metadata() failure must surface as None — the leave-to-GC signal that reconcile.rs:188 relies on"
        );
    }
}
