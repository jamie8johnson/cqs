//! Staleness checks and pruning for missing/stale files.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::store::helpers::{StaleFile, StaleReport, StoreError};
use crate::store::Store;

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

impl Store {
    /// Delete chunks for files that no longer exist
    ///
    /// Batches deletes in groups of 100 to balance memory usage and query efficiency.
    ///
    /// Uses Rust HashSet for existence check rather than SQL WHERE NOT IN because:
    /// - Existing files often number 10k+, exceeding SQLite's parameter limit (~999)
    /// - Sending full file list to SQLite would require chunked queries anyway
    /// - HashSet lookup is O(1), and we already have the set from enumerate_files()
    pub fn prune_missing(&self, existing_files: &HashSet<PathBuf>) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("prune_missing", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            // Collect missing origins
            //
            // PB-24: On macOS (HFS+/APFS case-insensitive), PathBuf comparison
            // is case-sensitive but the filesystem is not. Normalize both sides
            // to lowercase so we don't falsely mark files as missing.
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| {
                    let origin_path = PathBuf::from(origin);
                    #[cfg(target_os = "macos")]
                    {
                        let origin_lower = origin.to_lowercase();
                        !existing_files
                            .iter()
                            .any(|p| p.to_string_lossy().to_lowercase() == origin_lower)
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        !existing_files.contains(&origin_path)
                    }
                })
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

            let mut tx = self.pool.begin().await?;

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

            tx.commit().await?;

            if deleted > 0 {
                tracing::info!(deleted, files = missing.len(), "Pruned chunks for missing files");
            }

            Ok(deleted)
        })
    }

    /// Run all prune operations in a single SQLite transaction.
    ///
    /// Ensures concurrent readers never see an inconsistent state where chunks
    /// are deleted but orphan call graph / type edge / summary entries remain.
    /// Without this, the window between `prune_missing` and `prune_stale_calls`
    /// exposes stale `function_calls` rows referencing deleted chunks.
    pub fn prune_all(
        &self,
        existing_files: &HashSet<PathBuf>,
    ) -> Result<PruneAllResult, StoreError> {
        let _span = tracing::info_span!("prune_all", existing = existing_files.len()).entered();
        self.rt.block_on(async {
            // Phase 1: identify missing origins (Rust-side HashSet check, outside tx)
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT origin FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| {
                    let origin_path = PathBuf::from(origin);
                    #[cfg(target_os = "macos")]
                    {
                        let origin_lower = origin.to_lowercase();
                        !existing_files
                            .iter()
                            .any(|p| p.to_string_lossy().to_lowercase() == origin_lower)
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        !existing_files.contains(&origin_path)
                    }
                })
                .map(|(origin,)| origin)
                .collect();

            // Phase 2: single transaction for ALL mutations
            let mut tx = self.pool.begin().await?;

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

    /// Count files that are stale (mtime changed) or missing from disk.
    ///
    /// Compares stored source_mtime against current filesystem state.
    /// Only checks files with source_type='file' (not notes or other sources).
    ///
    /// Returns `(stale_count, missing_count)`.
    pub fn count_stale_files(
        &self,
        existing_files: &HashSet<PathBuf>,
    ) -> Result<(u64, u64), StoreError> {
        let _span = tracing::debug_span!("count_stale_files").entered();
        let report = self.list_stale_files(existing_files)?;
        Ok((report.stale.len() as u64, report.missing.len() as u64))
    }

    /// List files that are stale (mtime changed) or missing from disk.
    ///
    /// Like `count_stale_files()` but returns full details for display.
    /// Requires `existing_files` from `enumerate_files()` (~100ms for 10k files).
    pub fn list_stale_files(
        &self,
        existing_files: &HashSet<PathBuf>,
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
                if !existing_files.contains(&path) {
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

                let current_mtime = path
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
    ///
    /// Lightweight per-query check: only examines the given origins, not the
    /// entire index. O(result_count), not O(index_size).
    ///
    /// `root` is the project root — origins are relative paths joined against it.
    ///
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

            // Batch in groups of 900 to stay under SQLite's 999-parameter limit
            const BATCH_SIZE: usize = 900;
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
        let (store, _dir) = setup_store();
        let existing = HashSet::new();
        let report = store.list_stale_files(&existing).unwrap();
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
        let report = store.list_stale_files(&existing).unwrap();
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
        let report = store.list_stale_files(&existing).unwrap();
        assert_eq!(report.stale.len(), 1);
        assert_eq!(report.stale[0].stored_mtime, 1000);
        assert!(report.stale[0].current_mtime > 1000);
        assert!(report.missing.is_empty());
        assert_eq!(report.total_indexed, 1);
    }

    #[test]
    fn test_list_stale_files_detects_missing() {
        let (store, _dir) = setup_store();

        let c = make_chunk("gone", "/nonexistent/file.rs");
        store
            .upsert_chunks_batch(&[(c, mock_embedding(1.0))], Some(1000))
            .unwrap();

        // existing_files doesn't contain the path
        let existing = HashSet::new();
        let report = store.list_stale_files(&existing).unwrap();
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
        let report = store.list_stale_files(&existing).unwrap();
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
}
