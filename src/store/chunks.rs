//! Chunk CRUD operations

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::Row;

use super::helpers::{
    bytes_to_embedding, clamp_line_number, embedding_to_bytes, ChunkIdentity, ChunkRow,
    ChunkSummary, IndexStats, StoreError,
};
use super::Store;
use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::{Chunk, ChunkType, Language};

impl Store {
    /// Insert or update chunks in batch (10x faster than individual inserts)
    ///
    /// FTS operations (DELETE then INSERT per chunk) are not batched because:
    /// - FTS5 doesn't support INSERT OR REPLACE, requiring DELETE+INSERT
    /// - Batching DELETEs with WHERE IN requires dynamic SQL with varying params
    /// - All operations run in a single transaction, so disk I/O is already batched
    /// - FTS operations are fast (in-memory B-tree), not the bottleneck vs embeddings
    pub fn upsert_chunks_batch(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
    ) -> Result<usize, StoreError> {
        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            let now = chrono::Utc::now().to_rfc3339();
            for (chunk, embedding) in chunks {
                sqlx::query(
                    "INSERT OR REPLACE INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, source_mtime, created_at, updated_at, parent_id, window_idx)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                )
                .bind(&chunk.id)
                .bind(chunk.file.to_string_lossy().into_owned())
                .bind("file")
                .bind(chunk.language.to_string())
                .bind(chunk.chunk_type.to_string())
                .bind(&chunk.name)
                .bind(&chunk.signature)
                .bind(&chunk.content)
                .bind(&chunk.content_hash)
                .bind(&chunk.doc)
                .bind(chunk.line_start as i64)
                .bind(chunk.line_end as i64)
                .bind(embedding_to_bytes(embedding))
                .bind(source_mtime)
                .bind(&now)
                .bind(&now)
                .bind(&chunk.parent_id)
                .bind(chunk.window_idx.map(|i| i as i64))
                .execute(&mut *tx)
                .await?;

                // Delete from FTS before insert - error must fail transaction to prevent desync
                sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                    .bind(&chunk.id)
                    .execute(&mut *tx)
                    .await?;

                sqlx::query(
                    "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(&chunk.id)
                .bind(normalize_for_fts(&chunk.name))
                .bind(normalize_for_fts(&chunk.signature))
                .bind(normalize_for_fts(&chunk.content))
                .bind(chunk.doc.as_ref().map(|d| normalize_for_fts(d)).unwrap_or_default())
                .execute(&mut *tx)
                .await?;
            }

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
        self.upsert_chunks_batch(&[(chunk.clone(), embedding.clone())], source_mtime)?;
        Ok(())
    }

    /// Check if a file needs reindexing based on mtime.
    ///
    /// Returns `Ok(Some(mtime))` if reindex needed (with the file's current mtime),
    /// or `Ok(None)` if no reindex needed. This avoids reading file metadata twice.
    pub fn needs_reindex(&self, path: &Path) -> Result<Option<i64>, StoreError> {
        let current_mtime = path
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| StoreError::SystemTime)?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(Option<i64>,)> =
                sqlx::query_as("SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1")
                    .bind(path.to_string_lossy().into_owned())
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
        let origin_str = origin.to_string_lossy().into_owned();

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

    /// Delete chunks for files that no longer exist
    ///
    /// Batches deletes in groups of 100 to balance memory usage and query efficiency.
    ///
    /// Uses Rust HashSet for existence check rather than SQL WHERE NOT IN because:
    /// - Existing files often number 10k+, exceeding SQLite's parameter limit (~999)
    /// - Sending full file list to SQLite would require chunked queries anyway
    /// - HashSet lookup is O(1), and we already have the set from enumerate_files()
    pub fn prune_missing(&self, existing_files: &HashSet<PathBuf>) -> Result<u32, StoreError> {
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

            // Batch delete in chunks of 100 (SQLite has ~999 param limit)
            // Each batch is wrapped in a transaction for atomicity
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;

            for batch in missing.chunks(BATCH_SIZE) {
                let mut tx = self.pool.begin().await?;

                let placeholders: Vec<String> =
                    (1..=batch.len()).map(|i| format!("?{}", i)).collect();
                let placeholder_str = placeholders.join(",");

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

                tx.commit().await?;
            }

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
        self.rt.block_on(async {
            let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
                "SELECT DISTINCT origin, source_mtime FROM chunks WHERE source_type = 'file'",
            )
            .fetch_all(&self.pool)
            .await?;

            let mut stale = 0u64;
            let mut missing = 0u64;

            for (origin, stored_mtime) in rows {
                let path = PathBuf::from(&origin);
                if !existing_files.contains(&path) {
                    missing += 1;
                    continue;
                }

                // Check mtime
                if let Some(stored) = stored_mtime {
                    let current_mtime = path
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);

                    if let Some(current) = current_mtime {
                        if current > stored {
                            stale += 1;
                        }
                    }
                }
            }

            Ok((stale, missing))
        })
    }

    /// Get embedding by content hash (for reuse when content unchanged)
    ///
    /// Note: Prefer `get_embeddings_by_hashes` for batch lookups in production.
    pub fn get_by_content_hash(&self, hash: &str) -> Option<Embedding> {
        self.rt.block_on(async {
            let row: Option<(Vec<u8>,)> = match sqlx::query_as(
                "SELECT embedding FROM chunks WHERE content_hash = ?1 LIMIT 1",
            )
            .bind(hash)
            .fetch_optional(&self.pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Failed to fetch embedding by content_hash: {}", e);
                    return None;
                }
            };

            row.and_then(|(bytes,)| bytes_to_embedding(&bytes).map(Embedding::new))
        })
    }

    /// Get embeddings for chunks with matching content hashes (batch lookup).
    ///
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    pub fn get_embeddings_by_hashes(&self, hashes: &[&str]) -> HashMap<String, Embedding> {
        if hashes.is_empty() {
            return HashMap::new();
        }

        const BATCH_SIZE: usize = 500;
        let mut result = HashMap::new();

        self.rt.block_on(async {
            for batch in hashes.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT content_hash, embedding FROM chunks WHERE content_hash IN ({})",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for hash in batch {
                        q = q.bind(*hash);
                    }
                    match q.fetch_all(&self.pool).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!("Failed to fetch embeddings by hash: {}", e);
                            continue;
                        }
                    }
                };

                for row in rows {
                    let hash: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    if let Some(embedding) = bytes_to_embedding(&bytes) {
                        result.insert(hash, Embedding::new(embedding));
                    }
                }
            }
            result
        })
    }

    /// Get the number of chunks in the index
    pub fn chunk_count(&self) -> Result<u64, StoreError> {
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
                .and_then(|s| s.parse().ok())
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
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id
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

    /// Get a single chunk by its ID
    pub fn get_chunk_by_id(&self, id: &str) -> Result<Option<ChunkSummary>, StoreError> {
        self.rt.block_on(async {
            let row: Option<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id
                 FROM chunks WHERE id = ?1",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|r| ChunkSummary::from(ChunkRow::from_row(&r))))
        })
    }

    /// Get a chunk with its embedding vector.
    ///
    /// Returns `Ok(None)` if the chunk doesn't exist or has a corrupt embedding.
    /// Used by `cqs similar` and `cqs explain` to search by example.
    pub fn get_chunk_with_embedding(
        &self,
        id: &str,
    ) -> Result<Option<(ChunkSummary, Embedding)>, StoreError> {
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

    /// Get identity metadata for all chunks (for diff comparison).
    ///
    /// Returns minimal metadata needed to match chunks across stores.
    /// Loads all rows but only lightweight columns (no content or embeddings).
    pub fn all_chunk_identities(&self) -> Result<Vec<ChunkIdentity>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, name, chunk_type, line_start, parent_id, window_idx FROM chunks",
            )
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .iter()
                .map(|row| ChunkIdentity {
                    id: row.get("id"),
                    origin: row.get("origin"),
                    name: row.get("name"),
                    chunk_type: row.get("chunk_type"),
                    line_start: clamp_line_number(row.get::<i64, _>("line_start")),
                    parent_id: row.get("parent_id"),
                    window_idx: row
                        .get::<Option<i64>, _>("window_idx")
                        .map(|i| i as u32),
                })
                .collect())
        })
    }

    /// Fetch chunks by IDs (without embeddings) — async version.
    ///
    /// Returns a map of chunk ID → ChunkRow for the given IDs.
    /// Used by search to hydrate top-N results after scoring.
    pub(crate) async fn fetch_chunks_by_ids_async(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, ChunkRow>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let placeholders: String = (1..=ids.len())
            .map(|i| format!("?{}", i))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id
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
                let chunk = ChunkRow::from_row(r);
                (chunk.id.clone(), chunk)
            })
            .collect())
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

        let placeholders: String = (1..=ids.len())
            .map(|i| format!("?{}", i))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id, embedding
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

    /// Get all chunk IDs and embeddings (for HNSW index building)
    ///
    /// **Warning:** This loads all embeddings into memory at once.
    /// For large indexes (>50k chunks), prefer `embedding_batches()` to avoid OOM.
    ///
    /// Logs a warning if any embeddings are skipped due to corruption (wrong size).
    pub fn all_embeddings(&self) -> Result<Vec<(String, Embedding)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, embedding FROM chunks")
                .fetch_all(&self.pool)
                .await?;

            let total_rows = rows.len();
            let mut skipped = 0usize;

            let results: Vec<(String, Embedding)> = rows
                .into_iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    match bytes_to_embedding(&bytes) {
                        Some(emb) => Some((id, Embedding::new(emb))),
                        None => {
                            skipped += 1;
                            None
                        }
                    }
                })
                .collect();

            if skipped > 0 {
                tracing::warn!(
                    skipped,
                    total = total_rows,
                    "Skipped corrupted embeddings (wrong size). Run 'cqs index --force' to rebuild."
                );
            }

            Ok(results)
        })
    }

    /// Stream embeddings in batches for memory-efficient HNSW building.
    ///
    /// Uses LIMIT/OFFSET pagination to avoid loading all embeddings at once.
    /// Each batch contains up to `batch_size` embeddings (~30MB for 10k).
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
        EmbeddingBatchIterator {
            store: self,
            batch_size,
            offset: 0,
            done: false,
        }
    }
}

/// Iterator for streaming embeddings in batches
struct EmbeddingBatchIterator<'a> {
    store: &'a Store,
    batch_size: usize,
    offset: usize,
    done: bool,
}

impl<'a> Iterator for EmbeddingBatchIterator<'a> {
    type Item = Result<Vec<(String, Embedding)>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let result = self.store.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, embedding FROM chunks LIMIT ?1 OFFSET ?2")
                .bind(self.batch_size as i64)
                .bind(self.offset as i64)
                .fetch_all(&self.store.pool)
                .await?;

            // Track actual rows fetched (before filtering) to correctly advance SQL OFFSET.
            // If we used batch.len() (after filter_map), we'd under-count when some rows
            // have invalid embeddings, causing duplicate fetches or missed rows.
            let rows_fetched = rows.len();

            let batch: Vec<(String, Embedding)> = rows
                .into_iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    bytes_to_embedding(&bytes).map(|emb| (id, Embedding::new(emb)))
                })
                .collect();

            Ok((batch, rows_fetched))
        });

        match result {
            Ok((batch, rows_fetched)) if batch.is_empty() && rows_fetched == 0 => {
                // No more rows in database
                self.done = true;
                None
            }
            Ok((batch, rows_fetched)) => {
                self.offset += rows_fetched;
                if batch.is_empty() {
                    // Had rows but all filtered out - continue to next batch
                    self.next()
                } else {
                    Some(Ok(batch))
                }
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

// SAFETY: Once `done` is set to true, `next()` always returns None.
// This is guaranteed by the check at the start of `next()`.
impl<'a> std::iter::FusedIterator for EmbeddingBatchIterator<'a> {}
