// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Chunk upsert, metadata, delete, and summary operations.

use std::path::Path;

use crate::embedder::Embedding;
use crate::parser::Chunk;
use crate::store::helpers::{embedding_to_bytes, StoreError};
use crate::store::{ReadWrite, Store};

use super::async_helpers::{batch_insert_chunks, snapshot_content_hashes, upsert_fts_conditional};

impl<Mode> Store<Mode> {
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

    /// Get enrichment hashes for a batch of chunk IDs.
    ///
    /// Returns a map from chunk_id to enrichment_hash (only for chunks that have one).
    pub fn get_enrichment_hashes_batch(
        &self,
        chunk_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span =
            tracing::debug_span!("get_enrichment_hashes_batch", count = chunk_ids.len()).entered();
        if chunk_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.rt.block_on(async {
            let mut result = std::collections::HashMap::new();
            use crate::store::helpers::sql::max_rows_per_statement;
            for batch in chunk_ids.chunks(max_rows_per_statement(1)) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
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

    /// Fetch all enrichment hashes in a single query.
    ///
    /// Returns a map from chunk_id to enrichment_hash for all chunks that have one.
    /// Used by the enrichment pass to avoid per-page hash fetches (PERF-29).
    pub fn get_all_enrichment_hashes(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span = tracing::debug_span!("get_all_enrichment_hashes").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT id, enrichment_hash FROM chunks WHERE enrichment_hash IS NOT NULL",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows.into_iter().collect())
        })
    }

    /// Get LLM summaries for a batch of content hashes.
    ///
    /// Returns a map from content_hash to summary text. Only includes hashes
    /// that have summaries in the llm_summaries table matching the given purpose.
    pub fn get_summaries_by_hashes(
        &self,
        content_hashes: &[&str],
        purpose: &str,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span = tracing::debug_span!(
            "get_summaries_by_hashes",
            count = content_hashes.len(),
            purpose
        )
        .entered();
        if content_hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.rt.block_on(async {
            let mut result = std::collections::HashMap::new();
            use crate::store::helpers::sql::max_rows_per_statement;
            // Reserve one param for the purpose bind, so (limit - 1) per batch
            for batch in content_hashes.chunks(max_rows_per_statement(1) - 1) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT content_hash, summary FROM llm_summaries WHERE content_hash IN ({}) AND purpose = ?{}",
                    placeholders,
                    batch.len() + 1
                );
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for hash in batch {
                    query = query.bind(*hash);
                }
                query = query.bind(purpose);
                let rows = query.fetch_all(&self.pool).await?;
                for (hash, summary) in rows {
                    result.insert(hash, summary);
                }
            }
            Ok(result)
        })
    }

    /// Fetch all LLM summaries as a map from content_hash to summary text.
    ///
    /// Single query, no batching needed (reads entire table). Used by the
    /// enrichment pass to avoid per-page summary fetches.
    pub fn get_all_summaries(
        &self,
        purpose: &str,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span = tracing::debug_span!("get_all_summaries", purpose).entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT content_hash, summary FROM llm_summaries WHERE purpose = ?1",
            )
            .bind(purpose)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows.into_iter().collect())
        })
    }

    /// Get all distinct content hashes currently in the chunks table.
    /// Used to validate batch results against the current index (DS-20).
    pub fn get_all_content_hashes(&self) -> Result<Vec<String>, StoreError> {
        let _span = tracing::debug_span!("get_all_content_hashes").entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT content_hash FROM chunks")
                .fetch_all(&self.pool)
                .await?;
            Ok(rows.into_iter().map(|(h,)| h).collect())
        })
    }

    /// Get all summaries with full metadata for backup/restore.
    /// Returns Vec of (content_hash, summary, model, purpose).
    pub fn get_all_summaries_full(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, StoreError> {
        let _span = tracing::debug_span!("get_all_summaries_full").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String, String, String)> =
                sqlx::query_as("SELECT content_hash, summary, model, purpose FROM llm_summaries")
                    .fetch_all(&self.pool)
                    .await?;
            Ok(rows)
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
            .as_millis() as i64;

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
}

// Write methods live on `impl Store<ReadWrite>` — the compiler refuses to
// call them on a `Store<ReadOnly>`. Closes the bug class in GitHub #946.
impl Store<ReadWrite> {
    /// Insert or update chunks in batch using multi-row INSERT.
    ///
    /// Chunks are inserted in batches of 52 rows (52 * 19 params = 988 < SQLite's 999 limit).
    /// FTS operations remain per-row because FTS5 doesn't support INSERT OR REPLACE.
    ///
    /// **DS-19 warning:** Uses `INSERT OR REPLACE` which triggers `ON DELETE CASCADE` on
    /// `calls` and `type_edges` tables. Callers must re-populate call graph edges after
    /// this function if the chunks had existing relationships.
    pub fn upsert_chunks_batch(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("upsert_chunks_batch", count = chunks.len()).entered();

        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
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
        if updates.is_empty() {
            tracing::debug!("update_embeddings_batch called with empty batch, skipping");
            return Ok(0);
        }
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

        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = updates
            .iter()
            .map(|(_, emb, _)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        // PERF-40: Temp table + single UPDATE...FROM instead of N individual UPDATEs.
        // Reduces ~10K round-trips to ~100 batch INSERTs + 1 UPDATE.
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // 1. Create temp table for batch staging
            sqlx::query(
                "CREATE TEMP TABLE IF NOT EXISTS _update_embeddings \
                 (id TEXT PRIMARY KEY, embedding BLOB NOT NULL, enrichment_hash TEXT)",
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query("DELETE FROM _update_embeddings")
                .execute(&mut *tx)
                .await?;

            // 2. Batch INSERT into temp table. PF-V1.25-9: previously
            // `BATCH_SIZE = 100` (100 × 3 = 300 binds), sized for the
            // pre-3.32 SQLite 999-variable limit. Modern SQLite permits
            // 32766; `max_rows_per_statement(3)` derives ~10822 rows per
            // statement. On a full reindex with 50k updated embeddings
            // that's ~5 INSERTs instead of 500 — a 100× reduction in
            // SQL round-trips.
            use crate::store::helpers::sql::max_rows_per_statement;
            const BATCH_SIZE: usize = max_rows_per_statement(3);
            for batch_start in (0..updates.len()).step_by(BATCH_SIZE) {
                let batch_end = (batch_start + BATCH_SIZE).min(updates.len());
                let batch = &updates[batch_start..batch_end];
                let batch_bytes = &embedding_bytes[batch_start..batch_end];

                let mut placeholders = Vec::with_capacity(batch.len());
                for i in 0..batch.len() {
                    let base = i * 3;
                    placeholders.push(format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3));
                }
                let sql = format!(
                    "INSERT INTO _update_embeddings (id, embedding, enrichment_hash) VALUES {}",
                    placeholders.join(", ")
                );
                let mut query = sqlx::query(&sql);
                for (i, (id, _, hash)) in batch.iter().enumerate() {
                    query = query.bind(id);
                    query = query.bind(&batch_bytes[i]);
                    query = query.bind(hash.as_deref());
                }
                query.execute(&mut *tx).await?;
            }

            // 3. Single UPDATE...FROM join (SQLite 3.33+)
            let result = sqlx::query(
                "UPDATE chunks SET \
                    embedding = t.embedding, \
                    enrichment_hash = COALESCE(t.enrichment_hash, chunks.enrichment_hash) \
                 FROM _update_embeddings t \
                 WHERE chunks.id = t.id",
            )
            .execute(&mut *tx)
            .await?;
            let updated = result.rows_affected() as usize;

            if updated < updates.len() {
                let missing = updates.len() - updated;
                tracing::debug!(missing, "Enrichment update: some chunk IDs not found");
            }

            // 4. Drop temp table
            sqlx::query("DROP TABLE IF EXISTS _update_embeddings")
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(updated)
        })
    }

    /// Insert or update LLM summaries in batch.
    ///
    /// Each entry is (content_hash, summary, model, purpose).
    pub fn upsert_summaries_batch(
        &self,
        summaries: &[(String, String, String, String)],
    ) -> Result<usize, StoreError> {
        let _span =
            tracing::debug_span!("upsert_summaries_batch", count = summaries.len()).entered();
        if summaries.is_empty() {
            return Ok(0);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            use crate::store::helpers::sql::max_rows_per_statement;
            const BATCH_SIZE: usize = max_rows_per_statement(5);
            for batch in summaries.chunks(BATCH_SIZE) {
                let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT OR REPLACE INTO llm_summaries (content_hash, summary, model, purpose, created_at)",
                );
                qb.push_values(
                    batch.iter(),
                    |mut b, (hash, summary, model, purpose)| {
                        b.push_bind(hash)
                            .push_bind(summary)
                            .push_bind(model)
                            .push_bind(purpose)
                            .push_bind(&now);
                    },
                );
                qb.build().execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(summaries.len())
        })
    }

    /// Delete orphan LLM summaries whose content_hash doesn't exist in any chunk.
    pub fn prune_orphan_summaries(&self) -> Result<usize, StoreError> {
        let _span = tracing::debug_span!("prune_orphan_summaries").entered();
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

    /// Delete all chunks for an origin (file path or source identifier)
    pub fn delete_by_origin(&self, origin: &Path) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("delete_by_origin", origin = %origin.display()).entered();
        let origin_str = crate::normalize_path(origin);

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

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

            // E.2 (P1 #17): `function_calls` has no FK to `chunks` (it stores
            // `caller_name` strings, not chunk IDs), so deleting chunks does
            // not cascade. Without this DELETE, every incremental delete path
            // leaves orphan call-graph rows that surface as ghost callers in
            // `cqs callers`/`callees`/`dead`.
            sqlx::query("DELETE FROM function_calls WHERE file = ?1")
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
        let _span = tracing::info_span!(
            "upsert_chunks_and_calls",
            chunks = chunks.len(),
            calls = calls.len()
        )
        .entered();
        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(&mut tx, chunks, &embedding_bytes, source_mtime, &now).await?;
            upsert_fts_conditional(&mut tx, chunks, &old_hashes).await?;

            // Upsert calls: delete old calls for these chunk IDs, insert new ones
            if !calls.is_empty() {
                // Batch DELETE: collect unique caller IDs, delete in batches of 500
                let unique_ids: Vec<&str> = {
                    let mut seen = std::collections::HashSet::new();
                    calls
                        .iter()
                        .filter_map(|(id, _)| {
                            if seen.insert(id.as_str()) {
                                Some(id.as_str())
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                for batch in
                    unique_ids.chunks(crate::store::helpers::sql::max_rows_per_statement(1))
                {
                    let placeholders: String = batch
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 1))
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!("DELETE FROM calls WHERE caller_id IN ({})", placeholders);
                    let mut query = sqlx::query(&sql);
                    for id in batch {
                        query = query.bind(*id);
                    }
                    query.execute(&mut *tx).await?;
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

    /// Delete chunks for a file that are no longer in the current parse output (RT-DATA-10).
    ///
    /// After re-parsing a file, some functions may have been removed. Their old
    /// chunks would linger as phantoms. This deletes chunks whose origin matches
    /// `file` but whose ID is not in `live_ids`.
    pub fn delete_phantom_chunks(
        &self,
        file: &std::path::Path,
        live_ids: &[&str],
    ) -> Result<u32, StoreError> {
        let _span =
            tracing::info_span!("delete_phantom_chunks", ?file, live_count = live_ids.len())
                .entered();
        let origin_str = crate::normalize_path(file);
        if live_ids.is_empty() {
            // No live chunks means the whole file was emptied/deleted —
            // delete_by_origin handles that case.
            return self.delete_by_origin(file);
        }

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // Use a temp table to avoid SQLite's 999-parameter limit.
            // A file can have 1000+ chunks (e.g., large generated files).
            sqlx::query("CREATE TEMP TABLE IF NOT EXISTS _live_ids (id TEXT PRIMARY KEY)")
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM _live_ids")
                .execute(&mut *tx)
                .await?;

            for batch in live_ids.chunks(crate::store::helpers::sql::max_rows_per_statement(1)) {
                let placeholders: Vec<String> = batch
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("(?{})", i + 1))
                    .collect();
                let insert_sql = format!(
                    "INSERT OR IGNORE INTO _live_ids (id) VALUES {}",
                    placeholders.join(",")
                );
                let mut stmt = sqlx::query(&insert_sql);
                for id in batch {
                    stmt = stmt.bind(id);
                }
                stmt.execute(&mut *tx).await?;
            }

            let fts_query =
                "DELETE FROM chunks_fts WHERE id IN \
                 (SELECT id FROM chunks WHERE origin = ?1 AND id NOT IN (SELECT id FROM _live_ids))";
            sqlx::query(fts_query)
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            let chunks_query =
                "DELETE FROM chunks WHERE origin = ?1 AND id NOT IN (SELECT id FROM _live_ids)";
            let result = sqlx::query(chunks_query)
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            // NOTE on `function_calls` cleanup: `delete_phantom_chunks` is
            // called by `cli/watch.rs` AFTER `upsert_function_calls`
            // (`watch.rs:2325`) for the same file, which already DELETE-then-
            // INSERTs the current set. Adding a `DELETE FROM function_calls`
            // here would wipe those just-written rows. The other two delete
            // paths (`delete_by_origin`, `prune_missing`) — file removed
            // entirely — DO have an explicit `function_calls` DELETE because
            // no upsert follows; see those functions.
            tx.commit().await?;
            let deleted = result.rows_affected() as u32;
            if deleted > 0 {
                tracing::info!(origin = %origin_str, deleted, "Removed phantom chunks");
            }
            Ok(deleted)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_utils::make_chunk;
    use crate::test_helpers::{mock_embedding, setup_store};

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

    // ===== TC-8: LLM summary functions =====

    #[test]
    fn test_get_summaries_empty_input() {
        let (store, _dir) = setup_store();
        let result = store.get_summaries_by_hashes(&[], "summary").unwrap();
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
                "summary".to_string(),
            ),
            (
                "hash_b".to_string(),
                "summary B".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
            (
                "hash_c".to_string(),
                "summary C".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let result = store
            .get_summaries_by_hashes(&["hash_a", "hash_b", "hash_c"], "summary")
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
            .get_summaries_by_hashes(&["nonexistent_1", "nonexistent_2"], "summary")
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_summaries_mixed() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            (
                "h1".to_string(),
                "s1".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "h2".to_string(),
                "s2".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "h3".to_string(),
                "s3".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        // Query 5 hashes, only 3 exist
        let result = store
            .get_summaries_by_hashes(&["h1", "h2", "h3", "h4", "h5"], "summary")
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
            .upsert_summaries_batch(&[(
                "h1".to_string(),
                "first".to_string(),
                "m".to_string(),
                "summary".to_string(),
            )])
            .unwrap();
        store
            .upsert_summaries_batch(&[(
                "h1".to_string(),
                "second".to_string(),
                "m".to_string(),
                "summary".to_string(),
            )])
            .unwrap();

        let result = store.get_summaries_by_hashes(&["h1"], "summary").unwrap();
        assert_eq!(result["h1"], "second");
    }

    #[test]
    fn test_get_all_summaries_empty() {
        let (store, _dir) = setup_store();
        let result = store.get_all_summaries("summary").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_all_summaries_all() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            (
                "ha".to_string(),
                "sa".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "hb".to_string(),
                "sb".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "hc".to_string(),
                "sc".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let all = store.get_all_summaries("summary").unwrap();
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
            (
                c1.content_hash,
                "summary a".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                c2.content_hash,
                "summary b".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let pruned = store.prune_orphan_summaries().unwrap();
        assert_eq!(pruned, 0);

        // All summaries survive
        let all = store.get_all_summaries("summary").unwrap();
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
                "summary".to_string(),
            ),
            (
                "orphan_hash_1".to_string(),
                "orphan 1".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "orphan_hash_2".to_string(),
                "orphan 2".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();
        assert_eq!(store.get_all_summaries("summary").unwrap().len(), 3);

        let pruned = store.prune_orphan_summaries().unwrap();
        assert_eq!(pruned, 2);

        let remaining = store.get_all_summaries("summary").unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(&c1.content_hash));
    }

    // ===== TC-SQ8: purpose coexistence =====

    #[test]
    fn test_summaries_different_purposes_coexist() {
        let (store, _dir) = setup_store();

        // Insert same content_hash with two different purposes
        let summaries = vec![
            (
                "shared_hash".to_string(),
                "This function parses config files.".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
            (
                "shared_hash".to_string(),
                "/// Parses configuration from TOML files.\n/// Returns a Config struct."
                    .to_string(),
                "model-1".to_string(),
                "doc-comment".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        // Each purpose returns only its own entry
        let by_summary = store
            .get_summaries_by_hashes(&["shared_hash"], "summary")
            .unwrap();
        assert_eq!(by_summary.len(), 1);
        assert_eq!(
            by_summary["shared_hash"],
            "This function parses config files."
        );

        let by_doc = store
            .get_summaries_by_hashes(&["shared_hash"], "doc-comment")
            .unwrap();
        assert_eq!(by_doc.len(), 1);
        assert!(by_doc["shared_hash"].contains("Parses configuration"));

        // get_all_summaries also filters by purpose
        let all_summary = store.get_all_summaries("summary").unwrap();
        assert_eq!(all_summary.len(), 1);
        let all_doc = store.get_all_summaries("doc-comment").unwrap();
        assert_eq!(all_doc.len(), 1);
    }

    // ===== delete_phantom_chunks tests (TC-42) =====

    #[test]
    fn delete_phantom_chunks_removes_stale() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file.rs");
        let c2 = make_chunk("b", "file.rs");
        let c3 = make_chunk("c", "file.rs");
        let id1 = c1.id.clone();
        let id2 = c2.id.clone();
        store
            .upsert_chunks_batch(
                &[(c1, emb.clone()), (c2, emb.clone()), (c3, emb.clone())],
                Some(100),
            )
            .unwrap();

        // "c" was removed from the file
        let live: Vec<&str> = vec![id1.as_str(), id2.as_str()];
        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file.rs"), &live)
            .unwrap();
        assert_eq!(deleted, 1, "Should delete one phantom chunk");
        assert_eq!(store.chunk_count().unwrap(), 2);
    }

    #[test]
    fn delete_phantom_chunks_empty_live_ids_deletes_all() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file.rs");
        let c2 = make_chunk("b", "file.rs");
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb.clone())], Some(100))
            .unwrap();

        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file.rs"), &[])
            .unwrap();
        assert_eq!(
            deleted, 2,
            "Empty live_ids should delete all chunks for file"
        );
    }

    #[test]
    fn delete_phantom_chunks_no_phantoms() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file.rs");
        let id1 = c1.id.clone();
        store.upsert_chunks_batch(&[(c1, emb)], Some(100)).unwrap();

        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file.rs"), &[id1.as_str()])
            .unwrap();
        assert_eq!(deleted, 0, "No phantoms to delete");
    }

    #[test]
    fn delete_phantom_chunks_wrong_file_unaffected() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file1.rs");
        let c2 = make_chunk("b", "file2.rs");
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb)], Some(100))
            .unwrap();

        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file1.rs"), &[])
            .unwrap();
        assert_eq!(deleted, 1, "Should only delete file1.rs chunks");
        assert_eq!(
            store.chunk_count().unwrap(),
            1,
            "file2.rs chunk should remain"
        );
    }
}
