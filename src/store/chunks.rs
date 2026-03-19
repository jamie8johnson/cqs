//! Chunk CRUD operations

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::Row;

use super::helpers::{
    bytes_to_embedding, clamp_line_number, embedding_to_bytes, CandidateRow, ChunkIdentity,
    ChunkRow, ChunkSummary, IndexStats, StaleFile, StaleReport, StoreError,
};
use super::Store;
use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::{Chunk, ChunkType, Language};

impl Store {
    /// Retrieve a single metadata value by key.
    ///
    /// Returns `Ok(value)` if the key exists, or `Err` if not found or on DB error.
    /// Used for lightweight metadata checks (e.g., model compatibility between stores).
    pub fn get_metadata(&self, key: &str) -> Result<String, StoreError> {
        let _span = tracing::debug_span!("get_metadata", key = %key).entered();
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = ?1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await?;
            row.map(|(v,)| v)
                .ok_or_else(|| StoreError::NotFound(format!("metadata key '{}'", key)))
        })
    }

    /// Insert or update chunks in batch using multi-row INSERT.
    ///
    /// Chunks are inserted in batches of 52 rows (52 * 19 params = 988 < SQLite's 999 limit).
    /// FTS operations remain per-row because FTS5 doesn't support INSERT OR REPLACE.
    pub fn upsert_chunks_batch(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("upsert_chunks_batch", count = chunks.len()).entered();

        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb))
            .collect::<Result<Vec<_>, _>>()?;

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(&mut tx, chunks, &embedding_bytes, source_mtime, &now).await?;
            upsert_fts_conditional(&mut tx, chunks, &old_hashes).await?;
            tx.commit().await?;
            Ok(chunks.len())
        })
    }

    /// Insert or update a single chunk
    pub fn upsert_chunk(
        &self,
        chunk: &Chunk,
        embedding: &Embedding,
        source_mtime: Option<i64>,
    ) -> Result<(), StoreError> {
        let _span = tracing::info_span!("upsert_chunk", name = %chunk.name).entered();
        self.upsert_chunks_batch(&[(chunk.clone(), embedding.clone())], source_mtime)?;
        Ok(())
    }

    /// Update only the embedding for existing chunks by chunk ID.
    ///
    /// `updates` is a slice of `(chunk_id, embedding)` pairs. Chunk IDs not
    /// found in the store are logged and skipped (rows_affected == 0).
    /// Returns the count of actually updated rows.
    ///
    /// Update embeddings in batch (without changing enrichment hashes).
    ///
    /// Convenience wrapper around `update_embeddings_with_hashes_batch` that
    /// passes `None` for the enrichment hash, leaving it unchanged.
    pub fn update_embeddings_batch(
        &self,
        updates: &[(String, Embedding)],
    ) -> Result<usize, StoreError> {
        let with_none: Vec<(String, Embedding, Option<String>)> = updates
            .iter()
            .map(|(id, emb)| (id.clone(), emb.clone(), None))
            .collect();
        self.update_embeddings_with_hashes_batch(&with_none)
    }

    /// Update embeddings and optionally enrichment hashes in batch.
    ///
    /// When the hash is `Some`, stores the enrichment hash for idempotency
    /// detection. When `None`, leaves the existing enrichment hash unchanged.
    /// Used by the enrichment pass to record which call context was used,
    /// so re-indexing can skip unchanged chunks.
    pub fn update_embeddings_with_hashes_batch(
        &self,
        updates: &[(String, Embedding, Option<String>)],
    ) -> Result<usize, StoreError> {
        let _span =
            tracing::info_span!("update_embeddings_with_hashes_batch", count = updates.len())
                .entered();
        if updates.is_empty() {
            return Ok(0);
        }

        let embedding_bytes: Vec<Vec<u8>> = updates
            .iter()
            .map(|(_, emb, _)| embedding_to_bytes(emb))
            .collect::<Result<Vec<_>, _>>()?;

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            let mut updated = 0usize;
            for (i, (id, _, hash)) in updates.iter().enumerate() {
                let result =
                    match hash {
                        Some(h) => sqlx::query(
                            "UPDATE chunks SET embedding = ?1, enrichment_hash = ?2 WHERE id = ?3",
                        )
                        .bind(&embedding_bytes[i])
                        .bind(h)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?,
                        None => {
                            sqlx::query("UPDATE chunks SET embedding = ?1 WHERE id = ?2")
                                .bind(&embedding_bytes[i])
                                .bind(id)
                                .execute(&mut *tx)
                                .await?
                        }
                    };
                if result.rows_affected() > 0 {
                    updated += 1;
                } else {
                    tracing::debug!(chunk_id = %id, "Enrichment update found no row");
                }
            }
            tx.commit().await?;
            Ok(updated)
        })
    }

    /// Get enrichment hashes for a batch of chunk IDs.
    ///
    /// Returns a map from chunk_id to enrichment_hash (only for chunks that have one).
    pub fn get_enrichment_hashes_batch(
        &self,
        chunk_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        if chunk_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.rt.block_on(async {
            let mut result = std::collections::HashMap::new();
            // Process in batches to stay under SQLite parameter limit
            for batch in chunk_ids.chunks(500) {
                let placeholders: String = batch
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT id, enrichment_hash FROM chunks WHERE id IN ({}) AND enrichment_hash IS NOT NULL",
                    placeholders
                );
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for id in batch {
                    query = query.bind(*id);
                }
                let rows = query.fetch_all(&self.pool).await?;
                for (id, hash) in rows {
                    result.insert(id, hash);
                }
            }
            Ok(result)
        })
    }

    /// Get LLM summaries for a batch of content hashes.
    ///
    /// Returns a map from content_hash to summary text. Only includes hashes
    /// that have summaries in the llm_summaries table.
    pub fn get_summaries_by_hashes(
        &self,
        content_hashes: &[&str],
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        if content_hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.rt.block_on(async {
            let mut result = std::collections::HashMap::new();
            for batch in content_hashes.chunks(500) {
                let placeholders: String = batch
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT content_hash, summary FROM llm_summaries WHERE content_hash IN ({})",
                    placeholders
                );
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for hash in batch {
                    query = query.bind(*hash);
                }
                let rows = query.fetch_all(&self.pool).await?;
                for (hash, summary) in rows {
                    result.insert(hash, summary);
                }
            }
            Ok(result)
        })
    }

    /// Insert or update LLM summaries in batch.
    ///
    /// Each entry is (content_hash, summary, model).
    pub fn upsert_summaries_batch(
        &self,
        summaries: &[(String, String, String)],
    ) -> Result<usize, StoreError> {
        if summaries.is_empty() {
            return Ok(0);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            const BATCH_SIZE: usize = 166; // 166 * 4 params = 664 < 999
            for batch in summaries.chunks(BATCH_SIZE) {
                let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT OR REPLACE INTO llm_summaries (content_hash, summary, model, created_at)",
                );
                qb.push_values(batch.iter(), |mut b, (hash, summary, model)| {
                    b.push_bind(hash)
                        .push_bind(summary)
                        .push_bind(model)
                        .push_bind(&now);
                });
                qb.build().execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(summaries.len())
        })
    }

    /// Fetch all LLM summaries as a map from content_hash to summary text.
    ///
    /// Single query, no batching needed (reads entire table). Used by the
    /// enrichment pass to avoid per-page summary fetches.
    pub fn get_all_summaries(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT content_hash, summary FROM llm_summaries")
                    .fetch_all(&self.pool)
                    .await?;
            Ok(rows.into_iter().collect())
        })
    }

    /// Delete orphan LLM summaries whose content_hash doesn't exist in any chunk.
    pub fn prune_orphan_summaries(&self) -> Result<usize, StoreError> {
        self.rt.block_on(async {
            let result = sqlx::query(
                "DELETE FROM llm_summaries WHERE content_hash NOT IN \
                 (SELECT DISTINCT content_hash FROM chunks)",
            )
            .execute(&self.pool)
            .await?;
            Ok(result.rows_affected() as usize)
        })
    }

    /// Check if a file needs reindexing based on mtime.
    ///
    /// Returns `Ok(Some(mtime))` if reindex needed (with the file's current mtime),
    /// or `Ok(None)` if no reindex needed. This avoids reading file metadata twice.
    pub fn needs_reindex(&self, path: &Path) -> Result<Option<i64>, StoreError> {
        let _span = tracing::debug_span!("needs_reindex", path = %path.display()).entered();
        let current_mtime = path
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| StoreError::SystemTime)?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(Option<i64>,)> =
                sqlx::query_as("SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1")
                    .bind(crate::normalize_path(path))
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((Some(stored_mtime),)) if stored_mtime >= current_mtime => Ok(None),
                _ => Ok(Some(current_mtime)),
            }
        })
    }

    /// Delete all chunks for an origin (file path or source identifier)
    pub fn delete_by_origin(&self, origin: &Path) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("delete_by_origin", origin = %origin.display()).entered();
        let origin_str = crate::normalize_path(origin);

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            sqlx::query(
                "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin = ?1)",
            )
            .bind(&origin_str)
            .execute(&mut *tx)
            .await?;

            let result = sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(result.rows_affected() as u32)
        })
    }

    /// Atomically upsert chunks and their call graph in a single transaction.
    ///
    /// Combines chunk upsert (with FTS) and call graph upsert into one transaction,
    /// preventing inconsistency from crashes between separate operations.
    /// Chunks are inserted in batches of 52 rows (52 * 19 = 988 < SQLite's 999 limit).
    pub fn upsert_chunks_and_calls(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
    ) -> Result<usize, StoreError> {
        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb))
            .collect::<Result<Vec<_>, _>>()?;

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(&mut tx, chunks, &embedding_bytes, source_mtime, &now).await?;
            upsert_fts_conditional(&mut tx, chunks, &old_hashes).await?;

            // Upsert calls: delete old calls for these chunk IDs, insert new ones
            if !calls.is_empty() {
                let mut seen_ids = std::collections::HashSet::new();
                for (chunk_id, _) in calls {
                    if seen_ids.insert(chunk_id.as_str()) {
                        sqlx::query("DELETE FROM calls WHERE caller_id = ?1")
                            .bind(chunk_id)
                            .execute(&mut *tx)
                            .await?;
                    }
                }

                const INSERT_BATCH: usize = 300;
                for batch in calls.chunks(INSERT_BATCH) {
                    let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> =
                        sqlx::QueryBuilder::new(
                            "INSERT INTO calls (caller_id, callee_name, line_number) ",
                        );
                    query_builder.push_values(batch.iter(), |mut b, (chunk_id, call)| {
                        b.push_bind(chunk_id)
                            .push_bind(&call.callee_name)
                            .push_bind(call.line_number as i64);
                    });
                    query_builder.build().execute(&mut *tx).await?;
                }
            }

            tx.commit().await?;
            Ok(chunks.len())
        })
    }

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
            let missing: Vec<String> = rows
                .into_iter()
                .filter(|(origin,)| !existing_files.contains(&PathBuf::from(origin)))
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
                let placeholder_str = super::helpers::make_placeholders(batch.len());

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
                    .map(|d| d.as_secs() as i64);

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
        root: &Path,
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
                let placeholders = super::helpers::make_placeholders(batch.len());
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

                    let path = root.join(&origin);
                    let current_mtime = path
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);

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

    /// Get embeddings for chunks with matching content hashes (batch lookup).
    ///
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    pub fn get_embeddings_by_hashes(
        &self,
        hashes: &[&str],
    ) -> Result<HashMap<String, Embedding>, StoreError> {
        let _span =
            tracing::debug_span!("get_embeddings_by_hashes", count = hashes.len()).entered();
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = 500;
        let mut result = HashMap::new();

        self.rt.block_on(async {
            for batch in hashes.chunks(BATCH_SIZE) {
                let placeholders = super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT content_hash, embedding FROM chunks WHERE content_hash IN ({})",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for hash in batch {
                        q = q.bind(*hash);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in rows {
                    let hash: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    if let Some(embedding) = bytes_to_embedding(&bytes) {
                        result.insert(hash, Embedding::new(embedding));
                    }
                }
            }
            Ok(result)
        })
    }

    /// Get (chunk_id, embedding) pairs for chunks with matching content hashes.
    ///
    /// Unlike `get_embeddings_by_hashes` (which keys by content_hash), this returns
    /// the chunk ID alongside the embedding — exactly what HNSW `insert_batch` needs.
    ///
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    pub fn get_chunk_ids_and_embeddings_by_hashes(
        &self,
        hashes: &[&str],
    ) -> Result<Vec<(String, Embedding)>, StoreError> {
        let _span = tracing::debug_span!(
            "get_chunk_ids_and_embeddings_by_hashes",
            count = hashes.len()
        )
        .entered();
        if hashes.is_empty() {
            return Ok(Vec::new());
        }

        const BATCH_SIZE: usize = 500;
        let mut result = Vec::new();

        self.rt.block_on(async {
            for batch in hashes.chunks(BATCH_SIZE) {
                let placeholders = super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, embedding FROM chunks WHERE content_hash IN ({})",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for hash in batch {
                        q = q.bind(*hash);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in rows {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    if let Some(embedding) = bytes_to_embedding(&bytes) {
                        result.push((id, Embedding::new(embedding)));
                    }
                }
            }
            Ok(result)
        })
    }

    /// Get the number of chunks in the index
    pub fn chunk_count(&self) -> Result<u64, StoreError> {
        let _span = tracing::debug_span!("chunk_count").entered();
        self.rt.block_on(async {
            let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
                .fetch_one(&self.pool)
                .await?;
            Ok(row.0 as u64)
        })
    }

    /// Get index statistics
    ///
    /// Uses batched queries to minimize database round trips:
    /// 1. Single query for counts with GROUP BY using CTEs
    /// 2. Single query for all metadata keys
    pub fn stats(&self) -> Result<IndexStats, StoreError> {
        let _span = tracing::debug_span!("stats").entered();
        self.rt.block_on(async {
            // Combined counts query using CTEs (3 queries → 1)
            let (total_chunks, total_files): (i64, i64) = sqlx::query_as(
                "SELECT
                    (SELECT COUNT(*) FROM chunks),
                    (SELECT COUNT(DISTINCT origin) FROM chunks)",
            )
            .fetch_one(&self.pool)
            .await?;

            let lang_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT language, COUNT(*) FROM chunks GROUP BY language")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_language: HashMap<Language, u64> = lang_rows
                .into_iter()
                .filter_map(|(lang, count)| {
                    lang.parse()
                        .map_err(|_| {
                            tracing::warn!(
                                language = %lang,
                                count,
                                "Unknown language in database, skipping in stats"
                            );
                        })
                        .ok()
                        .map(|l| (l, count as u64))
                })
                .collect();

            let type_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT chunk_type, COUNT(*) FROM chunks GROUP BY chunk_type")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_type: HashMap<ChunkType, u64> = type_rows
                .into_iter()
                .filter_map(|(ct, count)| {
                    ct.parse()
                        .map_err(|_| {
                            tracing::warn!(
                                chunk_type = %ct,
                                count,
                                "Unknown chunk_type in database, skipping in stats"
                            );
                        })
                        .ok()
                        .map(|c| (c, count as u64))
                })
                .collect();

            // Batch metadata query (4 queries → 1)
            let metadata_rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT key, value FROM metadata WHERE key IN ('model_name', 'created_at', 'updated_at', 'schema_version')",
            )
            .fetch_all(&self.pool)
            .await?;

            let metadata: HashMap<String, String> = metadata_rows.into_iter().collect();

            let model_name = metadata.get("model_name").cloned().unwrap_or_default();
            let created_at = metadata.get("created_at").cloned().unwrap_or_default();
            let updated_at = metadata
                .get("updated_at")
                .cloned()
                .unwrap_or_else(|| created_at.clone());
            let schema_version: i32 = metadata
                .get("schema_version")
                .and_then(|s| {
                    s.parse().map_err(|e| {
                        tracing::warn!(raw = %s, error = %e, "Failed to parse schema_version, defaulting to 0");
                    }).ok()
                })
                .unwrap_or(0);

            Ok(IndexStats {
                total_chunks: total_chunks as u64,
                total_files: total_files as u64,
                chunks_by_language,
                chunks_by_type,
                index_size_bytes: 0,
                created_at,
                updated_at,
                model_name,
                schema_version,
            })
        })
    }

    /// Get all chunks for a given file (origin).
    ///
    /// Returns chunks sorted by line_start. Used by `cqs context` to list
    /// all functions/types in a file.
    pub fn get_chunks_by_origin(&self, origin: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        let _span = tracing::debug_span!("get_chunks_by_origin", origin = %origin).entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id, parent_type_name
                 FROM chunks WHERE origin = ?1
                 ORDER BY line_start",
            )
            .bind(origin)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .iter()
                .map(|r| ChunkSummary::from(ChunkRow::from_row(r)))
                .collect())
        })
    }

    /// Batch-fetch chunks by multiple origin paths.
    ///
    /// Returns a map of origin -> Vec<ChunkSummary> for all found origins.
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    /// Used by `cqs where` to avoid N+1 `get_chunks_by_origin` calls.
    pub fn get_chunks_by_origins_batch(
        &self,
        origins: &[&str],
    ) -> Result<HashMap<String, Vec<ChunkSummary>>, StoreError> {
        let _span =
            tracing::debug_span!("get_chunks_by_origins_batch", count = origins.len()).entered();
        if origins.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders = super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                            line_start, line_end, parent_id, parent_type_name
                     FROM chunks WHERE origin IN ({})
                     ORDER BY origin, line_start",
                    placeholders
                );

                let mut query = sqlx::query(&sql);
                for origin in batch {
                    query = query.bind(*origin);
                }

                let rows: Vec<_> = query.fetch_all(&self.pool).await?;
                for row in &rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(row));
                    let origin_key: String = row.get(1);
                    result.entry(origin_key).or_default().push(chunk);
                }
            }

            Ok(result)
        })
    }

    /// Batch-fetch chunks by multiple function names.
    ///
    /// Returns a map of name -> Vec<ChunkSummary> for all found names.
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    /// Used by `cqs related` to avoid N+1 `get_chunks_by_name` calls.
    pub fn get_chunks_by_names_batch(
        &self,
        names: &[&str],
    ) -> Result<HashMap<String, Vec<ChunkSummary>>, StoreError> {
        let _span =
            tracing::debug_span!("get_chunks_by_names_batch", count = names.len()).entered();
        if names.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in names.chunks(BATCH_SIZE) {
                let placeholders = super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                            line_start, line_end, parent_id, parent_type_name
                     FROM chunks WHERE name IN ({})
                     ORDER BY origin, line_start",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for name in batch {
                        q = q.bind(*name);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in &rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(row));
                    result.entry(chunk.name.clone()).or_default().push(chunk);
                }
            }

            Ok(result)
        })
    }

    /// Batch signature search: find function/method chunks matching any of the given type names.
    ///
    /// Get a chunk with its embedding vector.
    ///
    /// Returns `Ok(None)` if the chunk doesn't exist or has a corrupt embedding.
    /// Used by `cqs similar` and `cqs explain` to search by example.
    pub fn get_chunk_with_embedding(
        &self,
        id: &str,
    ) -> Result<Option<(ChunkSummary, Embedding)>, StoreError> {
        let _span = tracing::debug_span!("get_chunk_with_embedding", id = %id).entered();
        self.rt.block_on(async {
            let results = self
                .fetch_chunks_with_embeddings_by_ids_async(&[id])
                .await?;
            Ok(results.into_iter().next().and_then(|(row, bytes)| {
                match bytes_to_embedding(&bytes) {
                    Some(emb) => Some((ChunkSummary::from(row), Embedding::new(emb))),
                    None => {
                        tracing::warn!(chunk_id = %row.id, "Corrupt embedding for chunk, skipping");
                        None
                    }
                }
            }))
        })
    }

    /// Batch-fetch chunks by IDs.
    ///
    /// Returns a map of chunk ID → ChunkSummary for all found IDs.
    /// Used by `--expand` to fetch parent chunks for small-to-big retrieval.
    pub fn get_chunks_by_ids(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, ChunkSummary>, StoreError> {
        let _span = tracing::debug_span!("get_chunks_by_ids", count = ids.len()).entered();
        self.rt.block_on(async {
            let rows = self.fetch_chunks_by_ids_async(ids).await?;
            Ok(rows
                .into_iter()
                .map(|(id, row)| (id, ChunkSummary::from(row)))
                .collect())
        })
    }

    /// Batch-fetch embeddings by chunk IDs.
    ///
    /// Returns a map of chunk ID → Embedding for all found IDs.
    /// Skips chunks with corrupt embeddings. Batches queries in groups of 500
    /// to stay within SQLite's parameter limit (~999).
    ///
    /// Used by `semantic_diff` to avoid N+1 queries when comparing matched pairs.
    pub fn get_embeddings_by_ids(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, Embedding>, StoreError> {
        let _span = tracing::debug_span!("get_embeddings_by_ids", count = ids.len()).entered();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = 500;
        let mut result = HashMap::new();

        self.rt.block_on(async {
            for batch in ids.chunks(BATCH_SIZE) {
                let placeholders = super::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, embedding FROM chunks WHERE id IN ({})",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for id in batch {
                        q = q.bind(*id);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in rows {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    if let Some(emb) = bytes_to_embedding(&bytes) {
                        result.insert(id, Embedding::new(emb));
                    }
                }
            }
            Ok(result)
        })
    }

    /// Batch name search: look up multiple names in a single call.
    ///
    /// For each name, returns up to `limit_per_name` matching chunks.
    /// Batches names into groups of 20 and issues a combined FTS OR query
    /// per batch, then post-filters results to assign to matching names.
    ///
    /// Used by `gather` BFS expansion to avoid N+1 query patterns.
    pub fn search_by_names_batch(
        &self,
        names: &[&str],
        limit_per_name: usize,
    ) -> Result<HashMap<String, Vec<super::SearchResult>>, StoreError> {
        let _span =
            tracing::info_span!("search_by_names_batch", count = names.len(), limit_per_name)
                .entered();
        if names.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<super::SearchResult>> = HashMap::new();

            // Normalize and sanitize all names upfront, keeping originals for scoring
            let normalized_names: Vec<(&str, String)> = names
                .iter()
                .map(|n| (*n, super::sanitize_fts_query(&normalize_for_fts(n))))
                .filter(|(_, norm)| !norm.is_empty())
                .collect();

            // Batch into groups of 20 to avoid overly complex FTS queries
            const BATCH_SIZE: usize = 20;
            for batch in normalized_names.chunks(BATCH_SIZE) {
                // Build combined FTS query with OR
                // SAFETY: sanitize_fts_query independently strips all FTS5-significant
                // characters including double quotes, so format!-constructed FTS5
                // queries are safe even without normalize_for_fts().
                let fts_terms: Vec<String> = batch
                    .iter()
                    .filter_map(|(_, norm)| {
                        debug_assert!(
                            !norm.contains('"'),
                            "sanitized query must not contain double quotes"
                        );
                        if norm.contains('"') {
                            return None;
                        }
                        Some(format!("name:\"{}\" OR name:\"{}\"*", norm, norm))
                    })
                    .collect();
                let combined_fts = fts_terms.join(" OR ");

                // Single query for the batch with higher limit
                let total_limit = limit_per_name * batch.len();
                let rows: Vec<_> = sqlx::query(
                    "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id, c.parent_type_name
                     FROM chunks c
                     JOIN chunks_fts f ON c.id = f.id
                     WHERE chunks_fts MATCH ?1
                     ORDER BY bm25(chunks_fts, 10.0, 1.0, 1.0, 1.0)
                     LIMIT ?2",
                )
                .bind(&combined_fts)
                .bind(total_limit as i64)
                .fetch_all(&self.pool)
                .await?;

                // Post-filter: assign each row to matching names.
                // Complexity: O(results × batch.len()), but batch.len() ≤ BATCH_SIZE = 20
                // and score_name_match does fuzzy substring/prefix scoring, so a HashMap
                // on exact name is not applicable here. The bound is acceptable.
                for row in rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(&row));

                    // Find which query names this result matches
                    for (original_name, _normalized) in batch {
                        let score = super::score_name_match(&chunk.name, original_name);
                        if score > 0.0 {
                            let entry = result.entry(original_name.to_string()).or_default();
                            if entry.len() < limit_per_name {
                                entry.push(super::SearchResult {
                                    chunk: chunk.clone(),
                                    score,
                                });
                            }
                            break;
                        }
                    }
                }
            }

            Ok(result)
        })
    }

    /// Get identity metadata for all chunks (for diff comparison).
    ///
    /// Returns minimal metadata needed to match chunks across stores.
    /// Loads all rows but only lightweight columns (no content or embeddings).
    pub fn all_chunk_identities(&self) -> Result<Vec<ChunkIdentity>, StoreError> {
        let _span = tracing::debug_span!("all_chunk_identities").entered();
        self.all_chunk_identities_filtered(None)
    }

    /// Fetch a page of full chunks by rowid cursor.
    ///
    /// Returns `(chunks, next_cursor)`. When the returned vec is empty, iteration
    /// is complete. Used by the enrichment pass to iterate all chunks without
    /// loading everything into memory.
    pub fn chunks_paged(
        &self,
        after_rowid: i64,
        limit: usize,
    ) -> Result<(Vec<ChunkSummary>, i64), StoreError> {
        let _span = tracing::debug_span!("chunks_paged", after_rowid, limit).entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT rowid, id, origin, language, chunk_type, name, signature, content, doc, \
                 line_start, line_end, content_hash, window_idx, parent_id, parent_type_name \
                 FROM chunks WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
            )
            .bind(after_rowid)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            let mut max_rowid = after_rowid;
            let chunks: Vec<ChunkSummary> = rows
                .iter()
                .map(|row| {
                    let rowid: i64 = row.get("rowid");
                    if rowid > max_rowid {
                        max_rowid = rowid;
                    }
                    ChunkSummary::from(ChunkRow::from_row(row))
                })
                .collect();

            Ok((chunks, max_rowid))
        })
    }

    /// Like `all_chunk_identities` but with an optional language filter.
    ///
    /// When `language` is `Some`, only chunks matching that language are returned,
    /// avoiding loading all chunks into memory when only one language is needed.
    pub fn all_chunk_identities_filtered(
        &self,
        language: Option<&str>,
    ) -> Result<Vec<ChunkIdentity>, StoreError> {
        let _span =
            tracing::debug_span!("all_chunk_identities_filtered", language = ?language).entered();
        self.rt.block_on(async {
            let rows: Vec<_> = if let Some(lang) = language {
                sqlx::query(
                    "SELECT id, origin, name, chunk_type, language, line_start, parent_id, window_idx FROM chunks WHERE language = ?1",
                )
                .bind(lang)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query(
                    "SELECT id, origin, name, chunk_type, language, line_start, parent_id, window_idx FROM chunks",
                )
                .fetch_all(&self.pool)
                .await?
            };

            Ok(rows
                .iter()
                .map(|row| ChunkIdentity {
                    id: row.get("id"),
                    file: PathBuf::from(row.get::<String, _>("origin")),
                    name: row.get("name"),
                    chunk_type: {
                        let raw: String = row.get("chunk_type");
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "Unknown chunk_type in DB, defaulting to Function");
                            ChunkType::Function
                        })
                    },
                    line_start: clamp_line_number(row.get::<i64, _>("line_start")),
                    language: {
                        let raw: String = row.get("language");
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "Unknown language in DB, defaulting to Rust");
                            Language::Rust
                        })
                    },
                    parent_id: row.get("parent_id"),
                    window_idx: row
                        .get::<Option<i64>, _>("window_idx")
                        .map(|i| i.clamp(0, u32::MAX as i64) as u32),
                })
                .collect())
        })
    }

    /// Fetch chunks by IDs (without embeddings) — async version.
    ///
    /// Returns a map of chunk ID → ChunkRow for the given IDs.
    /// Used by search to hydrate top-N results after scoring.
    /// Batches in groups of 500 to stay under SQLite's 999-parameter limit.
    pub(crate) async fn fetch_chunks_by_ids_async(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, ChunkRow>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = 500;
        let mut result = HashMap::with_capacity(ids.len());

        for batch in ids.chunks(BATCH_SIZE) {
            let placeholders = super::helpers::make_placeholders(batch.len());
            let sql = format!(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id, parent_type_name
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for id in batch {
                    q = q.bind(*id);
                }
                q.fetch_all(&self.pool).await?
            };

            for r in &rows {
                let chunk = ChunkRow::from_row(r);
                result.insert(chunk.id.clone(), chunk);
            }
        }

        Ok(result)
    }

    /// Lightweight candidate fetch for scoring (PF-5).
    ///
    /// Returns only `(CandidateRow, embedding_bytes)` — excludes heavy `content`,
    /// `doc`, `signature`, `line_start`, `line_end` columns. Full content is
    /// loaded only for top-k survivors via `fetch_chunks_by_ids_async`.
    /// Batches in groups of 500 to stay under SQLite's 999-parameter limit.
    pub(crate) async fn fetch_candidates_by_ids_async(
        &self,
        ids: &[&str],
    ) -> Result<Vec<(CandidateRow, Vec<u8>)>, StoreError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        const BATCH_SIZE: usize = 500;
        let mut result = Vec::with_capacity(ids.len());

        for batch in ids.chunks(BATCH_SIZE) {
            let placeholders = super::helpers::make_placeholders(batch.len());
            let sql = format!(
                "SELECT id, name, origin, language, chunk_type, embedding
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for id in batch {
                    q = q.bind(*id);
                }
                q.fetch_all(&self.pool).await?
            };

            result.extend(rows.iter().map(|r| {
                let candidate = CandidateRow::from_row(r);
                let embedding_bytes: Vec<u8> = r.get("embedding");
                (candidate, embedding_bytes)
            }));
        }

        Ok(result)
    }

    /// Fetch chunks by IDs with embeddings — async version.
    ///
    /// Returns (ChunkRow, embedding_bytes) for each ID found.
    /// Used by search for candidate scoring (needs embeddings for similarity).
    pub(crate) async fn fetch_chunks_with_embeddings_by_ids_async(
        &self,
        ids: &[&str],
    ) -> Result<Vec<(ChunkRow, Vec<u8>)>, StoreError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        let placeholders = super::helpers::make_placeholders(ids.len());
        let sql = format!(
            "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id, parent_type_name, embedding
             FROM chunks WHERE id IN ({})",
            placeholders
        );

        let rows: Vec<_> = {
            let mut q = sqlx::query(&sql);
            for id in ids {
                q = q.bind(*id);
            }
            q.fetch_all(&self.pool).await?
        };

        Ok(rows
            .iter()
            .map(|r| {
                use sqlx::Row;
                let chunk = ChunkRow::from_row(r);
                let embedding_bytes: Vec<u8> = r.get("embedding");
                (chunk, embedding_bytes)
            })
            .collect())
    }

    /// Stream embeddings in batches for memory-efficient HNSW building.
    ///
    /// Uses cursor-based pagination (WHERE rowid > last_seen) for stability
    /// under concurrent writes. LIMIT/OFFSET can skip or duplicate rows if
    /// the table is modified between batches.
    ///
    /// # Arguments
    /// * `batch_size` - Number of embeddings per batch (recommend 10_000)
    ///
    /// # Returns
    /// Iterator yielding `Result<Vec<(String, Embedding)>, StoreError>`
    ///
    /// # Panics
    /// **Must be called from sync context only.** This iterator internally uses
    /// `block_on()` which will panic if called from within an async runtime.
    /// This is used for HNSW building which runs in dedicated sync threads.
    pub fn embedding_batches(
        &self,
        batch_size: usize,
    ) -> impl Iterator<Item = Result<Vec<(String, Embedding)>, StoreError>> + '_ {
        let _span = tracing::debug_span!("embedding_batches", batch_size = batch_size).entered();
        EmbeddingBatchIterator {
            store: self,
            batch_size,
            last_rowid: 0,
            done: false,
        }
    }
}

// ── Shared async helpers for chunk upsert (PERF-3) ──────────────────────────

/// Snapshot existing content hashes before INSERT overwrites them.
/// Batched in groups of 500 to stay within SQLite's 999-param limit.
async fn snapshot_content_hashes(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
) -> Result<HashMap<String, String>, StoreError> {
    const HASH_BATCH: usize = 500;
    let mut old_hashes = HashMap::new();
    let chunk_ids: Vec<&str> = chunks.iter().map(|(c, _)| c.id.as_str()).collect();
    for id_batch in chunk_ids.chunks(HASH_BATCH) {
        let placeholders = super::helpers::make_placeholders(id_batch.len());
        let sql = format!(
            "SELECT id, content_hash FROM chunks WHERE id IN ({})",
            placeholders
        );
        let mut q = sqlx::query_as::<_, (String, String)>(&sql);
        for id in id_batch {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(&mut **tx).await?;
        for (id, hash) in rows {
            old_hashes.insert(id, hash);
        }
    }
    Ok(old_hashes)
}

/// Batch INSERT chunks (55 rows × 18 params = 990 < SQLite's 999 limit).
async fn batch_insert_chunks(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
    embedding_bytes: &[Vec<u8>],
    source_mtime: Option<i64>,
    now: &str,
) -> Result<(), StoreError> {
    const CHUNK_INSERT_BATCH: usize = 52;
    for (batch_idx, batch) in chunks.chunks(CHUNK_INSERT_BATCH).enumerate() {
        let emb_offset = batch_idx * CHUNK_INSERT_BATCH;
        let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
            "INSERT OR REPLACE INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, source_mtime, created_at, updated_at, parent_id, window_idx, parent_type_name)",
        );
        qb.push_values(batch.iter().enumerate(), |mut b, (i, (chunk, _))| {
            b.push_bind(&chunk.id)
                .push_bind(crate::normalize_path(&chunk.file))
                .push_bind("file")
                .push_bind(chunk.language.to_string())
                .push_bind(chunk.chunk_type.to_string())
                .push_bind(&chunk.name)
                .push_bind(&chunk.signature)
                .push_bind(&chunk.content)
                .push_bind(&chunk.content_hash)
                .push_bind(&chunk.doc)
                .push_bind(chunk.line_start as i64)
                .push_bind(chunk.line_end as i64)
                .push_bind(&embedding_bytes[emb_offset + i])
                .push_bind(source_mtime)
                .push_bind(now)
                .push_bind(now)
                .push_bind(&chunk.parent_id)
                .push_bind(chunk.window_idx.map(|i| i as i64))
                .push_bind(&chunk.parent_type_name);
        });
        qb.build().execute(&mut **tx).await?;
    }
    Ok(())
}

/// Conditional FTS upsert: skip if content_hash unchanged (compared to pre-INSERT snapshot).
async fn upsert_fts_conditional(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
    old_hashes: &HashMap<String, String>,
) -> Result<(), StoreError> {
    for (chunk, _) in chunks {
        let content_changed = old_hashes
            .get(&chunk.id)
            .map(|old_hash| old_hash != &chunk.content_hash)
            .unwrap_or(true);

        if content_changed {
            let fts_name = normalize_for_fts(&chunk.name);
            let fts_sig = normalize_for_fts(&chunk.signature);
            let fts_content = normalize_for_fts(&chunk.content);
            let fts_doc = chunk
                .doc
                .as_ref()
                .map(|d| normalize_for_fts(d))
                .unwrap_or_default();

            sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                .bind(&chunk.id)
                .execute(&mut **tx)
                .await?;

            sqlx::query(
                "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(&chunk.id)
            .bind(&fts_name)
            .bind(&fts_sig)
            .bind(&fts_content)
            .bind(&fts_doc)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

/// Iterator for streaming embeddings in batches using cursor-based pagination
struct EmbeddingBatchIterator<'a> {
    store: &'a Store,
    batch_size: usize,
    /// Last seen rowid for cursor-based pagination
    last_rowid: i64,
    done: bool,
}

impl<'a> Iterator for EmbeddingBatchIterator<'a> {
    type Item = Result<Vec<(String, Embedding)>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }

            let result = self.store.rt.block_on(async {
                let rows: Vec<_> = sqlx::query(
                    "SELECT rowid, id, embedding FROM chunks WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
                )
                .bind(self.last_rowid)
                .bind(self.batch_size as i64)
                .fetch_all(&self.store.pool)
                .await?;

                let rows_fetched = rows.len();

                // Track the max rowid seen in this batch for the next cursor position
                let mut max_rowid = self.last_rowid;

                let batch: Vec<(String, Embedding)> = rows
                    .into_iter()
                    .filter_map(|row| {
                        let rowid: i64 = row.get(0);
                        let id: String = row.get(1);
                        let bytes: Vec<u8> = row.get(2);
                        if rowid > max_rowid {
                            max_rowid = rowid;
                        }
                        bytes_to_embedding(&bytes).map(|emb| (id, Embedding::new(emb)))
                    })
                    .collect();

                Ok((batch, rows_fetched, max_rowid))
            });

            match result {
                Ok((batch, rows_fetched, _max_rowid)) if batch.is_empty() && rows_fetched == 0 => {
                    // No more rows in database
                    self.done = true;
                    return None;
                }
                Ok((batch, _, max_rowid)) => {
                    self.last_rowid = max_rowid;
                    if batch.is_empty() {
                        // Had rows but all filtered out - continue to next batch
                        continue;
                    } else {
                        return Some(Ok(batch));
                    }
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}

// SAFETY: Once `done` is set to true, `next()` always returns None.
// This is guaranteed by the check at the start of `next()`.
impl<'a> std::iter::FusedIterator for EmbeddingBatchIterator<'a> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::Embedding;
    use crate::parser::{Chunk, ChunkType, Language};
    use crate::store::helpers::ModelInfo;
    use crate::store::Store;

    fn setup_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        (store, dir)
    }

    fn mock_embedding(seed: f32) -> Embedding {
        let mut v = vec![seed; 768];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v.push(0.0);
        Embedding::new(v)
    }

    fn make_chunk(name: &str, file: &str) -> Chunk {
        let content = format!("fn {}() {{ /* body */ }}", name);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("{}:1:{}", file, &hash[..8]),
            file: PathBuf::from(file),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    // ===== upsert_chunks_batch tests =====

    #[test]
    fn test_upsert_chunks_batch_insert_and_fetch() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk("alpha", "src/a.rs");
        let c2 = make_chunk("beta", "src/b.rs");
        let emb = mock_embedding(1.0);

        let count = store
            .upsert_chunks_batch(
                &[(c1.clone(), emb.clone()), (c2.clone(), emb.clone())],
                Some(100),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Verify via stats
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 2);
        assert_eq!(stats.total_files, 2);

        // Verify via chunk_count
        assert_eq!(store.chunk_count().unwrap(), 2);
    }

    #[test]
    fn test_upsert_chunks_batch_updates_existing() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk("alpha", "src/a.rs");
        let emb1 = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb1)], Some(100))
            .unwrap();

        // Re-insert same chunk with different embedding
        let emb2 = mock_embedding(2.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb2.clone())], Some(200))
            .unwrap();

        // Should still be 1 chunk (updated, not duplicated)
        assert_eq!(store.chunk_count().unwrap(), 1);

        // Embedding should be the updated one
        let found = store.get_embeddings_by_hashes(&[&c1.content_hash]).unwrap();
        assert!(found.contains_key(&c1.content_hash));
    }

    #[test]
    fn test_upsert_chunks_batch_empty() {
        let (store, _dir) = setup_store();
        let count = store.upsert_chunks_batch(&[], Some(100)).unwrap();
        assert_eq!(count, 0);
        assert_eq!(store.chunk_count().unwrap(), 0);
    }

    // ===== embedding_batches tests =====

    #[test]
    fn test_embedding_batches_pagination() {
        let (store, _dir) = setup_store();

        // Insert 15 chunks
        let pairs: Vec<_> = (0..15)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Batch size 10: should get 2 batches (10 + 5)
        let batches: Vec<_> = store.embedding_batches(10).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].as_ref().unwrap().len(), 10);
        assert_eq!(batches[1].as_ref().unwrap().len(), 5);
    }

    #[test]
    fn test_embedding_batches_returns_all() {
        let (store, _dir) = setup_store();

        let pairs: Vec<_> = (0..7)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        let total: usize = store
            .embedding_batches(3)
            .filter_map(|b| b.ok())
            .map(|b| b.len())
            .sum();
        assert_eq!(total, 7);
    }

    #[test]
    fn test_embedding_batches_empty_store() {
        let (store, _dir) = setup_store();
        let batches: Vec<_> = store.embedding_batches(10).collect();
        assert!(batches.is_empty());
    }

    // ===== all_chunk_identities_filtered tests =====

    #[test]
    fn test_all_chunk_identities_filtered_by_language() {
        let (store, _dir) = setup_store();

        let mut rust_chunk = make_chunk("rs_fn", "src/lib.rs");
        rust_chunk.language = Language::Rust;

        let mut py_chunk = make_chunk("py_fn", "src/main.py");
        py_chunk.language = Language::Python;
        py_chunk.id = format!("src/main.py:1:{}", &py_chunk.content_hash[..8]);

        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(
                &[(rust_chunk, emb.clone()), (py_chunk, emb.clone())],
                Some(100),
            )
            .unwrap();

        // Filter to Rust only
        let identities = store.all_chunk_identities_filtered(Some("rust")).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].language, Language::Rust);

        // Filter to Python only
        let identities = store.all_chunk_identities_filtered(Some("python")).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].language, Language::Python);

        // No filter returns all
        let identities = store.all_chunk_identities_filtered(None).unwrap();
        assert_eq!(identities.len(), 2);
    }

    // ===== get_chunks_by_origin tests =====

    #[test]
    fn test_get_chunks_by_origin_sorted_by_line() {
        let (store, _dir) = setup_store();

        let mut c1 = make_chunk("fn_late", "src/lib.rs");
        c1.line_start = 50;
        c1.line_end = 60;

        let mut c2 = make_chunk("fn_early", "src/lib.rs");
        c2.line_start = 1;
        c2.line_end = 10;
        c2.id = format!("src/lib.rs:1:{}", &c2.content_hash[..8]);

        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb.clone())], Some(100))
            .unwrap();

        let chunks = store.get_chunks_by_origin("src/lib.rs").unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks[0].line_start <= chunks[1].line_start,
            "Chunks should be sorted by line_start"
        );
    }

    #[test]
    fn test_get_chunks_by_origin_empty() {
        let (store, _dir) = setup_store();
        let chunks = store.get_chunks_by_origin("nonexistent.rs").unwrap();
        assert!(chunks.is_empty());
    }

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
            .as_secs() as i64;

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
            .as_secs() as i64;

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
            .as_secs() as i64;

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

    // ===== TC-8: LLM summary functions =====

    #[test]
    fn test_get_summaries_empty_input() {
        let (store, _dir) = setup_store();
        let result = store.get_summaries_by_hashes(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_summaries_roundtrip() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            (
                "hash_a".to_string(),
                "summary A".to_string(),
                "model-1".to_string(),
            ),
            (
                "hash_b".to_string(),
                "summary B".to_string(),
                "model-1".to_string(),
            ),
            (
                "hash_c".to_string(),
                "summary C".to_string(),
                "model-1".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let result = store
            .get_summaries_by_hashes(&["hash_a", "hash_b", "hash_c"])
            .unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result["hash_a"], "summary A");
        assert_eq!(result["hash_b"], "summary B");
        assert_eq!(result["hash_c"], "summary C");
    }

    #[test]
    fn test_get_summaries_missing_keys() {
        let (store, _dir) = setup_store();
        let result = store
            .get_summaries_by_hashes(&["nonexistent_1", "nonexistent_2"])
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_summaries_mixed() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            ("h1".to_string(), "s1".to_string(), "m".to_string()),
            ("h2".to_string(), "s2".to_string(), "m".to_string()),
            ("h3".to_string(), "s3".to_string(), "m".to_string()),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        // Query 5 hashes, only 3 exist
        let result = store
            .get_summaries_by_hashes(&["h1", "h2", "h3", "h4", "h5"])
            .unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains_key("h1"));
        assert!(result.contains_key("h2"));
        assert!(result.contains_key("h3"));
        assert!(!result.contains_key("h4"));
    }

    #[test]
    fn test_upsert_summaries_empty() {
        let (store, _dir) = setup_store();
        let count = store.upsert_summaries_batch(&[]).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_upsert_summaries_overwrites() {
        let (store, _dir) = setup_store();
        store
            .upsert_summaries_batch(&[("h1".to_string(), "first".to_string(), "m".to_string())])
            .unwrap();
        store
            .upsert_summaries_batch(&[("h1".to_string(), "second".to_string(), "m".to_string())])
            .unwrap();

        let result = store.get_summaries_by_hashes(&["h1"]).unwrap();
        assert_eq!(result["h1"], "second");
    }

    #[test]
    fn test_get_all_summaries_empty() {
        let (store, _dir) = setup_store();
        let result = store.get_all_summaries().unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_all_summaries_all() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            ("ha".to_string(), "sa".to_string(), "m".to_string()),
            ("hb".to_string(), "sb".to_string(), "m".to_string()),
            ("hc".to_string(), "sc".to_string(), "m".to_string()),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let all = store.get_all_summaries().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all["ha"], "sa");
        assert_eq!(all["hb"], "sb");
        assert_eq!(all["hc"], "sc");
    }

    #[test]
    fn test_prune_no_orphans() {
        let (store, _dir) = setup_store();

        // Insert chunks with known content_hashes
        let c1 = make_chunk("fn_a", "src/a.rs");
        let c2 = make_chunk("fn_b", "src/b.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb.clone()), (c2.clone(), emb)], Some(100))
            .unwrap();

        // Insert summaries matching those content_hashes
        let summaries = vec![
            (c1.content_hash, "summary a".to_string(), "m".to_string()),
            (c2.content_hash, "summary b".to_string(), "m".to_string()),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let pruned = store.prune_orphan_summaries().unwrap();
        assert_eq!(pruned, 0);

        // All summaries survive
        let all = store.get_all_summaries().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_prune_removes_orphans() {
        let (store, _dir) = setup_store();

        // Insert one chunk
        let c1 = make_chunk("fn_a", "src/a.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb)], Some(100))
            .unwrap();

        // Insert summaries: one matching, two orphans
        let summaries = vec![
            (
                c1.content_hash.clone(),
                "matching".to_string(),
                "m".to_string(),
            ),
            (
                "orphan_hash_1".to_string(),
                "orphan 1".to_string(),
                "m".to_string(),
            ),
            (
                "orphan_hash_2".to_string(),
                "orphan 2".to_string(),
                "m".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();
        assert_eq!(store.get_all_summaries().unwrap().len(), 3);

        let pruned = store.prune_orphan_summaries().unwrap();
        assert_eq!(pruned, 2);

        let remaining = store.get_all_summaries().unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(&c1.content_hash));
    }

    // ===== TC-11: chunks_paged =====

    #[test]
    fn test_chunks_paged_empty() {
        let (store, _dir) = setup_store();
        let (chunks, max_rowid) = store.chunks_paged(0, 10).unwrap();
        assert!(chunks.is_empty());
        assert_eq!(max_rowid, 0);
    }

    #[test]
    fn test_chunks_paged_single_page() {
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..3)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        let (chunks, max_rowid) = store.chunks_paged(0, 10).unwrap();
        assert_eq!(chunks.len(), 3);
        assert!(max_rowid > 0);
    }

    #[test]
    fn test_chunks_paged_multi_page() {
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..5)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Page 1: limit=2
        let (page1, cursor1) = store.chunks_paged(0, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert!(cursor1 > 0);

        // Page 2
        let (page2, cursor2) = store.chunks_paged(cursor1, 2).unwrap();
        assert_eq!(page2.len(), 2);
        assert!(cursor2 > cursor1);

        // Page 3: remaining
        let (page3, _cursor3) = store.chunks_paged(cursor2, 2).unwrap();
        assert_eq!(page3.len(), 1);

        // Total across all pages
        assert_eq!(page1.len() + page2.len() + page3.len(), 5);
    }

    #[test]
    fn test_chunks_paged_exact_boundary() {
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..4)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Fetch exactly 4 with limit=4
        let (page1, cursor1) = store.chunks_paged(0, 4).unwrap();
        assert_eq!(page1.len(), 4);

        // Next page should be empty
        let (page2, cursor2) = store.chunks_paged(cursor1, 4).unwrap();
        assert!(page2.is_empty());
        assert_eq!(cursor2, cursor1);
    }
}
