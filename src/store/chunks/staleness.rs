// WRITE_LOCK guard is held across .await inside block_on(). Safe because
// block_on runs single-threaded — no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Staleness checks and pruning for missing/stale files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::store::helpers::sql::max_rows_per_statement;
use crate::store::helpers::{StaleFile, StaleReport, StoreError};
use crate::store::{ReadWrite, Store};

/// Per-file fingerprint stored alongside each chunk for the reconcile path.
///
/// All three fields are nullable. Both production index paths populate the
/// full fingerprint at write time: the bulk pipeline stamps it inside the
/// chunk-write transaction (`Store::upsert_embedded_batch`) and the watch
/// reindex path stamps it right after its per-file upsert
/// (`Store::set_file_fingerprint`). `source_size` / `source_content_hash`
/// remain `NULL` only for rows written by lower-level upserts (tests,
/// `upsert_chunks_batch` callers) or by pre-v23 binaries; `mtime` is `None`
/// when `source_mtime` is `NULL` (FS without modification time).
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
/// two reconcile hazards:
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
    /// Compare mtime only. Cheap; misses both hazards above. Useful for
    /// tests that want the mtime-only shape and for environments where
    /// reading every divergent file is prohibitive.
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
                // Rows with `content_hash = None` are treated as "no
                // information, assume divergent" — the next reindex will
                // populate the hash and subsequent walks will be quick.
                match (&self.content_hash, &disk.content_hash) {
                    (Some(a), Some(b)) => a == b && self.size == disk.size,
                    _ => false,
                }
            }
            FingerprintPolicy::MtimeOrHash => {
                // If the stored fingerprint has only mtime — `source_size`
                // and `source_content_hash` are NULL — fall back to mtime
                // equality. Without this, every such chunk row would be
                // classified divergent on every reconcile pass until first
                // re-embed populated the new columns, drowning the watch
                // queue. Once the row has any fingerprint field populated,
                // we use the strict comparison below.
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
                // Stored has only mtime, so `matches` uses mtime equality —
                // no hash needed. Skipping the hash here matters: every
                // reconcile walk would otherwise BLAKE3 the entire tree on
                // the first size-mismatch (stored=None vs disk=Some(N))
                // until first re-embed.
                if stored.size.is_none() && stored.content_hash.is_none() {
                    false
                } else {
                    stored.mtime != mtime || stored.size != size
                }
            }
        };

        // Streaming blake3 with bounded RSS: Hasher::update_reader keeps the
        // working set at ~64 KiB, so a large file (e.g. a multi-GB SQL dump)
        // is hashed without slurping it into memory.
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
/// it — that's how `.claude/worktrees/agent-X/...` chunks would survive GC
/// even though `enumerate_files` correctly skips them. Canonicalizing both
/// sides removes the path-form mismatch that a fallback would otherwise
/// compensate for.
///
/// Origins are stored as slash-normalized absolute paths (via
/// `normalize_path`), while `existing_files` holds OS-native `PathBuf`s
/// (backslashes on Windows). Callers pre-compute a parallel
/// `HashSet<String>` of slash-normalized entries once per prune and pass
/// it in — the fast string hash probe hits for real matches, and
/// `dunce::canonicalize` only fires for true misses (e.g. stale relative
/// origins).
fn origin_exists(
    origin: &str,
    existing_files: &HashSet<PathBuf>,
    existing_normalized: Option<&HashSet<String>>,
    root: &Path,
) -> bool {
    // Fast string path: origins and the pre-normalized set are both in
    // slash form, so on Windows the common case is a hash probe rather than
    // a dunce::canonicalize syscall.
    if let Some(set) = existing_normalized {
        if set.contains(origin) {
            return true;
        }
    }

    let origin_path = PathBuf::from(origin);
    // Cheap path: exact PathBuf match as stored. No-cost fallback for
    // callers that pass an already-matching HashSet without pre-normalizing.
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

/// Delete every chunk (FTS + `chunks` + per-file `function_calls`) for the
/// given origins inside an already-open write transaction, then sweep orphan
/// `sparse_vectors`. Returns the number of `chunks` rows deleted.
///
/// Shared by all three wholesale-origin prune paths (`prune_missing`,
/// `prune_all`, `prune_gitignored`). Each used to inline this ~25-line
/// FTS+chunks+sparse sequence, and they had diverged on `function_calls`:
/// `prune_missing` deleted per batch, `prune_all` did a global sweep, and
/// `prune_gitignored` skipped it entirely — leaving orphan call-graph rows
/// (ghost callers) for gitignore-driven prunes until the next `prune_all`.
/// Routing all three through here closes that divergence: `function_calls`
/// is always cleaned for the origins being removed.
///
/// `function_calls` has no FK to `chunks` (it is keyed by `file`, not chunk
/// ID), so deleting chunks does not cascade; the explicit per-batch DELETE
/// over the same `file` values is required. The sparse sweep mirrors the FK
/// CASCADE the table can't express via SQL alone.
///
/// `origins` must be non-empty; callers short-circuit on the empty case.
async fn delete_origins_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    origins: &[String],
    span_label: &'static str,
) -> Result<u32, StoreError> {
    // Single-bind IN-list batched at the modern SQLite variable limit. The
    // caller's single transaction wraps ALL batches — a partial prune on
    // crash would leave the index inconsistent with disk.
    const BATCH_SIZE: usize = max_rows_per_statement(1);
    let mut deleted = 0u32;

    for batch in origins.chunks(BATCH_SIZE) {
        let placeholder_str = crate::store::helpers::make_placeholders(batch.len());

        // Delete from FTS first.
        let fts_query = format!(
            "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin IN ({}))",
            placeholder_str
        );
        let mut fts_stmt = sqlx::query(sqlx::AssertSqlSafe(fts_query.as_str()));
        for origin in batch {
            fts_stmt = fts_stmt.bind(origin);
        }
        fts_stmt.execute(&mut **tx).await?;

        // Delete from chunks.
        let chunks_query = format!("DELETE FROM chunks WHERE origin IN ({})", placeholder_str);
        let mut chunks_stmt = sqlx::query(sqlx::AssertSqlSafe(chunks_query.as_str()));
        for origin in batch {
            chunks_stmt = chunks_stmt.bind(origin);
        }
        let result = chunks_stmt.execute(&mut **tx).await?;
        deleted += result.rows_affected() as u32;

        // Delete orphan `function_calls` for the same files. Without this,
        // prune leaves call-graph rows that surface as ghost callers in
        // `cqs callers`/`callees`/`dead`.
        let calls_query = format!(
            "DELETE FROM function_calls WHERE file IN ({})",
            placeholder_str
        );
        let mut calls_stmt = sqlx::query(sqlx::AssertSqlSafe(calls_query.as_str()));
        for origin in batch {
            calls_stmt = calls_stmt.bind(origin);
        }
        calls_stmt.execute(&mut **tx).await?;

        // v32: the `candidate_edges` side-table is file-keyed too, so the same
        // wholesale prune must drop its rows for the removed origins — otherwise
        // a deleted/gitignored file leaves orphan candidate rows behind.
        let candidate_query = format!(
            "DELETE FROM candidate_edges WHERE file IN ({})",
            placeholder_str
        );
        let mut candidate_stmt = sqlx::query(sqlx::AssertSqlSafe(candidate_query.as_str()));
        for origin in batch {
            candidate_stmt = candidate_stmt.bind(origin);
        }
        candidate_stmt.execute(&mut **tx).await?;

        // Prune the v29 `file_registry` fingerprint for the same origins
        // #1774. These origins are being removed wholesale (file gone from
        // disk / now gitignored), so the persisted zero-chunk fingerprint must
        // go with them — otherwise a deleted-then-recreated file would be
        // treated as unchanged against a stale registry row. Same transaction
        // as the chunk delete so the registry never outlives the chunks.
        let registry_query = format!(
            "DELETE FROM file_registry WHERE origin IN ({})",
            placeholder_str
        );
        let mut registry_stmt = sqlx::query(sqlx::AssertSqlSafe(registry_query.as_str()));
        for origin in batch {
            registry_stmt = registry_stmt.bind(origin);
        }
        registry_stmt.execute(&mut **tx).await?;
    }

    // Sweep orphan sparse_vectors inside the same transaction so no window
    // exists where stale sparse vectors inflate the SPLADE index.
    if deleted > 0 {
        let sparse_result = sqlx::query(
            "DELETE FROM sparse_vectors WHERE chunk_id NOT IN \
             (SELECT id FROM chunks)",
        )
        .execute(&mut **tx)
        .await?;
        let pruned_sparse = sparse_result.rows_affected();
        if pruned_sparse > 0 {
            tracing::debug!(
                pruned_sparse,
                label = span_label,
                "Pruned orphan sparse vectors"
            );
        }
    }

    Ok(deleted)
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
            // Acquire the write transaction BEFORE reading origins. Reading
            // outside the tx creates a TOCTOU window where a concurrent
            // `cqs watch` upsert adds a chunk for a file between the SELECT
            // and the DELETE. Because that file also isn't in the caller's
            // `existing_files` snapshot (gathered before this call), our
            // stale origin list would flag it as missing and wipe the
            // just-added row on DELETE. Serialising the read under the write
            // lock closes it.
            let (_guard, mut tx) = self.begin_write().await?;

            // UNION (not UNION ALL) dedupes across the two sources. The
            // `file_registry` arm is load-bearing for v29 #1774: a zero-chunk
            // origin has NO chunk row, so without it a deleted comment-only
            // file would never enter the missing-set and its registry row
            // would leak forever — a permanent phantom in `list_stale_files`'
            // missing list that no GC pass could ever collect.
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT origin FROM chunks WHERE source_type = 'file' \
                 UNION \
                 SELECT origin FROM file_registry",
            )
            .fetch_all(&mut *tx)
            .await?;

            // Reconcile stored origins against current filesystem state via
            // `origin_exists`. Pre-compute the slash-normalized string set
            // once so `origin_exists` hits the cheap string path for Windows
            // origins (stored with `/` while `existing_files` holds `\`).
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

            // Delete FTS + chunks + function_calls per batch and sweep orphan
            // sparse_vectors — shared with prune_all / prune_gitignored.
            let deleted = delete_origins_in_tx(&mut tx, &missing, "prune_missing").await?;

            tx.commit().await?;

            if deleted > 0 {
                tracing::info!(
                    deleted,
                    files = missing.len(),
                    "Pruned chunks for missing files"
                );
            }

            Ok(deleted)
        })
    }

    /// Run all prune operations in a single SQLite transaction.
    /// Ensures concurrent readers never see an inconsistent state where chunks
    /// are deleted but orphan call graph / type edge / summary entries remain.
    //
    // The SELECT DISTINCT runs inside the write transaction. If `cqs watch`
    // (which only takes `try_acquire_index_lock`) interleaves an
    // `upsert_chunks_calls_and_prune` call for a freshly-added file, that file's
    // origin is missing from `existing_files` (caller-passed snapshot) yet
    // present in `chunks`; snapshotting inside the write transaction so we
    // observe a post-watch-reindex-consistent view prevents the DELETE from
    // wiping the just-added rows.
    pub fn prune_all(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<PruneAllResult, StoreError> {
        let _span = tracing::info_span!("prune_all", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            // Take the write lock first so the distinct-origin scan happens
            // against the same snapshot the DELETEs will operate on.
            let (_guard, mut tx) = self.begin_write().await?;

            // Identify missing origins via the tx's read snapshot. Any
            // concurrent watch reindex that committed *before* our
            // begin_write is reflected here; any reindex that committed
            // *after* will be queued behind our write lock. Either way the
            // missing-set lines up with what's actually deletable.
            // UNION (not UNION ALL) dedupes across the two sources. The
            // `file_registry` arm is load-bearing for v29 #1774: a zero-chunk
            // origin has NO chunk row, so without it a deleted comment-only
            // file would never enter the missing-set and its registry row
            // would leak forever — a permanent phantom in `list_stale_files`'
            // missing list that no GC pass could ever collect.
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT origin FROM chunks WHERE source_type = 'file' \
                 UNION \
                 SELECT origin FROM file_registry",
            )
            .fetch_all(&mut *tx)
            .await?;

            // Same filesystem-existence reconciliation as `prune_missing`.
            // Pre-compute the slash-normalized string set once so the
            // per-origin check hits the cheap string path.
            let existing_normalized = build_normalized_set(existing_files);
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| {
                    !origin_exists(origin, existing_files, Some(&existing_normalized), root)
                })
                .map(|(origin,)| origin)
                .collect();

            // 2a+2b. Delete FTS + chunks + per-file function_calls for the
            // missing origins, then sweep orphan sparse_vectors — shared with
            // prune_missing / prune_gitignored.
            //
            // Capture the pre-delete count of function_calls rows attached to
            // the missing files so the reported `pruned_calls` reflects the
            // call-graph rows this prune removed. (The shared helper deletes
            // them per batch; counting up front keeps the metric without a
            // second pass.)
            let pruned_calls = if missing.is_empty() {
                0u64
            } else {
                let mut total = 0u64;
                const COUNT_BATCH: usize = max_rows_per_statement(1);
                for batch in missing.chunks(COUNT_BATCH) {
                    let placeholders = crate::store::helpers::make_placeholders(batch.len());
                    let sql = format!(
                        "SELECT COUNT(*) FROM function_calls WHERE file IN ({})",
                        placeholders
                    );
                    let mut q = sqlx::query_as::<_, (i64,)>(sqlx::AssertSqlSafe(sql.as_str()));
                    for origin in batch {
                        q = q.bind(origin);
                    }
                    let (n,): (i64,) = q.fetch_one(&mut *tx).await?;
                    total += n as u64;
                }
                total
            };

            let pruned_chunks = if missing.is_empty() {
                0u32
            } else {
                delete_origins_in_tx(&mut tx, &missing, "prune_all").await?
            };

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

            // Orphan sparse_vectors are swept inside `delete_origins_in_tx`
            // above (same transaction), so no separate sweep is needed here.

            tx.commit().await?;

            if pruned_chunks > 0 {
                tracing::info!(
                    pruned_chunks,
                    files = missing.len(),
                    "Pruned chunks for missing files"
                );
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
    /// matcher. Used by the daemon's startup GC pass to clean up rows for
    /// paths a `.gitignore` change has since started ignoring.
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
            // Acquire the write transaction BEFORE reading origins. A
            // concurrent `cqs watch` upsert landing between the SELECT and
            // the DELETE creates a chunk for a path the matcher will flag as
            // ignored; the stale origin list would then point DELETE at a
            // just-inserted row. The matcher walk below is pure CPU over the
            // already-fetched `rows` Vec (microseconds on ~10k origins) and
            // is safe to hold the write lock across — single writer, no
            // re-entry.
            let (_guard, mut tx) = self.begin_write().await?;

            // Collect distinct origins via the tx's read snapshot so reads
            // and deletes serialise as one unit.
            // UNION (not UNION ALL) dedupes across the two sources. The
            // `file_registry` arm is load-bearing for v29 #1774: a zero-chunk
            // origin has NO chunk row, so without it a deleted comment-only
            // file would never enter the missing-set and its registry row
            // would leak forever — a permanent phantom in `list_stale_files`'
            // missing list that no GC pass could ever collect.
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT origin FROM chunks WHERE source_type = 'file' \
                 UNION \
                 SELECT origin FROM file_registry",
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

            // Delete FTS + chunks + per-file function_calls and sweep orphan
            // sparse_vectors — shared with prune_missing / prune_all. Before
            // this routed through the shared helper, prune_gitignored skipped
            // the function_calls DELETE entirely, leaving orphan call-graph
            // rows (ghost callers) for gitignore-driven prunes.
            let deleted = delete_origins_in_tx(&mut tx, &ignored, "prune_gitignored").await?;

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
    /// Count files that are stale (fingerprint diverged) or missing from disk.
    /// Compares the stored fingerprint against current filesystem state.
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

    /// List files that are stale (fingerprint diverged) or missing from disk.
    /// Like `count_stale_files()` but returns full details for display.
    /// Requires `existing_files` from `enumerate_files()` (~100ms for 10k files).
    ///
    /// Staleness is divergence in *either direction*: a file restored with a
    /// preserved older timestamp (`rsync -t`, `tar -x`, robocopy, backup
    /// restores) is just as out-of-date as one edited after indexing — the
    /// watch-loop reconcile already queues those, and this report must agree
    /// with it. Rows whose fingerprint columns (`source_size`,
    /// `source_content_hash`) are populated get the content-hash tiebreak
    /// from [`FingerprintPolicy::MtimeOrHash`], so an mtime-only flip with
    /// identical bytes (`git checkout`, formatter no-op) is not a false
    /// positive; rows without them degrade to mtime inequality.
    pub fn list_stale_files(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<StaleReport, StoreError> {
        let _span = tracing::debug_span!("list_stale_files").entered();
        self.rt.block_on(async {
            // GROUP BY origin (one row per file). `MAX(...)` deterministically
            // picks the newer fingerprint when an in-flight reindex briefly
            // holds two rows for the same file — same dedup semantics as
            // `indexed_file_origins`.
            // v29 #1774: UNION `file_registry` so zero-chunk origins are
            // included in the staleness report — a comment-only file whose disk
            // bytes changed should surface as stale even though it has no chunk
            // rows. Same `MAX` GROUP BY collapse as `indexed_file_origins`.
            type FpRow = (String, Option<i64>, Option<i64>, Option<Vec<u8>>);
            let rows: Vec<FpRow> = sqlx::query_as(
                "SELECT origin, \
                        MAX(source_mtime) AS mtime, \
                        MAX(source_size) AS size, \
                        MAX(source_content_hash) AS content_hash \
                 FROM ( \
                     SELECT origin, source_mtime, source_size, source_content_hash \
                     FROM chunks WHERE source_type = 'file' \
                     UNION ALL \
                     SELECT origin, source_mtime, source_size, source_content_hash \
                     FROM file_registry \
                 ) \
                 GROUP BY origin",
            )
            .fetch_all(&self.pool)
            .await?;

            let total_indexed = rows.len() as u64;
            let mut stale = Vec::new();
            let mut missing = Vec::new();

            for (origin, stored_mtime, stored_size, stored_hash) in rows {
                let path = PathBuf::from(&origin);

                // Filesystem existence check — same logic as prune_*.
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

                let stored_fp = FileFingerprint {
                    mtime: stored_mtime,
                    size: stored_size.and_then(|s| u64::try_from(s).ok()),
                    content_hash: stored_hash
                        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok()),
                };

                // Resolve the path against `root` for metadata lookup so
                // relative origins work regardless of current directory.
                let lookup_path: PathBuf = if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(&path)
                };
                // `read_disk` returns `None` when `metadata()` fails
                // (permission-denied, busy-file). Surface that as stale with
                // a sentinel `current_mtime = -1` so an operator can spot the
                // unreadable origin in `cqs stats --json` rather than having
                // it silently treated as fresh.
                let disk = match FileFingerprint::read_disk(
                    &lookup_path,
                    &stored_fp,
                    FingerprintPolicy::MtimeOrHash,
                ) {
                    Some(d) => d,
                    None => {
                        tracing::warn!(
                            path = %lookup_path.display(),
                            "Failed to read metadata for indexed file — treating as stale"
                        );
                        stale.push(StaleFile {
                            file: path,
                            stored_mtime: stored,
                            current_mtime: -1,
                        });
                        continue;
                    }
                };

                if !stored_fp.matches(&disk, FingerprintPolicy::MtimeOrHash) {
                    stale.push(StaleFile {
                        file: path,
                        stored_mtime: stored,
                        current_mtime: disk.mtime.unwrap_or(-1),
                    });
                }
            }

            Ok(StaleReport {
                stale,
                missing,
                total_indexed,
            })
        })
    }

    /// List every indexed source-file origin paired with its stored
    /// `source_mtime`. Returned as a map keyed by origin string so a
    /// watch-loop reconciler can:
    ///   1. Walk the disk → set of current files
    ///   2. Look up each disk file in this map
    ///   3. Queue for reindex when missing (added) OR the disk fingerprint
    ///      diverges from the stored one (modified — in either direction).
    ///      Files in this map but not on disk are handled by
    ///      `prune_missing` in the existing GC pass.
    ///
    /// `source_mtime` may be `None`. Treat those as needing reindex — same
    /// posture as `list_stale_files`, which surfaces them as
    /// stale-with-mtime=0.
    ///
    /// One SELECT, returns ~one row per source file. On a 17k-chunk corpus
    /// with ~3k unique source files this is sub-50 ms. Filter
    /// `source_type='file'` is critical — notes and other sources have
    /// their own mtime semantics.
    pub fn indexed_file_origins(&self) -> Result<HashMap<String, FileFingerprint>, StoreError> {
        let _span = tracing::debug_span!("indexed_file_origins").entered();
        self.rt.block_on(async {
            // GROUP BY origin (one row per file). `MAX(source_mtime)`
            // deterministically picks the newer fingerprint when an
            // in-flight reindex briefly holds two mtimes for the same file;
            // ties dedupe to one row per origin.
            //
            // The fingerprint columns (`source_size`, `source_content_hash`)
            // are nullable. `MAX` over a NULL column yields NULL, which
            // `read_disk` and `matches` treat as "no information, assume
            // divergent" — first re-embed populates them and subsequent
            // walks short-circuit on size match.
            // Tuple shape: (origin, mtime, size, content_hash) — all nullable
            // except origin.
            //
            // v29 #1774: UNION the `file_registry` table so origins that
            // parse to ZERO chunks (no chunk row to carry a fingerprint) still
            // surface a stored fingerprint and are classified MODIFIED-vs-
            // unchanged instead of ADDED. A file WITH chunks contributes from
            // both sources with identical fingerprints (the stamp writes both),
            // so the `MAX` GROUP BY collapses to one row either way.
            type FpRow = (String, Option<i64>, Option<i64>, Option<Vec<u8>>);
            let rows: Vec<FpRow> = sqlx::query_as(
                "SELECT origin, \
                        MAX(source_mtime) AS mtime, \
                        MAX(source_size) AS size, \
                        MAX(source_content_hash) AS content_hash \
                 FROM ( \
                     SELECT origin, source_mtime, source_size, source_content_hash \
                     FROM chunks WHERE source_type = 'file' \
                     UNION ALL \
                     SELECT origin, source_mtime, source_size, source_content_hash \
                     FROM file_registry \
                 ) \
                 GROUP BY origin",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|(origin, mtime, size, hash)| {
                    let content_hash = hash.and_then(|bytes| {
                        // Defensive: any row with a non-32-byte BLOB drops to
                        // None rather than truncating silently. Should never
                        // happen — we only ever insert 32-byte BLAKE3 — but
                        // the schema declares BLOB so be strict.
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

    /// Batched per-origin fingerprint lookup. Mirror of
    /// [`indexed_file_origins`] but bounded by the supplied `origins`
    /// list rather than the full file table — lets `run_daemon_reconcile`
    /// stream a 1M-file walk in 1k-row batches without materializing the
    /// whole-tree origin map (the eager `Vec<PathBuf>` + `HashMap<String,
    /// FileFingerprint>` is ~12 MB transient on a 100k-file repo, scaling
    /// linearly to ~120 MB on 1M files).
    ///
    /// Same `MAX(...)` deduplication semantics as `indexed_file_origins`:
    /// if a row briefly carries two mtimes for the same origin during an
    /// in-flight reindex, `MAX` picks the newer one and dedupes to one
    /// entry per origin.
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
                // Two placeholder runs of the same `batch` length — one binds
                // the chunks subquery's `origin IN (...)`, the other the
                // registry subquery's. Both are bound in order below.
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                // v29 #1774: UNION `file_registry` so zero-chunk origins
                // surface their persisted fingerprint here too (the reconcile
                // pre-filter's primary lookup). Same `MAX` GROUP BY collapse as
                // `indexed_file_origins`.
                let sql = format!(
                    "SELECT origin, \
                            MAX(source_mtime) AS mtime, \
                            MAX(source_size) AS size, \
                            MAX(source_content_hash) AS content_hash \
                     FROM ( \
                         SELECT origin, source_mtime, source_size, source_content_hash \
                         FROM chunks WHERE source_type = 'file' AND origin IN ({}) \
                         UNION ALL \
                         SELECT origin, source_mtime, source_size, source_content_hash \
                         FROM file_registry WHERE origin IN ({}) \
                     ) \
                     GROUP BY origin",
                    placeholders, placeholders
                );
                let mut query = sqlx::query_as::<_, FpRow>(sqlx::AssertSqlSafe(sql.as_str()));
                for origin in batch {
                    query = query.bind(*origin);
                }
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

    /// Report which of `origins` have at least one file chunk stamped with a
    /// `parser_version` other than `current` — i.e. the chunks were extracted
    /// by an older parser and need re-extraction even though their disk
    /// fingerprint is unchanged.
    ///
    /// The staleness pre-filters (`filter_stale_files` and the watch reconcile
    /// `process_batch`) select by fingerprint only, so a `PARSER_VERSION` bump
    /// would otherwise heal nothing without `--force`: an unchanged file keeps
    /// its stale chunks (and stale derived data — call edges, edge_kind, doc
    /// enrichment) until its bytes change. Treating version drift as stale
    /// closes that hole — a reindex re-parses drifted files and the UPSERT
    /// rewrites the rows (`OR parser_version != excluded.parser_version`).
    ///
    /// Only `source_type = 'file'` chunks are considered (notes/registry rows
    /// carry no parser stamp). An origin absent from the result either has no
    /// indexed chunks or all of them already match `current`.
    ///
    /// Drift loop-breaker (v31): an origin whose `file_registry`
    /// `parse_failed_parser_version` equals `current` is EXCLUDED even when its
    /// chunks are version-drifted. Such a file already failed to parse at the
    /// current parser version, so re-queuing it every tick re-runs a parse that
    /// will fail again — an unbounded loop a `PARSER_VERSION` bump re-arms. The
    /// marker is cleared by any successful re-parse (`set_fingerprint_in_tx`
    /// writes NULL), so a content edit that fixes the file lets it re-queue and
    /// heal normally.
    pub fn origins_with_parser_drift(
        &self,
        origins: &[&str],
        current: u32,
    ) -> Result<HashSet<String>, StoreError> {
        let _span =
            tracing::debug_span!("origins_with_parser_drift", count = origins.len()).entered();
        if origins.is_empty() {
            return Ok(HashSet::new());
        }
        self.rt.block_on(async {
            const BATCH_SIZE: usize = max_rows_per_statement(1);
            let mut drifted: HashSet<String> = HashSet::new();
            for batch in origins.chunks(BATCH_SIZE) {
                // `make_placeholders` emits NUMBERED placeholders (`?1, ?2, …`),
                // so `current` takes `?1` and the origins start at `?2`. Mixing
                // a bare `?` with numbered ones mis-binds under SQLite.
                let placeholders = crate::store::helpers::make_placeholders_offset(batch.len(), 2);
                // The NOT EXISTS clause is the v31 drift loop-breaker: skip an
                // origin already marked as having failed to parse at `?1` (the
                // current parser version). `?1` binds both the drift comparison
                // and the marker comparison, so the suppression tracks the same
                // version the drift predicate keys on.
                let sql = format!(
                    "SELECT DISTINCT c.origin FROM chunks c \
                     WHERE c.source_type = 'file' AND c.parser_version != ?1 \
                     AND c.origin IN ({placeholders}) \
                     AND NOT EXISTS ( \
                         SELECT 1 FROM file_registry fr \
                         WHERE fr.origin = c.origin \
                           AND fr.parse_failed_parser_version = ?1 \
                     )"
                );
                let mut query = sqlx::query_as::<_, (String,)>(sqlx::AssertSqlSafe(sql.as_str()));
                query = query.bind(i64::from(current));
                for origin in batch {
                    query = query.bind(*origin);
                }
                for (origin,) in query.fetch_all(&self.pool).await? {
                    drifted.insert(origin);
                }
            }
            Ok(drifted)
        })
    }

    /// Check if specific origins are stale (fingerprint diverged on disk).
    /// Lightweight per-query check: only examines the given origins, not the
    /// entire index. O(result_count), not O(index_size).
    /// `root` is the project root — origins are relative paths joined against it.
    /// Returns the set of stale origin paths.
    ///
    /// Same divergence semantics as [`Self::list_stale_files`]: stale means
    /// the disk fingerprint differs from the stored one in *either*
    /// direction (a rewound mtime from `rsync -t` / `tar -x` / backup
    /// restore counts), with the [`FingerprintPolicy::MtimeOrHash`]
    /// content-hash tiebreak suppressing mtime-only flips when the
    /// fingerprint columns are populated.
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
            type FpRow = (String, Option<i64>, Option<i64>, Option<Vec<u8>>);
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                // `MAX(...)` dedup: same semantics as `indexed_file_origins`
                // when an in-flight reindex briefly holds two rows per origin.
                // v29 #1774: UNION `file_registry` so a zero-chunk origin
                // passed to the per-query staleness check resolves against its
                // persisted fingerprint instead of being absent (which would
                // silently skip the staleness warning). Two `origin IN (...)`
                // placeholder runs, bound in order below.
                let sql = format!(
                    "SELECT origin, \
                            MAX(source_mtime) AS mtime, \
                            MAX(source_size) AS size, \
                            MAX(source_content_hash) AS content_hash \
                     FROM ( \
                         SELECT origin, source_mtime, source_size, source_content_hash \
                         FROM chunks WHERE origin IN ({}) \
                         UNION ALL \
                         SELECT origin, source_mtime, source_size, source_content_hash \
                         FROM file_registry WHERE origin IN ({}) \
                     ) \
                     GROUP BY origin",
                    placeholders, placeholders
                );

                let mut query = sqlx::query_as::<_, FpRow>(sqlx::AssertSqlSafe(sql.as_str()));
                for origin in batch {
                    query = query.bind(*origin);
                }
                for origin in batch {
                    query = query.bind(*origin);
                }
                let rows = query.fetch_all(&self.pool).await?;

                for (origin, stored_mtime, stored_size, stored_hash) in rows {
                    if stored_mtime.is_none() {
                        stale.insert(origin);
                        continue;
                    }

                    // Origins in DB always use forward slashes (via normalize_path).
                    debug_assert!(
                        !origin.contains('\\'),
                        "DB origin contains backslash: {origin}"
                    );
                    let stored_fp = FileFingerprint {
                        mtime: stored_mtime,
                        size: stored_size.and_then(|s| u64::try_from(s).ok()),
                        content_hash: stored_hash
                            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok()),
                    };
                    // Normalize the joined path to handle OS-native root with
                    // forward-slash origin (e.g., `C:\proj` + `src/lib.rs`).
                    let path = PathBuf::from(crate::normalize_path(&root.join(&origin)));
                    match FileFingerprint::read_disk(
                        &path,
                        &stored_fp,
                        FingerprintPolicy::MtimeOrHash,
                    ) {
                        Some(disk) => {
                            if !stored_fp.matches(&disk, FingerprintPolicy::MtimeOrHash) {
                                stale.insert(origin);
                            }
                        }
                        None => {
                            // File deleted or inaccessible — treat as stale
                            stale.insert(origin);
                        }
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
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
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
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
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
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
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
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
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
            byte_start: 0,
            content_hash: "fresh".to_string(),
            canonical_hash: String::new(),
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
            byte_start: 0,
            content_hash: "stale".to_string(),
            canonical_hash: String::new(),
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

    /// Rewound-mtime case for the per-query check: disk mtime older than
    /// stored must surface as stale, mirroring both `list_stale_files` and
    /// the reconcile pin (`run_daemon_reconcile_queues_older_disk_mtime`).
    /// Otherwise the per-query staleness warning stays silent for files
    /// restored with preserved timestamps while the daemon queues them.
    #[test]
    fn test_check_origins_stale_rewound_disk_mtime_is_stale() {
        let (store, dir) = setup_store();

        let file_path = dir.path().join("src/rewound.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn rewound() {}").unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "rewound".to_string(),
            signature: "fn rewound()".to_string(),
            content: "fn rewound() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Stored mtime sits well above the disk mtime — the disk file was
        // "rewound" by a timestamp-preserving restore.
        let disk_mtime = crate::duration_to_mtime_millis(
            file_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );

        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(disk_mtime + 10_000_000))
            .unwrap();

        let stale = store.check_origins_stale(&[&origin], dir.path()).unwrap();
        assert!(
            stale.contains(&origin),
            "Disk mtime older than stored must be stale in the per-query check, got {stale:?}"
        );
    }

    /// Empty index returns empty origin map.
    #[test]
    fn test_indexed_file_origins_empty_store() {
        let (store, _dir) = setup_store();
        let map = store.indexed_file_origins().expect("indexed_file_origins");
        assert!(map.is_empty());
    }

    /// Inserted file chunks surface as origin → mtime entries. Pinned mtime
    /// so the test isn't sensitive to clock.
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

    /// Rows without `source_type='file'` are filtered out. The store schema
    /// only emits `source_type='file'` for indexed code chunks, but the
    /// explicit WHERE clause guards against test fixtures or synthetic
    /// origins leaking into the reconcile loop.
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

    // ===== v29 file_registry #1774 tests =====

    /// A zero-chunk origin whose fingerprint lives ONLY in `file_registry`
    /// (no chunk rows) must surface from `indexed_file_origins` and
    /// `fingerprints_for_origins` — the UNION wiring that lets the staleness
    /// pre-filter skip re-parsing comment-only files every run.
    #[test]
    fn test_file_registry_origin_surfaces_in_readers() {
        use crate::store::FileFingerprint;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();

        let fp = FileFingerprint {
            mtime: Some(5000),
            size: Some(17),
            content_hash: Some([7u8; 32]),
        };
        let stamped = store
            .set_file_registry_fingerprints_batch(&[(PathBuf::from("src/empty.rs"), fp.clone())])
            .expect("registry stamp");
        assert_eq!(stamped, 1);

        // indexed_file_origins (full-tree map) includes the registry origin
        // even though no chunk row exists for it.
        let map = store.indexed_file_origins().expect("indexed_file_origins");
        let got = map
            .get("src/empty.rs")
            .expect("registry-only origin must appear in indexed_file_origins");
        assert_eq!(got.mtime, Some(5000));
        assert_eq!(got.size, Some(17));
        assert_eq!(got.content_hash, Some([7u8; 32]));

        // fingerprints_for_origins (batched, origin-scoped) returns it too.
        let batched = store
            .fingerprints_for_origins(&["src/empty.rs"])
            .expect("fingerprints_for_origins");
        assert_eq!(
            batched.get("src/empty.rs").and_then(|f| f.mtime),
            Some(5000),
            "registry-only origin must resolve in the batched reconcile lookup"
        );
    }

    /// `delete_by_origin` must prune the `file_registry` row alongside the
    /// chunks so a deleted-then-recreated file isn't matched against a stale
    /// persisted fingerprint.
    #[test]
    fn test_delete_by_origin_prunes_file_registry() {
        use crate::store::FileFingerprint;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();

        let fp = FileFingerprint {
            mtime: Some(9000),
            size: Some(3),
            content_hash: Some([1u8; 32]),
        };
        store
            .set_file_registry_fingerprints_batch(&[(PathBuf::from("src/gone.rs"), fp)])
            .unwrap();
        assert!(
            store
                .indexed_file_origins()
                .unwrap()
                .contains_key("src/gone.rs"),
            "precondition: registry origin present"
        );

        store
            .delete_by_origin(&PathBuf::from("src/gone.rs"))
            .unwrap();

        assert!(
            !store
                .indexed_file_origins()
                .unwrap()
                .contains_key("src/gone.rs"),
            "delete_by_origin must prune the file_registry row"
        );
    }

    /// `prune_missing` (via `delete_origins_in_tx`) must remove the
    /// `file_registry` row for an origin no longer on disk — otherwise the
    /// persisted zero-chunk fingerprint outlives the file.
    #[test]
    fn test_prune_missing_prunes_file_registry() {
        use crate::store::FileFingerprint;
        use std::path::PathBuf;
        let (store, dir) = setup_store();

        // Registry origin for a file that does NOT exist on disk.
        let fp = FileFingerprint {
            mtime: Some(1234),
            size: Some(8),
            content_hash: Some([2u8; 32]),
        };
        store
            .set_file_registry_fingerprints_batch(&[(PathBuf::from("src/missing_empty.rs"), fp)])
            .unwrap();

        // Deliberately NO chunk row for this origin: the registry-only case is
        // the population #1774 exists for, and the prune's origin enumeration
        // must UNION `file_registry` to see it at all. (A chunk-bearing origin
        // would mask an enumeration that only walks `chunks`.)

        // existing_files is empty → the origin is "missing" → pruned.
        let existing = HashSet::new();
        store.prune_missing(&existing, dir.path()).unwrap();

        assert!(
            !store
                .indexed_file_origins()
                .unwrap()
                .contains_key("src/missing_empty.rs"),
            "prune_missing must remove the file_registry row for a registry-only missing origin"
        );
    }

    /// `prune_all` must also collect a registry-only missing origin — the
    /// daemon GC pass uses prune_all, and the inotify delete event (the only
    /// other cleanup) is unreliable on WSL mounts, so GC is the backstop.
    #[test]
    fn test_prune_all_prunes_registry_only_origin() {
        use crate::store::FileFingerprint;
        use std::path::PathBuf;
        let (store, dir) = setup_store();

        let fp = FileFingerprint {
            mtime: Some(99),
            size: Some(5),
            content_hash: Some([3u8; 32]),
        };
        store
            .set_file_registry_fingerprints_batch(&[(PathBuf::from("src/ghost_empty.rs"), fp)])
            .unwrap();

        let existing = HashSet::new();
        store.prune_all(&existing, dir.path()).unwrap();

        assert!(
            !store
                .indexed_file_origins()
                .unwrap()
                .contains_key("src/ghost_empty.rs"),
            "prune_all must remove a registry-only missing origin"
        );
    }

    // ===== prune_all tests =====

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
            byte_start: 0,
            content_hash: hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// Happy path: `prune_all` removes chunks for files deleted from disk,
    /// counts reflect the prune, remaining chunks intact.
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

    /// A nested origin like `cuvs-fork-push/CHANGELOG.md` whose directory
    /// does not exist on disk must be pruned, even though its tail matches
    /// the root file `CHANGELOG.md`. The filesystem existence check, not a
    /// suffix match, decides ownership.
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

    /// `.claude/worktrees/agent-X/src/foo.rs` must be pruned when only
    /// `src/foo.rs` is on disk, even though the worktree tail matches the
    /// real path.
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

    /// `enumerate_files` skips nested git worktrees (the
    /// directory-with-`.git`-as-file filter), so worktree-prefixed origins
    /// do NOT appear in `existing_files`. The worktree files DO exist on
    /// disk while the agent runs, so a `Path::exists()` fallback would
    /// retain `.claude/worktrees/agent-X/...` chunks and pollute search
    /// results. `origin_exists` canonicalizes both sides and requires
    /// strict membership in `existing_files` instead.
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

    /// The happy-path test above asserts
    /// `pruned_calls/type_edges/summaries == 0` because nothing was inserted
    /// into those tables. This test populates each of the four cascade tables
    /// and verifies that deleting the source file propagates through every
    /// counter. A refactor that short-circuits any of steps 2b / 2c / 2d
    /// would survive the happy-path test — this one catches it.
    #[test]
    fn test_prune_all_cascade_populates_all_counters() {
        use crate::parser::{CallEdgeKind, CallSite, FunctionCalls, TypeRef};

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
                            kind: CallEdgeKind::Call,
                        },
                        CallSite {
                            callee_name: "helper_b".to_string(),
                            line_number: 3,
                            kind: CallEdgeKind::Call,
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

    /// Baseline "nothing to prune". When every file still exists on disk,
    /// prune_all must return an all-zero `PruneAllResult`.
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

    // ===== mtime semantics tests =====
    //
    // The staleness predicate in `list_stale_files` is fingerprint
    // *divergence* (`FileFingerprint::matches` under `MtimeOrHash`), the
    // same rule the watch-loop reconcile applies — see
    // `run_daemon_reconcile_queues_older_disk_mtime`. Three tests pin the
    // boundary behaviour:
    //   - Equal mtime: fresh (not stale).
    //   - Disk mtime older than stored (rewound by rsync -t / tar / backup
    //     restore): stale — a strict `current > stored` predicate would
    //     silently skip these while reconcile queues them.
    //   - Rewound mtime but identical content with fingerprint columns
    //     populated: fresh — the content-hash tiebreak suppresses the
    //     mtime-only flip, so backup restores of unchanged files don't
    //     trigger a full re-embed.

    /// Equal mtime must be treated as fresh. Tests the boundary of the
    /// divergence predicate — a refactor to "always stale when columns are
    /// NULL" would flip this case and report the file as stale.
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
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
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

    /// Rewound-mtime case: the file on disk carries an mtime *older* than
    /// the stored one. Files restored with preserved timestamps (`rsync -t`,
    /// `tar -x`, robocopy, backup restores) land in this state with
    /// arbitrary content. The watch-loop reconcile already queues these
    /// (see `run_daemon_reconcile_queues_older_disk_mtime`); the staleness
    /// report must agree, otherwise `cqs stats` / `cqs index --stale` claim
    /// fresh for a file the index no longer reflects. With NULL fingerprint
    /// columns the predicate degrades to mtime inequality, which catches
    /// the backward divergence a strict greater-than comparison missed.
    #[test]
    fn test_list_stale_files_rewound_disk_mtime_is_stale() {
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
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        // Store with an mtime 10_000_000 ms (~2.7 hours) in the future
        // relative to the file on disk — equivalent to the disk file being
        // rewound below the indexed mtime.
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
        assert_eq!(
            report.stale.len(),
            1,
            "Disk mtime older than stored (rewound restore) must be reported as stale, got {:?}",
            report.stale
        );
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    /// Rewound mtime with *identical content* must stay fresh when the
    /// fingerprint columns are populated: `FingerprintPolicy::MtimeOrHash`
    /// falls through to the content-hash tiebreak on mtime mismatch, so a
    /// timestamp-only flip (`git checkout`, formatter no-op, `touch`) does
    /// not become a false positive under the divergence predicate.
    #[test]
    fn test_list_stale_files_rewound_mtime_same_content_is_fresh() {
        let (store, dir) = setup_store();

        let file_path = dir.path().join("src/flip.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        let content = "fn flip() {}";
        std::fs::write(&file_path, content).unwrap();

        let origin = file_path.to_string_lossy().to_string();
        let c = Chunk {
            id: format!("{}:1:abc", origin),
            file: file_path.clone(),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "flip".to_string(),
            signature: "fn flip()".to_string(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "abc".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };

        let disk_mtime = crate::duration_to_mtime_millis(
            file_path
                .metadata()
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap(),
        );
        let diverged_mtime = disk_mtime + 10_000_000;

        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(diverged_mtime))
            .unwrap();

        // Stamp the full fingerprint: mtime diverges from disk, but size +
        // BLAKE3 hash match the bytes on disk exactly.
        let fp = super::FileFingerprint {
            mtime: Some(diverged_mtime),
            size: Some(content.len() as u64),
            content_hash: Some(*blake3::hash(content.as_bytes()).as_bytes()),
        };
        store.set_file_fingerprint(&file_path, &fp).unwrap();

        let mut existing = HashSet::new();
        existing.insert(file_path);
        let report = store.list_stale_files(&existing, dir.path()).unwrap();
        assert!(
            report.stale.is_empty(),
            "mtime-only flip with identical content must not be stale, got {:?}",
            report.stale
        );
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    // ===== Daemon startup GC tests =====
    //
    // These four tests pin the contract for the two prune passes the
    // `cqs watch --serve` startup hook calls:
    //
    //   1. `prune_missing` — drop chunks whose origin no longer exists on
    //      disk (e.g. file deleted while the daemon was down).
    //   2. `prune_gitignored` — drop chunks whose path is now matched by
    //      `.gitignore` (cleanup of rows for paths a `.gitignore` change
    //      has since started ignoring).
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
            byte_start: 0,
            content_hash: "keep".to_string(),
            canonical_hash: String::new(),
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
                byte_start: 0,
                content_hash: hash.to_string(),
                canonical_hash: String::new(),
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
    /// is `target/cache.rs` must be pruned. Models the canonical ignored-path
    /// case (worktrees, build artifacts, etc.).
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
            byte_start: 0,
            content_hash: "lib".to_string(),
            canonical_hash: String::new(),
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

    /// `prune_gitignored` must remove the `function_calls` rows for the
    /// pruned origins, not just the chunks. Before the shared
    /// `delete_origins_in_tx` helper, this path skipped the function_calls
    /// DELETE entirely, leaving orphan call-graph rows (ghost callers) for a
    /// file the indexer no longer owns until the next full `prune_all`.
    #[test]
    fn test_prune_gitignored_removes_function_calls() {
        use crate::parser::{CallEdgeKind, CallSite, FunctionCalls};

        let (store, dir) = setup_store();

        // Chunk + call edges for a file that `.gitignore` will start ignoring.
        let ignored_chunk = make_chunk("cache_helper", "target/cache.rs");
        store
            .upsert_chunks_batch(&[(ignored_chunk, mock_embedding(1.0))], Some(1000))
            .unwrap();

        // Seed a call edge: target/cache.rs's `cache_helper` calls
        // `ghost_callee`. The `file` recorded matches the chunk origin
        // (`target/cache.rs`) so the prune's per-file DELETE can match it.
        store
            .upsert_function_calls(
                std::path::Path::new("target/cache.rs"),
                &[FunctionCalls {
                    name: "cache_helper".to_string(),
                    line_start: 1,
                    calls: vec![CallSite {
                        callee_name: "ghost_callee".to_string(),
                        line_number: 2,
                        kind: CallEdgeKind::Call,
                    }],
                }],
            )
            .unwrap();

        // Sanity: the caller exists before the prune.
        let before = store.get_callers_full("ghost_callee").unwrap();
        assert_eq!(before.len(), 1, "edge must exist before prune");

        let matcher = matcher_from_lines(dir.path(), &["target/"]);
        let pruned = store.prune_gitignored(&matcher, dir.path(), None).unwrap();
        assert_eq!(pruned, 1, "target/cache.rs chunk must be pruned");

        // The call-graph row must be gone too — no ghost callers survive a
        // gitignore-driven prune.
        let after = store.get_callers_full("ghost_callee").unwrap();
        assert!(
            after.is_empty(),
            "function_calls rows for the gitignored file must be pruned, got {after:?}"
        );
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
            byte_start: 0,
            content_hash: "lib".to_string(),
            canonical_hash: String::new(),
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

    /// `FileFingerprint::read_disk` returns `None` when `metadata()` fails.
    /// The reconcile "leave to GC" branch is keyed on this contract: if
    /// `metadata()` errs
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
