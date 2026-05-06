//! Async fetch helpers, batch insert, and EmbeddingBatchIterator.

use std::collections::HashMap;

use sqlx::Row;

use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::Chunk;
use crate::store::helpers::{bytes_to_embedding, CandidateRow, ChunkRow, StoreError};
use crate::store::Store;

impl<Mode> Store<Mode> {
    /// Fetch chunks by IDs (without embeddings) — async version.
    ///
    /// Returns a map of chunk ID → ChunkRow for the given IDs.
    /// Used by search to hydrate top-N results after scoring.
    /// PF-V1.29-2: batch size derives from the modern SQLite variable limit
    /// (`max_rows_per_statement(1)`) — prior `500` was sized for the
    /// pre-3.32 999-variable ceiling.
    pub(crate) async fn fetch_chunks_by_ids_async(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, ChunkRow>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = crate::store::helpers::sql::max_rows_per_statement(1);
        let mut result = HashMap::with_capacity(ids.len());

        for batch in ids.chunks(BATCH_SIZE) {
            let placeholders = crate::store::helpers::make_placeholders(batch.len());
            let sql = format!(
                "SELECT {cols} FROM chunks WHERE id IN ({placeholders})",
                cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
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
    /// PF-V1.29-2: batch size derives from `max_rows_per_statement(1)` —
    /// prior `500` was sized for the pre-3.32 999-variable ceiling.
    pub(crate) async fn fetch_candidates_by_ids_async(
        &self,
        ids: &[&str],
    ) -> Result<Vec<(CandidateRow, Vec<u8>)>, StoreError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        const BATCH_SIZE: usize = crate::store::helpers::sql::max_rows_per_statement(1);

        // PF-V1.25-6: previously built rows in DB order, then `sort_by_key`
        // (O(N log N)) using a HashMap<&str, usize> position index to
        // restore caller order. Drop the sort entirely by writing each row
        // directly into its caller-ordered slot:
        //   1. Build id→position map once (same cost as before, but now
        //      amortizes the whole function instead of paying it twice).
        //   2. Allocate `Vec<Option<T>>` sized to `ids.len()`.
        //   3. For each DB row, look up its position and place into slot.
        //   4. `flatten()` drops `None` slots (ids not found in DB) while
        //      preserving caller order.
        // Tie-break semantics for downstream sorts are unchanged: the
        // returned Vec has caller-provided ordering exactly as before.
        let id_pos: std::collections::HashMap<&str, usize> =
            ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let mut positioned: Vec<Option<(CandidateRow, Vec<u8>)>> =
            (0..ids.len()).map(|_| None).collect();

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

            for r in &rows {
                let candidate = CandidateRow::from_row(r);
                let embedding_bytes: Vec<u8> = r.get("embedding");
                if let Some(&pos) = id_pos.get(candidate.id.as_str()) {
                    positioned[pos] = Some((candidate, embedding_bytes));
                }
            }
        }

        // Compact `None` slots (caller ids absent from DB) while keeping
        // the caller-provided order for the remainder.
        Ok(positioned.into_iter().flatten().collect())
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
        // PERF: pinned columns first so ChunkRow::from_row ordinals stay stable;
        // `embedding` appended after (read by index 16 below).
        let sql = format!(
            "SELECT {cols}, embedding FROM chunks WHERE id IN ({placeholders})",
            cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
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

    /// Stream `(id, embedding, content_hash)` triples in batches.
    ///
    /// Same cursor-paginated single-pass shape as [`Store::embedding_batches`],
    /// but each row also carries the chunk's `content_hash`. This is the
    /// snapshot path used by the background HNSW rebuild thread (see #1124):
    /// the rebuild needs both the vector (to feed HNSW build) and the hash
    /// it was built from (so the swap-time drain can detect entries that
    /// were re-embedded mid-rebuild and replay the fresh vector instead of
    /// dropping it as a "duplicate id").
    ///
    /// Single SQL pass — `embedding` and `content_hash` come from the same
    /// row read so they're consistent under concurrent writers (WAL snapshot
    /// isolation only holds within a transaction).
    ///
    /// Sync-only: internally uses `block_on`. Call from the same context as
    /// [`Store::embedding_batches`].
    pub fn embedding_and_hash_batches(
        &self,
        batch_size: usize,
    ) -> impl Iterator<Item = Result<Vec<(String, Embedding, String)>, StoreError>> + '_ {
        let _span =
            tracing::debug_span!("embedding_and_hash_batches", batch_size = batch_size).entered();
        EmbeddingHashBatchIterator {
            store: self,
            batch_size,
            last_rowid: 0,
            done: false,
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

/// Pre-INSERT snapshot of fields used by the `ON CONFLICT DO UPDATE WHERE`
/// short-circuit and by `upsert_fts_conditional`'s "did anything actually
/// change?" filter.
///
/// `content_hash` is the original PERF-3 / FTS skip key. `parser_version` was
/// added in v1.28.0 audit P2 #29 so chunks whose source bytes are unchanged
/// but whose parser logic moved on (e.g. `extract_doc_fallback_for_short_chunk`
/// in PR #1040) still trigger a refresh.
#[derive(Clone)]
pub(super) struct ChunkSnapshot {
    pub content_hash: String,
    pub parser_version: u32,
}

/// Snapshot existing content_hash + parser_version before INSERT overwrites
/// them. Single-bind IN-list batched at the modern SQLite variable limit.
pub(super) async fn snapshot_content_hashes(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
) -> Result<HashMap<String, ChunkSnapshot>, StoreError> {
    use crate::store::helpers::sql::max_rows_per_statement;
    const HASH_BATCH: usize = max_rows_per_statement(1);
    let mut old: HashMap<String, ChunkSnapshot> = HashMap::new();
    let chunk_ids: Vec<&str> = chunks.iter().map(|(c, _)| c.id.as_str()).collect();
    for id_batch in chunk_ids.chunks(HASH_BATCH) {
        let placeholders = crate::store::helpers::make_placeholders(id_batch.len());
        let sql = format!(
            "SELECT id, content_hash, parser_version FROM chunks WHERE id IN ({})",
            placeholders
        );
        let mut q = sqlx::query_as::<_, (String, String, i64)>(&sql);
        for id in id_batch {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(&mut **tx).await?;
        for (id, hash, pv) in rows {
            old.insert(
                id,
                ChunkSnapshot {
                    content_hash: hash,
                    // Stored as INTEGER NOT NULL DEFAULT 0; cast back to u32.
                    // Negative values shouldn't occur but clamp defensively.
                    parser_version: pv.max(0).min(u32::MAX as i64) as u32,
                },
            );
        }
    }
    Ok(old)
}

/// Batch INSERT chunks — derived from the modern SQLite variable limit.
///
/// v1.22.0 audit SHL-32: the previous `CHUNK_INSERT_BATCH = 49` (49 × 20
/// params = 980) was sized for the pre-3.32 SQLite 999-variable limit. On
/// a 12k-chunk reindex this produced ~245 INSERT statements. The modern
/// limit (32766) permits ~1622 rows per statement, reducing to ~8 INSERTs
/// for the same corpus — a 30× reduction in SQL round-trips.
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
    vendored_per_chunk: &[bool],
    source_mtime: Option<i64>,
    now: &str,
) -> Result<(), StoreError> {
    use crate::store::helpers::sql::max_rows_per_statement;
    // 22 binds per row (v24 / #1221: vendored added after parser_version).
    const CHUNK_INSERT_BATCH: usize = max_rows_per_statement(22);
    debug_assert_eq!(
        chunks.len(),
        vendored_per_chunk.len(),
        "vendored_per_chunk must align 1:1 with chunks"
    );
    for (batch_idx, batch) in chunks.chunks(CHUNK_INSERT_BATCH).enumerate() {
        let emb_offset = batch_idx * CHUNK_INSERT_BATCH;
        let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
            "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, signature, content, content_hash, doc, line_start, line_end, embedding, embedding_base, source_mtime, created_at, updated_at, parent_id, window_idx, parent_type_name, parser_version, vendored)",
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
                .push_bind(&chunk.parent_type_name)
                // P2 #29: stamp the parser version emitted with this chunk so
                // a parser-logic bump (without source change) still refreshes.
                .push_bind(chunk.parser_version as i64)
                // v24 / #1221: vendored bit precomputed by `upsert_chunks_batch`
                // from the chunk's origin and the store's configured prefix list.
                .push_bind(if vendored_per_chunk[emb_offset + i] {
                    1_i64
                } else {
                    0_i64
                });
        });
        // DS-2: ON CONFLICT upsert preserves enrichment_hash and enrichment_version.
        //
        // Skip the UPDATE only when BOTH content_hash and parser_version are
        // identical to the existing row. P2 #29: a parser-logic bump (e.g.
        // PR #1040's doc fallback) needs to refresh `doc` even though
        // `content_hash` is unchanged. The OR clause handles that case.
        //
        // Phase 5: on content change, refresh embedding_base too — new content
        // means new NL text means new base embedding. Reindex sets both columns.
        // P1.15: invalidate UMAP coords on content change. The cluster view
        // filters `WHERE umap_x IS NOT NULL` to find chunks needing reprojection;
        // without nulling here, a re-embedded chunk renders at its old position
        // until `cqs index --umap` is rerun. Parser-version-only bumps do NOT
        // change the embedding, so the CASE preserves coords on that branch.
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
             parent_type_name=excluded.parent_type_name, \
             parser_version=excluded.parser_version, \
             vendored=excluded.vendored, \
             umap_x=CASE WHEN chunks.content_hash != excluded.content_hash \
                         THEN NULL ELSE chunks.umap_x END, \
             umap_y=CASE WHEN chunks.content_hash != excluded.content_hash \
                         THEN NULL ELSE chunks.umap_y END \
             WHERE chunks.content_hash != excluded.content_hash \
                OR chunks.parser_version != excluded.parser_version \
                OR chunks.vendored != excluded.vendored",
        );
        qb.build().execute(&mut **tx).await?;
    }
    Ok(())
}

/// Conditional FTS upsert: skip if content_hash AND parser_version are
/// unchanged (compared to pre-INSERT snapshot).
///
/// P2 #29: the parser_version OR mirrors the UPSERT WHERE filter in
/// `batch_insert_chunks`. A parser bump that updates `doc` without touching
/// source bytes still needs the FTS row refreshed — `normalize_for_fts` is
/// applied to `doc` and the FTS would otherwise serve stale text.
///
/// Batches DELETE and INSERT for efficiency (PERF-2: was 2 SQL per chunk, now batched).
pub(super) async fn upsert_fts_conditional(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    chunks: &[(Chunk, Embedding)],
    old: &HashMap<String, ChunkSnapshot>,
) -> Result<(), StoreError> {
    // Collect changed chunks (content OR parser_version differs vs. snapshot)
    let changed: Vec<&Chunk> = chunks
        .iter()
        .filter_map(|(chunk, _)| {
            let chunk_changed = old
                .get(&chunk.id)
                .map(|snap| {
                    snap.content_hash != chunk.content_hash
                        || snap.parser_version != chunk.parser_version
                })
                // No snapshot → row is brand-new → always insert.
                .unwrap_or(true);
            if chunk_changed {
                Some(chunk)
            } else {
                None
            }
        })
        .collect();

    if changed.is_empty() {
        return Ok(());
    }

    use crate::store::helpers::sql::max_rows_per_statement;
    // Batch DELETE: remove old FTS entries for changed chunks (PF-8: reuse make_placeholders)
    for batch in changed.chunks(max_rows_per_statement(1)) {
        let placeholders = crate::store::helpers::make_placeholders(batch.len());
        let sql = format!("DELETE FROM chunks_fts WHERE id IN ({})", placeholders);
        let mut query = sqlx::query(&sql);
        for chunk in batch {
            query = query.bind(&chunk.id);
        }
        query.execute(&mut **tx).await?;
    }

    // Batch INSERT: add new FTS entries
    for batch in changed.chunks(max_rows_per_statement(5)) {
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
struct EmbeddingBatchIterator<'a, Mode> {
    store: &'a Store<Mode>,
    batch_size: usize,
    /// Last seen rowid for cursor-based pagination
    last_rowid: i64,
    done: bool,
    column: &'static str,
}

impl<'a, Mode> Iterator for EmbeddingBatchIterator<'a, Mode> {
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
                        match bytes_to_embedding(&bytes, self.store.dim) {
                            Ok(emb) => Some((id, Embedding::new(emb))),
                            Err(e) => {
                                tracing::warn!(chunk_id = %id, error = %e, "Skipping corrupt embedding in batch iterator");
                                None
                            }
                        }
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
impl<'a, Mode> std::iter::FusedIterator for EmbeddingBatchIterator<'a, Mode> {}

/// Iterator for streaming `(id, embedding, content_hash)` triples.
///
/// Mirrors [`EmbeddingBatchIterator`]'s cursor pagination but reads the
/// `content_hash` column in the same SQL pass so the hash is consistent
/// with the embedding bytes the snapshot returned (#1124). Used only for
/// the background-rebuild snapshot path; the regular `embedding` column
/// (no hash) flows through [`EmbeddingBatchIterator`].
struct EmbeddingHashBatchIterator<'a, Mode> {
    store: &'a Store<Mode>,
    batch_size: usize,
    last_rowid: i64,
    done: bool,
}

impl<'a, Mode> Iterator for EmbeddingHashBatchIterator<'a, Mode> {
    type Item = Result<Vec<(String, Embedding, String)>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }

            let result = self.store.rt.block_on(async {
                let sql = "SELECT rowid, id, embedding, content_hash FROM chunks \
                           WHERE rowid > ?1 AND embedding IS NOT NULL \
                           ORDER BY rowid ASC LIMIT ?2";
                let rows: Vec<_> = sqlx::query(sql)
                    .bind(self.last_rowid)
                    .bind(self.batch_size as i64)
                    .fetch_all(&self.store.pool)
                    .await?;

                let rows_fetched = rows.len();
                let mut max_rowid = self.last_rowid;

                let batch: Vec<(String, Embedding, String)> = rows
                    .into_iter()
                    .filter_map(|row| {
                        let rowid: i64 = row.get(0);
                        let id: String = row.get(1);
                        let bytes: Vec<u8> = row.get(2);
                        let hash: String = row.get(3);
                        if rowid > max_rowid {
                            max_rowid = rowid;
                        }
                        match bytes_to_embedding(&bytes, self.store.dim) {
                            Ok(emb) => Some((id, Embedding::new(emb), hash)),
                            Err(e) => {
                                tracing::warn!(chunk_id = %id, error = %e, "Skipping corrupt embedding in hash-batch iterator");
                                None
                            }
                        }
                    })
                    .collect();

                Ok::<_, StoreError>((batch, rows_fetched, max_rowid))
            });

            match result {
                Ok((batch, rows_fetched, _)) if batch.is_empty() && rows_fetched == 0 => {
                    self.done = true;
                    return None;
                }
                Ok((batch, _, max_rowid)) => {
                    self.last_rowid = max_rowid;
                    if batch.is_empty() {
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

impl<'a, Mode> std::iter::FusedIterator for EmbeddingHashBatchIterator<'a, Mode> {}

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

    /// P1.17 / #1124: `embedding_and_hash_batches` must yield each chunk's
    /// `(id, embedding, content_hash)` together from a single SQL pass —
    /// the rebuild snapshot relies on hash↔embedding consistency.
    #[test]
    fn test_embedding_and_hash_batches_pairs_id_and_hash_per_row() {
        let (store, _dir) = setup_store();

        let chunks: Vec<_> = (0..6)
            .map(|i| make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i)))
            .collect();
        let expected_hashes: std::collections::HashMap<String, String> = chunks
            .iter()
            .map(|c| (c.id.clone(), c.content_hash.clone()))
            .collect();
        let pairs: Vec<_> = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), mock_embedding(i as f32)))
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        let triples: Vec<_> = store
            .embedding_and_hash_batches(4)
            .filter_map(|b| b.ok())
            .flatten()
            .collect();
        assert_eq!(triples.len(), 6);
        for (id, _emb, hash) in &triples {
            let want = expected_hashes
                .get(id)
                .unwrap_or_else(|| panic!("unexpected id from iterator: {id}"));
            assert_eq!(hash, want, "hash for id {id} must match upsert hash");
        }
    }

    #[test]
    fn test_embedding_and_hash_batches_empty_store() {
        let (store, _dir) = setup_store();
        let batches: Vec<_> = store.embedding_and_hash_batches(10).collect();
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
