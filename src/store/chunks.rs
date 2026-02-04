//! Chunk CRUD operations

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::Row;

use super::helpers::{
    bytes_to_embedding, clamp_line_number, embedding_to_bytes, ChunkRow, ChunkSummary, IndexStats,
    StoreError,
};
use super::Store;
use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::{Chunk, ChunkType, Language};

impl Store {
    /// Insert or update chunks in batch (10x faster than individual inserts)
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
                .bind(chunk.file.to_string_lossy().to_string())
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

                if let Err(e) = sqlx::query("DELETE FROM chunks_fts WHERE id = ?1")
                    .bind(&chunk.id)
                    .execute(&mut *tx)
                    .await
                {
                    tracing::warn!("Failed to delete from chunks_fts: {}", e);
                }

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

    /// Check if a file needs reindexing based on mtime
    pub fn needs_reindex(&self, path: &Path) -> Result<bool, StoreError> {
        let current_mtime = path
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| StoreError::SystemTime)?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(Option<i64>,)> =
                sqlx::query_as("SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1")
                    .bind(path.to_string_lossy().to_string())
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((Some(mtime),)) if mtime >= current_mtime => Ok(false),
                _ => Ok(true),
            }
        })
    }

    /// Delete all chunks for an origin (file path or source identifier)
    pub fn delete_by_origin(&self, origin: &Path) -> Result<u32, StoreError> {
        let origin_str = origin.to_string_lossy().to_string();

        self.rt.block_on(async {
            sqlx::query(
                "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin = ?1)",
            )
            .bind(&origin_str)
            .execute(&self.pool)
            .await?;

            let result = sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                .bind(&origin_str)
                .execute(&self.pool)
                .await?;

            Ok(result.rows_affected() as u32)
        })
    }

    /// Delete chunks for files that no longer exist
    ///
    /// Batches deletes in groups of 100 to balance memory usage and query efficiency.
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
            const BATCH_SIZE: usize = 100;
            let mut deleted = 0u32;

            for batch in missing.chunks(BATCH_SIZE) {
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
                fts_stmt.execute(&self.pool).await?;

                // Delete from chunks
                let chunks_query =
                    format!("DELETE FROM chunks WHERE origin IN ({})", placeholder_str);
                let mut chunks_stmt = sqlx::query(&chunks_query);
                for origin in batch {
                    chunks_stmt = chunks_stmt.bind(origin);
                }
                let result = chunks_stmt.execute(&self.pool).await?;
                deleted += result.rows_affected() as u32;
            }

            Ok(deleted)
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

            row.map(|(bytes,)| Embedding::new(bytes_to_embedding(&bytes)))
        })
    }

    /// Get embeddings for chunks with matching content hashes (batch lookup).
    pub fn get_embeddings_by_hashes(&self, hashes: &[&str]) -> HashMap<String, Embedding> {
        if hashes.is_empty() {
            return HashMap::new();
        }

        self.rt.block_on(async {
            let placeholders: String = (1..=hashes.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT content_hash, embedding FROM chunks WHERE content_hash IN ({})",
                placeholders
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for hash in hashes {
                    q = q.bind(*hash);
                }
                match q.fetch_all(&self.pool).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to fetch embeddings by hash: {}", e);
                        return HashMap::new();
                    }
                }
            };

            let mut result = HashMap::new();
            for row in rows {
                let hash: String = row.get(0);
                let bytes: Vec<u8> = row.get(1);
                result.insert(hash, Embedding::new(bytes_to_embedding(&bytes)));
            }
            result
        })
    }

    /// Get the number of chunks in the index
    pub fn chunk_count(&self) -> Result<usize, StoreError> {
        self.rt.block_on(async {
            let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
                .fetch_one(&self.pool)
                .await?;
            Ok(row.0 as usize)
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
                .filter_map(|(lang, count)| lang.parse().ok().map(|l| (l, count as u64)))
                .collect();

            let type_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT chunk_type, COUNT(*) FROM chunks GROUP BY chunk_type")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_type: HashMap<ChunkType, u64> = type_rows
                .into_iter()
                .filter_map(|(ct, count)| ct.parse().ok().map(|c| (c, count as u64)))
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

            Ok(row.map(|r| {
                ChunkSummary::from(ChunkRow {
                    id: r.get(0),
                    origin: r.get(1),
                    language: r.get(2),
                    chunk_type: r.get(3),
                    name: r.get(4),
                    signature: r.get(5),
                    content: r.get(6),
                    doc: r.get(7),
                    line_start: clamp_line_number(r.get::<i64, _>(8)),
                    line_end: clamp_line_number(r.get::<i64, _>(9)),
                    parent_id: r.get(10),
                })
            }))
        })
    }

    /// Get all chunk IDs and embeddings (for HNSW index building)
    pub fn all_embeddings(&self) -> Result<Vec<(String, Embedding)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, embedding FROM chunks")
                .fetch_all(&self.pool)
                .await?;

            let results: Vec<(String, Embedding)> = rows
                .into_iter()
                .map(|row| {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    (id, Embedding::new(bytes_to_embedding(&bytes)))
                })
                .collect();

            Ok(results)
        })
    }
}
