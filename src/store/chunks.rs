//! Chunk CRUD operations

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::Row;

use super::helpers::{
    bytes_to_embedding, clamp_line_number, embedding_to_bytes, ChunkIdentity, ChunkRow,
    ChunkSummary, IndexStats, StaleFile, StaleReport, StoreError,
};
use super::Store;
use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::{Chunk, ChunkType, Language};

/// Normalize a path to forward slashes for consistent origin storage.
///
/// On Windows, `Path::to_string_lossy()` produces backslashes which causes
/// mismatches when querying with forward-slash paths.
fn normalize_origin(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

impl Store {
    /// Retrieve a single metadata value by key.
    ///
    /// Returns `Ok(value)` if the key exists, or `Err` if not found or on DB error.
    /// Used for lightweight metadata checks (e.g., model compatibility between stores).
    pub fn get_metadata(&self, key: &str) -> Result<String, StoreError> {
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = ?1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await?;
            row.map(|(v,)| v)
                .ok_or_else(|| StoreError::Runtime(format!("metadata key '{}' not found", key)))
        })
    }

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
                .bind(normalize_origin(&chunk.file))
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
                .bind(embedding_to_bytes(embedding)?)
                .bind(source_mtime)
                .bind(&now)
                .bind(&now)
                .bind(&chunk.parent_id)
                .bind(chunk.window_idx.map(|i| i as i64))
                .execute(&mut *tx)
                .await?;

                // Pre-compute FTS normalized values
                let fts_name = normalize_for_fts(&chunk.name);
                let fts_sig = normalize_for_fts(&chunk.signature);
                let fts_content = normalize_for_fts(&chunk.content);
                let fts_doc = chunk.doc.as_ref().map(|d| normalize_for_fts(d)).unwrap_or_default();

                // Delete from FTS before insert - error must fail transaction to prevent desync
                sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                    .bind(&chunk.id)
                    .execute(&mut *tx)
                    .await?;

                sqlx::query(
                    "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(&chunk.id)
                .bind(&fts_name)
                .bind(&fts_sig)
                .bind(&fts_content)
                .bind(&fts_doc)
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
                    .bind(normalize_origin(path))
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
        let origin_str = normalize_origin(origin);

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

    /// Atomically replace all chunks for a file in a single transaction.
    ///
    /// Deletes existing chunks (+ FTS) for the origin, then inserts new chunks.
    /// This prevents data loss from crashes between separate delete and insert calls.
    pub fn replace_file_chunks(
        &self,
        origin: &Path,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
    ) -> Result<usize, StoreError> {
        let origin_str = normalize_origin(origin);

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            // Delete existing FTS entries for this origin
            sqlx::query(
                "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin = ?1)",
            )
            .bind(&origin_str)
            .execute(&mut *tx)
            .await?;

            // Delete existing chunks for this origin
            sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            // Insert new chunks + FTS
            let now = chrono::Utc::now().to_rfc3339();
            for (chunk, embedding) in chunks {
                sqlx::query(
                    "INSERT OR REPLACE INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, source_mtime, created_at, updated_at, parent_id, window_idx)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                )
                .bind(&chunk.id)
                .bind(normalize_origin(&chunk.file))
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
                .bind(embedding_to_bytes(embedding)?)
                .bind(source_mtime)
                .bind(&now)
                .bind(&now)
                .bind(&chunk.parent_id)
                .bind(chunk.window_idx.map(|i| i as i64))
                .execute(&mut *tx)
                .await?;

                // Pre-compute FTS normalized values
                let fts_name = normalize_for_fts(&chunk.name);
                let fts_sig = normalize_for_fts(&chunk.signature);
                let fts_content = normalize_for_fts(&chunk.content);
                let fts_doc = chunk.doc.as_ref().map(|d| normalize_for_fts(d)).unwrap_or_default();

                sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                    .bind(&chunk.id)
                    .execute(&mut *tx)
                    .await?;

                sqlx::query(
                    "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(&chunk.id)
                .bind(&fts_name)
                .bind(&fts_sig)
                .bind(&fts_content)
                .bind(&fts_doc)
                .execute(&mut *tx)
                .await?;
            }

            tx.commit().await?;
            Ok(chunks.len())
        })
    }

    /// Atomically upsert chunks and their call graph in a single transaction.
    ///
    /// Combines chunk upsert (with FTS) and call graph upsert into one transaction,
    /// preventing inconsistency from crashes between separate operations.
    pub fn upsert_chunks_and_calls(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
    ) -> Result<usize, StoreError> {
        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            // Upsert chunks + FTS
            let now = chrono::Utc::now().to_rfc3339();
            for (chunk, embedding) in chunks {
                sqlx::query(
                    "INSERT OR REPLACE INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, source_mtime, created_at, updated_at, parent_id, window_idx)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                )
                .bind(&chunk.id)
                .bind(normalize_origin(&chunk.file))
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
                .bind(embedding_to_bytes(embedding)?)
                .bind(source_mtime)
                .bind(&now)
                .bind(&now)
                .bind(&chunk.parent_id)
                .bind(chunk.window_idx.map(|i| i as i64))
                .execute(&mut *tx)
                .await?;

                // Pre-compute FTS normalized values
                let fts_name = normalize_for_fts(&chunk.name);
                let fts_sig = normalize_for_fts(&chunk.signature);
                let fts_content = normalize_for_fts(&chunk.content);
                let fts_doc = chunk.doc.as_ref().map(|d| normalize_for_fts(d)).unwrap_or_default();

                sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                    .bind(&chunk.id)
                    .execute(&mut *tx)
                    .await?;

                sqlx::query(
                    "INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(&chunk.id)
                .bind(&fts_name)
                .bind(&fts_sig)
                .bind(&fts_content)
                .bind(&fts_doc)
                .execute(&mut *tx)
                .await?;
            }

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

                // 300 rows * 3 binds = 900 < SQLite's 999 limit
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

    /// List files that are stale (mtime changed) or missing from disk.
    ///
    /// Like `count_stale_files()` but returns full details for display.
    /// Requires `existing_files` from `enumerate_files()` (~100ms for 10k files).
    pub fn list_stale_files(
        &self,
        existing_files: &HashSet<PathBuf>,
    ) -> Result<StaleReport, StoreError> {
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
                    missing.push(origin);
                    continue;
                }

                let stored = match stored_mtime {
                    Some(m) => m,
                    None => {
                        // NULL mtime → treat as stale (can't verify freshness)
                        stale.push(StaleFile {
                            origin,
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
                            origin,
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
        if origins.is_empty() {
            return Ok(HashSet::new());
        }

        self.rt.block_on(async {
            // Batch query: get stored mtimes for these origins
            let placeholders: String = (1..=origins.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT origin, source_mtime FROM chunks WHERE origin IN ({}) GROUP BY origin",
                placeholders
            );

            let mut query = sqlx::query_as::<_, (String, Option<i64>)>(&sql);
            for origin in origins {
                query = query.bind(*origin);
            }
            let rows = query.fetch_all(&self.pool).await?;

            let mut stale = HashSet::new();
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

            Ok(stale)
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

    /// Batch-fetch chunks by multiple origin paths.
    ///
    /// Returns a map of origin -> Vec<ChunkSummary> for all found origins.
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    /// Used by `cqs where` to avoid N+1 `get_chunks_by_origin` calls.
    pub fn get_chunks_by_origins_batch(
        &self,
        origins: &[&str],
    ) -> Result<HashMap<String, Vec<ChunkSummary>>, StoreError> {
        if origins.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders: Vec<String> =
                    (1..=batch.len()).map(|i| format!("?{}", i)).collect();
                let sql = format!(
                    "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                            line_start, line_end, parent_id
                     FROM chunks WHERE origin IN ({})
                     ORDER BY origin, line_start",
                    placeholders.join(", ")
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

    /// Get chunks by function name.
    ///
    /// Returns all chunks with the given name (may span multiple files).
    /// Used by `cqs related` to resolve function names to file locations.
    pub fn get_chunks_by_name(&self, name: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id
                 FROM chunks WHERE name = ?1
                 ORDER BY origin, line_start",
            )
            .bind(name)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .iter()
                .map(|r| ChunkSummary::from(ChunkRow::from_row(r)))
                .collect())
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
        if names.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in names.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                            line_start, line_end, parent_id
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

    /// Find function/method chunks whose signature contains a type name.
    ///
    /// Uses `LIKE '%name%'` on the signature column. Used by `cqs related`
    /// to find functions sharing custom types.
    pub fn search_chunks_by_signature(
        &self,
        type_name: &str,
    ) -> Result<Vec<ChunkSummary>, StoreError> {
        self.rt.block_on(async {
            // Escape LIKE wildcards in user input to prevent unintended pattern matching
            let escaped = type_name
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let pattern = format!("%{}%", escaped);
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id
                 FROM chunks
                 WHERE chunk_type IN ('function', 'method')
                   AND signature LIKE ?1 ESCAPE '\\'
                 ORDER BY origin, line_start
                 LIMIT 100",
            )
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .iter()
                .map(|r| ChunkSummary::from(ChunkRow::from_row(r)))
                .collect())
        })
    }

    /// Batch signature search: find function/method chunks matching any of the given type names.
    ///
    /// Combines multiple LIKE patterns into a single query with OR, reducing N queries to 1.
    /// Each result is tagged with which type name(s) it matched. Used by `cqs related`
    /// to avoid per-type LIKE scans.
    pub fn search_chunks_by_signatures_batch(
        &self,
        type_names: &[String],
    ) -> Result<Vec<(String, ChunkSummary)>, StoreError> {
        if type_names.is_empty() {
            return Ok(Vec::new());
        }

        self.rt.block_on(async {
            // Build OR clauses for all type names
            let mut conditions = Vec::new();
            let mut bind_values = Vec::new();
            for (i, type_name) in type_names.iter().enumerate() {
                let escaped = type_name
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                let pattern = format!("%{}%", escaped);
                conditions.push(format!("signature LIKE ?{} ESCAPE '\\'", i + 1));
                bind_values.push(pattern);
            }

            let where_clause = conditions.join(" OR ");
            let sql = format!(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id
                 FROM chunks
                 WHERE chunk_type IN ('function', 'method')
                   AND ({})
                 ORDER BY origin, line_start
                 LIMIT 500",
                where_clause
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for val in &bind_values {
                    q = q.bind(val);
                }
                q.fetch_all(&self.pool).await?
            };

            // For each row, determine which type name(s) matched
            let mut results = Vec::new();
            for row in &rows {
                let chunk = ChunkSummary::from(ChunkRow::from_row(row));
                for type_name in type_names {
                    if chunk.signature.contains(type_name.as_str()) {
                        results.push((type_name.clone(), chunk.clone()));
                    }
                }
            }

            Ok(results)
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

    /// Batch-fetch chunks by IDs.
    ///
    /// Returns a map of chunk ID → ChunkSummary for all found IDs.
    /// Used by `--expand` to fetch parent chunks for small-to-big retrieval.
    pub fn get_chunks_by_ids(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, ChunkSummary>, StoreError> {
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
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = 500;
        let mut result = HashMap::new();

        self.rt.block_on(async {
            for batch in ids.chunks(BATCH_SIZE) {
                let placeholders: String = (1..=batch.len())
                    .map(|i| format!("?{}", i))
                    .collect::<Vec<_>>()
                    .join(", ");
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
                let fts_terms: Vec<String> = batch
                    .iter()
                    .map(|(_, norm)| format!("name:\"{}\" OR name:\"{}\"*", norm, norm))
                    .collect();
                let combined_fts = fts_terms.join(" OR ");

                // Single query for the batch with higher limit
                let total_limit = limit_per_name * batch.len();
                let rows: Vec<_> = sqlx::query(
                    "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id
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

                // Post-filter: assign each row to matching names
                for row in rows {
                    let chunk = ChunkSummary::from(ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: clamp_line_number(row.get::<i64, _>(8)),
                        line_end: clamp_line_number(row.get::<i64, _>(9)),
                        parent_id: row.get(10),
                    });

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
        self.all_chunk_identities_filtered(None)
    }

    /// Like `all_chunk_identities` but with an optional language filter.
    ///
    /// When `language` is `Some`, only chunks matching that language are returned,
    /// avoiding loading all chunks into memory when only one language is needed.
    pub fn all_chunk_identities_filtered(
        &self,
        language: Option<&str>,
    ) -> Result<Vec<ChunkIdentity>, StoreError> {
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
                    origin: row.get("origin"),
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
        EmbeddingBatchIterator {
            store: self,
            batch_size,
            last_rowid: 0,
            done: false,
        }
    }
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
            Ok((batch, rows_fetched, max_rowid)) if batch.is_empty() && rows_fetched == 0 => {
                // No more rows in database
                self.done = true;
                None
            }
            Ok((batch, _, max_rowid)) => {
                self.last_rowid = max_rowid;
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
        let found = store.get_by_content_hash(&c1.content_hash);
        assert!(found.is_some());
    }

    #[test]
    fn test_upsert_chunks_batch_empty() {
        let (store, _dir) = setup_store();
        let count = store.upsert_chunks_batch(&[], Some(100)).unwrap();
        assert_eq!(count, 0);
        assert_eq!(store.chunk_count().unwrap(), 0);
    }

    // ===== replace_file_chunks tests =====

    #[test]
    fn test_replace_file_chunks_removes_old() {
        let (store, _dir) = setup_store();

        // Insert two chunks from same file
        let c1 = make_chunk("old_fn1", "src/lib.rs");
        let c2 = make_chunk("old_fn2", "src/lib.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb.clone())], Some(100))
            .unwrap();
        assert_eq!(store.chunk_count().unwrap(), 2);

        // Replace with one new chunk
        let c3 = make_chunk("new_fn", "src/lib.rs");
        let replaced = store
            .replace_file_chunks(
                &PathBuf::from("src/lib.rs"),
                &[(c3.clone(), emb.clone())],
                Some(200),
            )
            .unwrap();
        assert_eq!(replaced, 1);

        // Only the new chunk should remain
        assert_eq!(store.chunk_count().unwrap(), 1);
        let chunks = store.get_chunks_by_origin("src/lib.rs").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "new_fn");
    }

    #[test]
    fn test_replace_file_chunks_doesnt_affect_other_files() {
        let (store, _dir) = setup_store();

        let c_a = make_chunk("fn_a", "src/a.rs");
        let c_b = make_chunk("fn_b", "src/b.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c_a, emb.clone()), (c_b, emb.clone())], Some(100))
            .unwrap();

        // Replace only a.rs
        let c_new = make_chunk("fn_a_new", "src/a.rs");
        store
            .replace_file_chunks(
                &PathBuf::from("src/a.rs"),
                &[(c_new, emb.clone())],
                Some(200),
            )
            .unwrap();

        // b.rs should be untouched
        assert_eq!(store.chunk_count().unwrap(), 2);
        let b_chunks = store.get_chunks_by_origin("src/b.rs").unwrap();
        assert_eq!(b_chunks.len(), 1);
        assert_eq!(b_chunks[0].name, "fn_b");
    }

    #[test]
    fn test_replace_file_chunks_with_empty_clears_file() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk("fn1", "src/lib.rs");
        let emb = mock_embedding(1.0);
        store.upsert_chunks_batch(&[(c1, emb)], Some(100)).unwrap();
        assert_eq!(store.chunk_count().unwrap(), 1);

        // Replace with empty list
        let replaced = store
            .replace_file_chunks(&PathBuf::from("src/lib.rs"), &[], Some(200))
            .unwrap();
        assert_eq!(replaced, 0);
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
