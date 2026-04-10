//! Async fetch helpers, batch insert, and EmbeddingBatchIterator.

use std::collections::HashMap;

use sqlx::Row;

use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::Chunk;
use crate::store::helpers::{bytes_to_embedding, CandidateRow, ChunkRow, StoreError};
use crate::store::Store;

impl Store {
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
            let placeholders = crate::store::helpers::make_placeholders(batch.len());
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
            let placeholders = crate::store::helpers::make_placeholders(batch.len());
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

        let placeholders = crate::store::helpers::make_placeholders(ids.len());
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
            column: "embedding",
        }
    }

    /// Stream `embedding_base` rows in batches for the Phase 5 dual HNSW build.
    ///
    /// Rows with NULL `embedding_base` are skipped — that happens after the
    /// v17→v18 migration has added the column but before the next index pass
    /// has re-populated it. The HNSW builder treats these as "not yet available"
    /// and the router silently falls back to the enriched index.
    ///
    /// Sync-only: internally uses `block_on`. Call from the same context as
    /// [`Store::embedding_batches`].
    pub fn embedding_base_batches(
        &self,
        batch_size: usize,
    ) -> impl Iterator<Item = Result<Vec<(String, Embedding)>, StoreError>> + '_ {
        let _span =
            tracing::debug_span!("embedding_base_batches", batch_size = batch_size).entered();
        EmbeddingBatchIterator {
            store: self,
            batch_size,
            last_rowid: 0,
            done: false,
            column: "embedding_base",
        }
    }
}

// ── Shared async helpers for chunk upsert (PERF-3) ──────────────────────────

/// Snapshot existing content hashes before INSERT overwrites them.
/// Batched in groups of 500 to stay within SQLite's 999-param limit.
pub(super) async fn snapshot_content_hashes(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
) -> Result<HashMap<String, String>, StoreError> {
    const HASH_BATCH: usize = 500;
    let mut old_hashes = HashMap::new();
    let chunk_ids: Vec<&str> = chunks.iter().map(|(c, _)| c.id.as_str()).collect();
    for id_batch in chunk_ids.chunks(HASH_BATCH) {
        let placeholders = crate::store::helpers::make_placeholders(id_batch.len());
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

/// Batch INSERT chunks (49 rows × 20 params = 980 < SQLite's 999 limit).
///
/// Uses `ON CONFLICT(id) DO UPDATE` (upsert) instead of `INSERT OR REPLACE`
/// to preserve `enrichment_hash` and `enrichment_version` columns that are
/// set by the enrichment pass. `INSERT OR REPLACE` deletes and re-inserts the
/// row, wiping those columns back to NULL/default (DS-2).
///
/// The WHERE clause on content_hash skips the UPDATE when the content is
/// unchanged, avoiding unnecessary write amplification.
///
/// Phase 5 (v18): `embedding_base` is seeded with the same bytes as `embedding`
/// on initial insert. The incoming embedding was generated from
/// `generate_nl_description` (raw NL, no enrichment), so it IS the base.
/// The enrichment pass later overwrites only `embedding`, leaving
/// `embedding_base` intact as the dual-index target.
pub(super) async fn batch_insert_chunks(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
    embedding_bytes: &[Vec<u8>],
    source_mtime: Option<i64>,
    now: &str,
) -> Result<(), StoreError> {
    const CHUNK_INSERT_BATCH: usize = 49;
    for (batch_idx, batch) in chunks.chunks(CHUNK_INSERT_BATCH).enumerate() {
        let emb_offset = batch_idx * CHUNK_INSERT_BATCH;
        let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
            "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, embedding_base, source_mtime, created_at, updated_at, parent_id, window_idx, parent_type_name)",
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
                // Phase 5 (v18): seed embedding_base with the same bytes. The
                // incoming embedding comes from raw NL (no enrichment), so on
                // initial insert it IS the base. Enrichment pass later updates
                // `embedding` only; `embedding_base` stays put for dual-index.
                .push_bind(&embedding_bytes[emb_offset + i])
                .push_bind(source_mtime)
                .push_bind(now)
                .push_bind(now)
                .push_bind(&chunk.parent_id)
                .push_bind(chunk.window_idx.map(|i| i as i64))
                .push_bind(&chunk.parent_type_name);
        });
        // DS-2: ON CONFLICT upsert preserves enrichment_hash and enrichment_version.
        // Only update when content_hash changed (avoids write amplification for unchanged chunks).
        //
        // Phase 5: on content change, refresh embedding_base too — new content
        // means new NL text means new base embedding. Reindex sets both columns.
        qb.push(
            " ON CONFLICT(id) DO UPDATE SET \
             origin=excluded.origin, \
             source_type=excluded.source_type, \
             language=excluded.language, \
             chunk_type=excluded.chunk_type, \
             name=excluded.name, \
             signature=excluded.signature, \
             content=excluded.content, \
             content_hash=excluded.content_hash, \
             doc=excluded.doc, \
             line_start=excluded.line_start, \
             line_end=excluded.line_end, \
             embedding=excluded.embedding, \
             embedding_base=excluded.embedding_base, \
             source_mtime=excluded.source_mtime, \
             updated_at=excluded.updated_at, \
             parent_id=excluded.parent_id, \
             window_idx=excluded.window_idx, \
             parent_type_name=excluded.parent_type_name \
             WHERE chunks.content_hash != excluded.content_hash",
        );
        qb.build().execute(&mut **tx).await?;
    }
    Ok(())
}

/// Conditional FTS upsert: skip if content_hash unchanged (compared to pre-INSERT snapshot).
/// Batches DELETE and INSERT for efficiency (PERF-2: was 2 SQL per chunk, now batched).
pub(super) async fn upsert_fts_conditional(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
    old_hashes: &HashMap<String, String>,
) -> Result<(), StoreError> {
    // Collect changed chunks
    let changed: Vec<&Chunk> = chunks
        .iter()
        .filter_map(|(chunk, _)| {
            let content_changed = old_hashes
                .get(&chunk.id)
                .map(|old_hash| old_hash != &chunk.content_hash)
                .unwrap_or(true);
            if content_changed {
                Some(chunk)
            } else {
                None
            }
        })
        .collect();

    if changed.is_empty() {
        return Ok(());
    }

    // Batch DELETE: remove old FTS entries for changed chunks (PF-8: reuse make_placeholders)
    for batch in changed.chunks(500) {
        let placeholders = crate::store::helpers::make_placeholders(batch.len());
        let sql = format!("DELETE FROM chunks_fts WHERE id IN ({})", placeholders);
        let mut query = sqlx::query(&sql);
        for chunk in batch {
            query = query.bind(&chunk.id);
        }
        query.execute(&mut **tx).await?;
    }

    // Batch INSERT: add new FTS entries
    for batch in changed.chunks(180) {
        // 180 chunks × 5 bind params = 900, under SQLite 999 limit
        let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> =
            sqlx::QueryBuilder::new("INSERT INTO chunks_fts (id, name, signature, content, doc) ");
        qb.push_values(batch.iter(), |mut b, chunk| {
            b.push_bind(&chunk.id)
                .push_bind(normalize_for_fts(&chunk.name))
                .push_bind(normalize_for_fts(&chunk.signature))
                .push_bind(normalize_for_fts(&chunk.content))
                .push_bind(
                    chunk
                        .doc
                        .as_ref()
                        .map(|d| normalize_for_fts(d))
                        .unwrap_or_default(),
                );
        });
        qb.build().execute(&mut **tx).await?;
    }

    Ok(())
}

/// Iterator for streaming embeddings in batches using cursor-based pagination.
///
/// `column` selects which BLOB column to read: `"embedding"` (enriched, the
/// default) or `"embedding_base"` (Phase 5 dual index). Rows where the
/// selected column is NULL are silently skipped — NULL is valid state for
/// `embedding_base` between the v17→v18 migration and the next index pass.
struct EmbeddingBatchIterator<'a> {
    store: &'a Store,
    batch_size: usize,
    /// Last seen rowid for cursor-based pagination
    last_rowid: i64,
    done: bool,
    column: &'static str,
}

impl<'a> Iterator for EmbeddingBatchIterator<'a> {
    type Item = Result<Vec<(String, Embedding)>, StoreError>;

    /// Advances the iterator to the next batch of embedding records from the database.
    ///
    /// Fetches a batch of chunks from the database ordered by rowid, deserializes their embeddings, and returns them as a vector of (id, embedding) pairs. Automatically handles pagination by tracking the last rowid and fetching subsequent batches on subsequent calls. Skips batches where all rows fail deserialization and continues to the next batch.
    ///
    /// # Returns
    ///
    /// `Option<Result<Vec<(String, Embedding)>, Error>>` - Some(Ok(batch)) with the next batch of embeddings, Some(Err(e)) if a database error occurs, or None when all records have been consumed.
    ///
    /// # Errors
    ///
    /// Returns a database error if the query fails or if the connection pool encounters an error.
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }

            // Allow-list `column` — it's a `&'static str` set by the
            // constructor to one of two known values, never from user input,
            // but we still panic on anything else to keep this safe.
            let col = match self.column {
                "embedding" => "embedding",
                "embedding_base" => "embedding_base",
                other => panic!("EmbeddingBatchIterator: invalid column {other:?}"),
            };

            let result = self.store.rt.block_on(async {
                let sql = format!(
                    "SELECT rowid, id, {col} FROM chunks \
                     WHERE rowid > ?1 AND {col} IS NOT NULL \
                     ORDER BY rowid ASC LIMIT ?2"
                );
                let rows: Vec<_> = sqlx::query(&sql)
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
                        bytes_to_embedding(&bytes, self.store.dim)
                            .ok()
                            .map(|emb| (id, Embedding::new(emb)))
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
    use super::super::test_utils::make_chunk;
    use crate::test_helpers::{mock_embedding, setup_store};

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

    // ===== embedding_base_batches tests (Phase 5) =====

    /// Fresh inserts populate both `embedding` and `embedding_base` with the
    /// same bytes (the incoming embedding comes from raw NL, which IS the base).
    #[test]
    fn test_embedding_base_batches_populated_on_insert() {
        let (store, _dir) = setup_store();

        let pairs: Vec<_> = (0..5)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32 + 0.1))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        let enriched: Vec<_> = store
            .embedding_batches(100)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        let base: Vec<_> = store
            .embedding_base_batches(100)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();

        assert_eq!(enriched.len(), 5);
        assert_eq!(base.len(), 5);

        // On fresh insert the two columns must be byte-identical.
        for ((id_a, emb_a), (id_b, emb_b)) in enriched.iter().zip(base.iter()) {
            assert_eq!(id_a, id_b);
            assert_eq!(emb_a.as_slice(), emb_b.as_slice());
        }
    }

    /// NULL embedding_base rows (simulating a fresh v17→v18 migration before
    /// the next index pass) are silently skipped by `embedding_base_batches`.
    /// `embedding_batches` still yields them because `embedding` is NOT NULL.
    #[test]
    fn test_embedding_base_batches_skips_null_rows() {
        let (store, _dir) = setup_store();

        let pairs: Vec<_> = (0..4)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32 + 0.1))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Simulate a mid-migration state: NULL out embedding_base for 2 rows.
        store
            .rt
            .block_on(async {
                sqlx::query(
                    "UPDATE chunks SET embedding_base = NULL WHERE name IN ('fn_1', 'fn_3')",
                )
                .execute(&store.pool)
                .await
            })
            .unwrap();

        let enriched_count: usize = store
            .embedding_batches(100)
            .filter_map(|b| b.ok())
            .map(|b| b.len())
            .sum();
        let base_count: usize = store
            .embedding_base_batches(100)
            .filter_map(|b| b.ok())
            .map(|b| b.len())
            .sum();

        assert_eq!(enriched_count, 4, "enriched column unchanged");
        assert_eq!(base_count, 2, "base iterator skips NULL rows");
    }

    #[test]
    fn test_embedding_base_batches_empty_store() {
        let (store, _dir) = setup_store();
        let batches: Vec<_> = store.embedding_base_batches(10).collect();
        assert!(batches.is_empty());
    }

    /// Phase 5 invariant: the enrichment pass (`update_embeddings_batch` /
    /// `update_embeddings_with_hashes_batch`) writes ONLY to the `embedding`
    /// column. `embedding_base` must survive the enrichment update untouched
    /// — that's what makes dual indexing meaningful, since otherwise both
    /// columns would converge after the first enrichment cycle.
    #[test]
    fn test_enrichment_does_not_overwrite_base() {
        let (store, _dir) = setup_store();

        // Insert one chunk; both columns now hold the same base bytes.
        let chunk = make_chunk("victim", "src/victim.rs");
        let original_base = mock_embedding(0.42);
        store
            .upsert_chunks_batch(&[(chunk.clone(), original_base.clone())], Some(100))
            .unwrap();

        let base_before: Vec<_> = store
            .embedding_base_batches(10)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        assert_eq!(base_before.len(), 1);

        // Simulate enrichment: rewrite `embedding` with a totally different
        // vector (the "enriched" embedding) and a fresh enrichment_hash.
        let enriched = mock_embedding(99.0);
        let updates = vec![(
            chunk.id.clone(),
            enriched.clone(),
            Some("enrichment-hash-v1".to_string()),
        )];
        let updated = store.update_embeddings_with_hashes_batch(&updates).unwrap();
        assert_eq!(updated, 1);

        // After enrichment: `embedding` reflects the enriched vector, but
        // `embedding_base` is byte-identical to the original.
        let enriched_after: Vec<_> = store
            .embedding_batches(10)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        let base_after: Vec<_> = store
            .embedding_base_batches(10)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        assert_eq!(enriched_after.len(), 1);
        assert_eq!(base_after.len(), 1);
        // `embedding` was overwritten by enrichment.
        assert_eq!(enriched_after[0].1.as_slice(), enriched.as_slice());
        // `embedding_base` survived untouched — this is the dual-indexing invariant.
        assert_eq!(base_after[0].1.as_slice(), original_base.as_slice());
    }

    /// Phase 5: when content changes, the re-upsert path must refresh BOTH
    /// columns (new content → new base NL → new base embedding). The
    /// ON CONFLICT clause includes `embedding_base = excluded.embedding_base`
    /// to enforce this.
    #[test]
    fn test_content_change_refreshes_both_columns() {
        let (store, _dir) = setup_store();

        // First insert: original content, original embedding bytes.
        let mut chunk = make_chunk("evolving", "src/evolving.rs");
        let original = mock_embedding(0.1);
        store
            .upsert_chunks_batch(&[(chunk.clone(), original.clone())], Some(100))
            .unwrap();

        // Mutate the chunk: new content + new content_hash → triggers the
        // ON CONFLICT WHERE clause (only updates rows where hash changed).
        chunk.content = "fn evolving() { /* changed */ }".to_string();
        chunk.content_hash = "new-hash-v2".to_string();
        let new_embedding = mock_embedding(0.9);

        store
            .upsert_chunks_batch(&[(chunk.clone(), new_embedding.clone())], Some(200))
            .unwrap();

        // Both columns must reflect the new bytes.
        let enriched: Vec<_> = store
            .embedding_batches(10)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        let base: Vec<_> = store
            .embedding_base_batches(10)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        assert_eq!(enriched.len(), 1);
        assert_eq!(base.len(), 1);
        assert_eq!(enriched[0].1.as_slice(), new_embedding.as_slice());
        assert_eq!(base[0].1.as_slice(), new_embedding.as_slice());
    }
}
