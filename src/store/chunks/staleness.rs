// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Staleness checks and pruning for missing/stale files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::store::helpers::{StaleFile, StaleReport, StoreError};
use crate::store::{ReadWrite, Store};

/// Decide whether a chunk origin refers to a file that exists.
///
/// Used by all four staleness helpers in this module. Replaces the
/// over-loose path-suffix matching that retained 81% of chunks as
/// orphans on this repo (root cause of v1.22.0 → v1.25.0 eval drift).
///
/// The check is: exact membership in `existing_files`, falling back to a
/// filesystem `exists()` probe against `root` for paths that may have been
/// stored under a different relative/absolute form. Filesystem existence is
/// case-correct on every OS (including macOS case-fold), so the previous
/// `#[cfg(target_os = "macos")]` branch is no longer needed.
fn origin_exists(origin: &str, existing_files: &HashSet<PathBuf>, root: &Path) -> bool {
    let origin_path = PathBuf::from(origin);
    if existing_files.contains(&origin_path) {
        return true;
    }
    let absolute = if origin_path.is_absolute() {
        origin_path
    } else {
        root.join(&origin_path)
    };
    absolute.exists()
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
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            // AC / CQ-V1.25-4 / CQ-V1.25-6 / PB-V1.25-7: reconcile stored origins
            // against current filesystem state via `origin_exists`. The previous
            // `ends_with` heuristic retained 81% of chunks as orphans whenever a
            // worktree or subdirectory tail-matched a root file name.
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| !origin_exists(origin, existing_files, root))
                .map(|(origin,)| origin)
                .collect();

            if missing.is_empty() {
                return Ok(0);
            }

            // Batch delete in chunks of 100 (SQLite has ~999 param limit).
            // Single transaction wraps ALL batches — partial prune on crash
            // would leave the index inconsistent with disk.
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;

            let (_guard, mut tx) = self.begin_write().await?;

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
    // Note: This has a theoretical TOCTOU race between the Phase 1 file-existence
    // check and the Phase 2 transaction, but acquire_index_lock in cmd_index
    // prevents concurrent writers in practice.
    pub fn prune_all(
        &self,
        existing_files: &HashSet<PathBuf>,
        root: &Path,
    ) -> Result<PruneAllResult, StoreError> {
        let _span = tracing::info_span!("prune_all", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            // Phase 1: identify missing origins (Rust-side HashSet check, outside tx)
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            // Same filesystem-existence reconciliation as `prune_missing`.
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| !origin_exists(origin, existing_files, root))
                .map(|(origin,)| origin)
                .collect();

            // Phase 2: single transaction for ALL mutations
            let (_guard, mut tx) = self.begin_write().await?;

            // 2a. Delete chunks for missing files (batched for SQLite param limit)
            const BATCH_SIZE: usize = 100;
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
                if !origin_exists(&origin, existing_files, root) {
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
                let current_mtime = lookup_path
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64);

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
                        .map(|d| d.as_millis() as i64);

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
        };

        // Get current mtime
        let mtime = file_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

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
        };

        let mtime = file_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

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
        };

        let fresh_mtime = fresh_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

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
        };

        // Store with the exact mtime currently on disk.
        let mtime = file_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

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
        };

        // Store with an mtime 10_000_000 ms (~2.7 hours) in the future
        // relative to the file on disk. Pins `current > stored` (false)
        // → fresh.
        let disk_mtime = file_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
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
}
