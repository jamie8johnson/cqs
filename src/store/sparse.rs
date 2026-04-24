// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Sparse vector storage for SPLADE hybrid search.
//!
//! ## Invalidation contract (v19+)
//!
//! The persisted `splade.index.bin` file embeds a generation counter in its
//! header. On load, the counter is compared against `metadata.splade_generation`;
//! any mismatch forces a rebuild from SQLite. The counter is maintained by
//! [`Store::bump_splade_generation_tx`], the single source of truth for this
//! bump. **Every write site that mutates `sparse_vectors` must call it before
//! committing.** As of v19 the `sparse_vectors` table has a foreign key with
//! `ON DELETE CASCADE` on `chunk_id → chunks(id)`, so deletes through the
//! `chunks` table automatically cascade — those paths bump the generation via
//! their own `bump_splade_generation_tx` call after the `chunks` delete.
//!
//! v1.22.0 audit rules ([docs/audit-triage.md] cluster: CQ-3, DS-W1, DS-W3,
//! DS-W4, EH-1/2/3/4, API-14) apply here. Do not add a fourth write site
//! without wiring the bump.

use super::{ReadWrite, Store};
use crate::splade::SparseVector;
use crate::store::StoreError;

use sqlx::Row;

/// Number of distinct chunk_ids fetched per page by [`Store::load_all_sparse_vectors`].
///
/// PF-V1.25-19: sized so the per-batch row Vec fits comfortably in a few MB
/// even at the upper bound of tokens-per-chunk (~100-500). With 1000 chunks
/// × 200 avg tokens/row × ~80B/row ≈ 16 MB per batch. The single-query
/// `fetch_all` path that preceded this could hit multiple GB on a large
/// index. Larger values reduce round-trip count but cap the memory win.
const LOAD_SPARSE_CHUNK_ID_BATCH: i64 = 1000;

/// Bump the SPLADE generation counter inside an existing write transaction.
///
/// This is the one place the counter gets incremented. Every write path that
/// touches `sparse_vectors` (directly or via FK cascade from `chunks`) must
/// call this before committing. Silent parse failures on corrupt metadata
/// are warned but not made fatal — a corrupt value resets to 1 and the next
/// loader sees a fresh generation, forcing a clean rebuild.
///
/// Takes `&mut Transaction` so callers that already hold a write transaction
/// don't fight the WRITE_LOCK. Readers use [`Store::splade_generation`]
/// against the pool.
pub(crate) async fn bump_splade_generation_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), StoreError> {
    let current: Option<(String,)> =
        sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
            .fetch_optional(&mut **tx)
            .await?;
    let next: u64 = match current {
        Some((ref s,)) => match s.parse::<u64>() {
            Ok(n) => n.saturating_add(1),
            Err(e) => {
                tracing::warn!(
                    raw = %s,
                    error = %e,
                    "splade_generation metadata is not a valid u64, resetting to 1",
                );
                1
            }
        },
        None => 1,
    };
    sqlx::query(
        "INSERT INTO metadata (key, value) VALUES ('splade_generation', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(next.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// DS2-9: Standalone (not-in-transaction) SPLADE generation bump.
///
/// Used by error paths that already rolled back the primary write transaction
/// but still want to force on-disk `splade.index.bin` invalidation. The upsert
/// attempt may have touched DDL state visible to concurrent writers (DROP /
/// CREATE INDEX), and any cached on-disk SPLADE index built from a prior
/// successful state should be treated as suspect. Bumping in its own micro-
/// transaction is the cheapest belt-and-suspenders guard: the next loader
/// sees the new generation, fails the checksum equality against the file
/// header, and rebuilds from whatever sparse_vectors actually persisted.
///
/// Opens its own write transaction on the pool. Errors here are logged at
/// warn level and returned so callers can surface a combined failure, but
/// the typical pattern is log-and-swallow — the original upsert error is
/// what the user cares about, the bump is a durability safety net.
pub(crate) async fn bump_splade_generation_standalone(
    pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    bump_splade_generation_tx(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

impl Store<ReadWrite> {
    /// Upsert sparse vectors for a batch of chunks.
    /// Replaces existing vectors for the same chunk_id.
    ///
    /// **Bulk insert optimization:** drops the secondary `idx_sparse_token`
    /// index before the bulk insert and recreates it after. With SPLADE-Code
    /// 0.6B's denser sparse vectors (~1000+ tokens per chunk), the secondary
    /// B-tree maintenance per row was the dominant cost — every INSERT had to
    /// update both the PRIMARY KEY index AND the token_id index, doubling
    /// per-row work and producing 6+ GB WAL files for a single reindex pass.
    /// Drop + recreate is the standard SQLite bulk-load pattern: the recreate
    /// rebuilds the index in one O(n log n) batch instead of N O(log n) per-row
    /// updates, and the read-time SPLADE search path is unaffected because
    /// the index is back in place by the time the function returns.
    pub fn upsert_sparse_vectors(
        &self,
        vectors: &[(String, SparseVector)],
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("upsert_sparse_vectors", count = vectors.len()).entered();
        if vectors.is_empty() {
            return Ok(0);
        }
        self.rt.block_on(async {
            // DS2-9: Wrap the write transaction in an inner async block so any
            // `?`-propagated error lands in the outer `match`. On error we bump
            // the SPLADE generation outside the (now rolled-back) transaction
            // to invalidate any on-disk `splade.index.bin` that might still be
            // cached from a prior state — the DROP/CREATE INDEX DDL and partial
            // row inserts are all reverted by rollback, but a standalone bump
            // is the cheapest belt-and-suspenders against stale on-disk caches
            // and concurrent daemons.
            let inner_result: Result<usize, StoreError> = async {
                let (_guard, mut tx) = self.begin_write().await?;
                let mut total = 0usize;

                // Drop the secondary token_id index before the bulk insert.
                // The PRIMARY KEY index (chunk_id, token_id) cannot be dropped
                // without recreating the table, so we leave it in place; that
                // single B-tree is unavoidable. The secondary token_id index
                // is purely for read-time SPLADE search lookups and can be
                // safely dropped+recreated around any write batch. The recreate
                // at the end of this function restores it before any reader
                // can observe the missing-index state.
                tracing::debug!("Dropping idx_sparse_token before bulk insert");
                sqlx::query("DROP INDEX IF EXISTS idx_sparse_token")
                    .execute(&mut *tx)
                    .await?;

                // Batched DELETE for all chunk IDs, sized to the SQLite variable
                // limit (not the pre-3.32 999). Previously `chunks(333)` — audit
                // finding SHL-31: PR #891 fixed the sibling INSERT loop but left
                // the DELETE on the old constant, paying ~30x the statement
                // count. One bind variable per chunk_id, minus the safety margin.
                use crate::store::helpers::sql::max_rows_per_statement;
                const DELETE_BATCH: usize = max_rows_per_statement(1);
                let chunk_ids: Vec<&str> = vectors.iter().map(|(id, _)| id.as_str()).collect();
                for batch in chunk_ids.chunks(DELETE_BATCH) {
                    let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> =
                        sqlx::QueryBuilder::new("DELETE FROM sparse_vectors WHERE chunk_id IN (");
                    let mut sep = qb.separated(", ");
                    for id in batch {
                        sep.push_bind(*id);
                    }
                    sep.push_unseparated(")");
                    qb.build().execute(&mut *tx).await?;
                }

                // Insert in batches sized to the SQLite variable limit.
                //
                // SQLite caps the number of bind variables per statement.
                // SQLITE_MAX_VARIABLE_NUMBER was 999 before v3.32 (2020) and
                // is 32766 in current SQLite. The previous BATCH_SIZE constant
                // was tuned for the old 999 limit and produced ~10x more INSERT
                // statements than necessary with the modern limit. With
                // SPLADE-Code 0.6B's denser sparse vectors (~1000+ tokens per
                // chunk vs ~134 for BERT 110M), the per-statement sqlx overhead
                // compounded into 30+ minute upserts that looked like a hang.
                //
                // The new batch size is derived from the constraint, not picked:
                // each row uses VARS_PER_ROW bind variables, the maximum rows
                // per statement is therefore (limit / VARS_PER_ROW), and we
                // leave a safety margin for any future schema addition that
                // adds another bound column to the row tuple. The SAFETY_MARGIN
                // is a generic headroom, not sized to absorb a full extra column
                // (SHL-41 audit: adding a new bind column requires increasing
                // VARS_PER_ROW, not this margin).
                //
                // Iterate across chunks AND rows together so each batch fills
                // close to capacity, instead of starting a fresh batch per chunk
                // and producing tiny INSERTs for chunks with few tokens.
                const ROWS_PER_INSERT: usize = max_rows_per_statement(3);
                let mut pending: Vec<(&str, u32, f32)> = Vec::with_capacity(ROWS_PER_INSERT);
                for (chunk_id, sparse) in vectors {
                    for &(token_id, weight) in sparse {
                        pending.push((chunk_id.as_str(), token_id, weight));
                        if pending.len() >= ROWS_PER_INSERT {
                            let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> =
                                sqlx::QueryBuilder::new(
                                    "INSERT INTO sparse_vectors (chunk_id, token_id, weight)",
                                );
                            qb.push_values(pending.iter(), |mut b, &(cid, tid, w)| {
                                b.push_bind(cid).push_bind(tid as i64).push_bind(w);
                            });
                            qb.build().execute(&mut *tx).await?;
                            total += pending.len();
                            pending.clear();
                        }
                    }
                }
                // Flush remaining rows
                if !pending.is_empty() {
                    let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                        "INSERT INTO sparse_vectors (chunk_id, token_id, weight)",
                    );
                    qb.push_values(pending.iter(), |mut b, &(cid, tid, w)| {
                        b.push_bind(cid).push_bind(tid as i64).push_bind(w);
                    });
                    qb.build().execute(&mut *tx).await?;
                    total += pending.len();
                }

                // Recreate the secondary index. SQLite builds the index in one
                // O(n log n) batch from the populated table, which is far cheaper
                // than maintaining the index per-row through the bulk insert.
                tracing::debug!("Recreating idx_sparse_token after bulk insert");
                sqlx::query(
                    "CREATE INDEX IF NOT EXISTS idx_sparse_token ON sparse_vectors(token_id)",
                )
                .execute(&mut *tx)
                .await?;

                // Centralized generation bump — one call site for the invariant
                // (audit API-14). Prior to v19 this was inlined here AND in
                // `prune_orphan_sparse_vectors`; the duplication was a footgun
                // because every new write path would need to remember the bump.
                bump_splade_generation_tx(&mut tx).await?;

                tx.commit().await?;
                Ok(total)
            }
            .await;

            match inner_result {
                Ok(total) => {
                    tracing::info!(
                        entries = total,
                        chunks = vectors.len(),
                        "Sparse vectors upserted"
                    );
                    Ok(total)
                }
                Err(e) => {
                    // DS2-9: The inner transaction rolled back cleanly. Bump
                    // the generation in a fresh micro-transaction so any
                    // on-disk `splade.index.bin` built from a previous
                    // successful state is forced to rebuild at next load —
                    // a bump failure here is logged but non-fatal; the
                    // original upsert error is the one the caller cares
                    // about.
                    tracing::warn!(
                        error = %e,
                        "upsert_sparse_vectors failed — bumping SPLADE generation to invalidate on-disk cache"
                    );
                    if let Err(bump_err) = bump_splade_generation_standalone(&self.pool).await {
                        tracing::warn!(
                            error = %bump_err,
                            "failed to bump SPLADE generation on error-path cleanup (non-fatal)"
                        );
                    }
                    Err(e)
                }
            }
        })
    }
}

impl<Mode> Store<Mode> {
    /// Load all sparse vectors for building the in-memory SpladeIndex.
    /// Returns Vec of (chunk_id, sparse_vector).
    ///
    /// PF-V1.25-19: previously ran a single `SELECT ... FROM sparse_vectors
    /// ORDER BY chunk_id` + `fetch_all`, which materialized the entire
    /// `sparse_vectors` table as a `Vec<SqliteRow>` in memory before
    /// grouping. On a 60k-chunk index with ~100 tokens/chunk that's 6M
    /// rows × ~80B/row ≈ 500MB peak, on top of the final grouped Vec.
    ///
    /// Now paginates by chunk_id in batches of up to
    /// [`LOAD_SPARSE_CHUNK_ID_BATCH`] distinct chunk_ids, using a cursor on
    /// `chunk_id`. Each batch's rows are fully grouped before discarding
    /// the row Vec, so peak memory is bounded to one batch (~5-20 MB) plus
    /// the growing result.
    pub fn load_all_sparse_vectors(&self) -> Result<Vec<(String, SparseVector)>, StoreError> {
        let _span = tracing::info_span!("load_all_sparse_vectors").entered();
        self.rt.block_on(async {
            let mut result: Vec<(String, SparseVector)> = Vec::new();
            let mut total_entries: usize = 0;
            // Cursor: chunk_id ordering. Empty string sorts before any real id
            // (SQLite BINARY collation, which is the default), so using "" as
            // the initial cursor fetches the full range on the first query.
            let mut last_chunk_id = String::new();

            loop {
                // Fetch up to LOAD_SPARSE_CHUNK_ID_BATCH distinct chunk_ids
                // past the cursor, and all their rows, in one query. The
                // subquery's LIMIT bounds the outer rows to one batch.
                let rows: Vec<_> = sqlx::query(
                    "SELECT chunk_id, token_id, weight FROM sparse_vectors \
                     WHERE chunk_id IN ( \
                         SELECT DISTINCT chunk_id FROM sparse_vectors \
                         WHERE chunk_id > ?1 \
                         ORDER BY chunk_id \
                         LIMIT ?2 \
                     ) \
                     ORDER BY chunk_id, token_id",
                )
                .bind(&last_chunk_id)
                .bind(LOAD_SPARSE_CHUNK_ID_BATCH)
                .fetch_all(&self.pool)
                .await?;

                if rows.is_empty() {
                    break;
                }
                total_entries += rows.len();

                // Group rows by chunk_id (rows are already ORDER BY chunk_id,
                // so we can stream through linearly). Each chunk_id belongs
                // entirely to this batch — the subquery guarantees complete
                // groups, never splitting a chunk across pages.
                let mut current_id: Option<String> = None;
                let mut current_vec: SparseVector = Vec::new();
                let mut last_id_in_batch = String::new();
                let mut distinct_ids_in_batch: i64 = 0;

                for row in &rows {
                    let chunk_id: String = row.get("chunk_id");
                    let token_id: i64 = row.get("token_id");
                    let weight: f64 = row.get("weight");

                    if current_id.as_ref() != Some(&chunk_id) {
                        if let Some(id) = current_id.take() {
                            result.push((id, std::mem::take(&mut current_vec)));
                        }
                        last_id_in_batch = chunk_id.clone();
                        distinct_ids_in_batch += 1;
                        current_id = Some(chunk_id);
                    }
                    if token_id < 0 || token_id > u32::MAX as i64 {
                        tracing::warn!(
                            token_id,
                            chunk_id = %current_id.as_deref().unwrap_or("?"),
                            "Invalid token_id, skipping"
                        );
                        continue;
                    }
                    current_vec.push((token_id as u32, weight as f32));
                }
                if let Some(id) = current_id {
                    result.push((id, current_vec));
                }

                // If the batch returned fewer distinct chunk_ids than the
                // page budget, the next query would find nothing — skip
                // the empty round-trip. Otherwise advance the cursor past
                // the last-seen id and loop.
                if distinct_ids_in_batch < LOAD_SPARSE_CHUNK_ID_BATCH {
                    break;
                }
                last_chunk_id = last_id_in_batch;
            }

            tracing::info!(
                chunks = result.len(),
                total_entries,
                "Sparse vectors loaded"
            );
            Ok(result)
        })
    }

    /// Get (id, text) pairs for SPLADE encoding.
    /// Text is name + signature + doc comment — the most informative NL-like fields.
    pub fn chunk_splade_texts(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.chunk_splade_texts_query("SELECT id, name, signature, doc FROM chunks")
    }

    /// Like `chunk_splade_texts` but only returns chunks that don't yet have
    /// sparse vectors in the `sparse_vectors` table. For incremental indexing
    /// (CQ-4): avoids re-encoding the entire corpus on every `cqs index` run.
    pub fn chunk_splade_texts_missing(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.chunk_splade_texts_query(
            "SELECT c.id, c.name, c.signature, c.doc FROM chunks c \
             WHERE c.id NOT IN (SELECT DISTINCT chunk_id FROM sparse_vectors)",
        )
    }

    fn chunk_splade_texts_query(&self, sql: &str) -> Result<Vec<(String, String)>, StoreError> {
        let _span = tracing::info_span!("chunk_splade_texts").entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(sql).fetch_all(&self.pool).await?;
            let result: Vec<(String, String)> = rows
                .iter()
                .map(|row| {
                    let id: String = row.get("id");
                    let name: String = row.get("name");
                    let sig: String = row.get("signature");
                    let doc: Option<String> = row.get("doc");
                    let text = match doc {
                        Some(d) if !d.is_empty() => format!("{} {} {}", name, sig, d),
                        _ => format!("{} {}", name, sig),
                    };
                    (id, text)
                })
                .collect();
            tracing::info!(chunks = result.len(), "Loaded chunk texts for SPLADE");
            Ok(result)
        })
    }
}

impl Store<ReadWrite> {
    /// Delete sparse vectors for chunks that no longer exist.
    ///
    /// **As of v19 this is a no-op on clean data** — the `sparse_vectors` →
    /// `chunks` FK cascade removes orphans automatically on every chunks
    /// delete. The function is kept for one-shot repair scenarios (manual
    /// SQL manipulation, restore from older backup, migration from pre-v19
    /// data that had orphans). The v18→v19 migration already drops orphans
    /// during the table rebuild, so on a correctly-migrated DB this will
    /// delete zero rows.
    ///
    /// Wraps DELETE + generation bump in a single `begin_write` transaction
    /// (audit CQ-3: previously the three statements ran on `&self.pool`
    /// independently, opening a lost-update race and a crash window between
    /// DELETE and the metadata UPDATE).
    pub fn prune_orphan_sparse_vectors(&self) -> Result<usize, StoreError> {
        let _span = tracing::debug_span!("prune_orphan_sparse_vectors").entered();
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let result = sqlx::query(
                "DELETE FROM sparse_vectors WHERE chunk_id NOT IN \
                 (SELECT DISTINCT id FROM chunks)",
            )
            .execute(&mut *tx)
            .await?;
            // If any rows were actually deleted, bump the SPLADE generation
            // so stale on-disk indexes get invalidated. A prune that removes
            // zero rows leaves sparse_vectors byte-identical, so the existing
            // generation is still valid and no bump is needed.
            let affected = result.rows_affected();
            if affected > 0 {
                bump_splade_generation_tx(&mut tx).await?;
                tracing::warn!(
                    rows = affected,
                    "prune_orphan_sparse_vectors deleted rows that should have been caught by \
                     the v19 FK cascade — either FK enforcement is disabled or this database was \
                     manipulated directly. Investigate."
                );
            }
            tx.commit().await?;
            Ok(affected as usize)
        })
    }
}

impl<Mode> Store<Mode> {
    /// Read the current SPLADE generation counter from the metadata table.
    ///
    /// Returns `0` when the key is missing (fresh DB, no sparse vectors ever
    /// written, or a schema predating this field). A non-parseable value is
    /// logged as a warning and collapses to 0 — callers should treat 0 as
    /// "force rebuild" because a corrupt counter cannot be trusted.
    ///
    /// This is read on every SpladeIndex load so persisted files can be
    /// cheaply checked for staleness without walking `sparse_vectors`.
    /// Audit EH-1/OB-17: previously `.parse::<u64>().ok().unwrap_or(0)`
    /// silently collapsed corruption to 0, pairing with the `load_or_build`
    /// caller to form a self-perpetuating cache-poison loop (EH-3). The
    /// explicit warn makes corruption visible; the caller still returns `0`
    /// so the loader rebuilds defensively.
    pub fn splade_generation(&self) -> Result<u64, StoreError> {
        let _span = tracing::debug_span!("splade_generation").entered();
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_optional(&self.pool)
                    .await?;
            Ok(match row {
                Some((s,)) => match s.parse::<u64>() {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!(
                            raw = %s,
                            error = %e,
                            "splade_generation metadata is not a valid u64, treating as 0 — \
                             next SPLADE load will rebuild from SQLite"
                        );
                        0
                    }
                },
                None => 0,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Insert a minimal valid `chunks` row so a sparse_vectors insert can
    /// satisfy the v19 FK constraint. Pattern mirrored from
    /// `src/store/types.rs:insert_test_chunk`.
    fn insert_test_chunk(store: &Store, id: &str) {
        store.rt.block_on(async {
            let embedding = crate::embedder::Embedding::new(vec![0.0f32; crate::EMBEDDING_DIM]);
            let embedding_bytes =
                crate::store::helpers::embedding_to_bytes(&embedding, crate::EMBEDDING_DIM)
                    .unwrap();
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                     signature, content, content_hash, doc, line_start, line_end, embedding,
                     source_mtime, created_at, updated_at)
                     VALUES (?1, ?1, 'file', 'rust', 'function', ?1,
                     '', '', '', NULL, 1, 10, ?2, 0, ?3, ?3)",
            )
            .bind(id)
            .bind(&embedding_bytes)
            .bind(&now)
            .execute(&store.pool)
            .await
            .unwrap();
        });
    }

    fn setup_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    /// Insert a chunk with explicit name, signature, and optional doc.
    /// Used by chunk_splade_texts tests that need control over those fields.
    fn insert_chunk_with_fields(
        store: &Store,
        id: &str,
        name: &str,
        signature: &str,
        doc: Option<&str>,
    ) {
        store.rt.block_on(async {
            let embedding = crate::embedder::Embedding::new(vec![0.0f32; crate::EMBEDDING_DIM]);
            let embedding_bytes =
                crate::store::helpers::embedding_to_bytes(&embedding, crate::EMBEDDING_DIM)
                    .unwrap();
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                     signature, content, content_hash, doc, line_start, line_end, embedding,
                     source_mtime, created_at, updated_at)
                     VALUES (?1, ?1, 'file', 'rust', 'function', ?2,
                     ?3, '', '', ?4, 1, 10, ?5, 0, ?6, ?6)",
            )
            .bind(id)
            .bind(name)
            .bind(signature)
            .bind(doc)
            .bind(&embedding_bytes)
            .bind(&now)
            .execute(&store.pool)
            .await
            .unwrap();
        });
    }

    #[test]
    fn test_sparse_roundtrip() {
        let (store, _dir) = setup_store();

        // v19 requires matching chunks rows for the FK to validate.
        insert_test_chunk(&store, "chunk_a");
        insert_test_chunk(&store, "chunk_b");

        let vectors = vec![
            (
                "chunk_a".to_string(),
                vec![(1u32, 0.5f32), (2, 0.3), (3, 0.8)],
            ),
            ("chunk_b".to_string(), vec![(1, 0.7), (4, 0.6)]),
        ];

        let entries = store.upsert_sparse_vectors(&vectors).unwrap();
        assert_eq!(entries, 5); // 3 + 2

        let loaded = store.load_all_sparse_vectors().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].0, "chunk_a");
        assert_eq!(loaded[0].1.len(), 3);
        assert_eq!(loaded[1].0, "chunk_b");
        assert_eq!(loaded[1].1.len(), 2);
    }

    #[test]
    fn test_sparse_upsert_replaces() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "chunk_a");

        let v1 = vec![("chunk_a".to_string(), vec![(1u32, 0.5f32)])];
        store.upsert_sparse_vectors(&v1).unwrap();

        // Upsert with different values
        let v2 = vec![("chunk_a".to_string(), vec![(2u32, 0.9f32), (3, 0.1)])];
        store.upsert_sparse_vectors(&v2).unwrap();

        let loaded = store.load_all_sparse_vectors().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1.len(), 2); // replaced, not appended
        assert_eq!(loaded[0].1[0].0, 2); // new token
    }

    #[test]
    fn test_sparse_empty() {
        let (store, _dir) = setup_store();
        let loaded = store.load_all_sparse_vectors().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_splade_generation_starts_at_zero_and_is_monotonic() {
        // Audit Test Coverage (Adversarial): prune_orphan_sparse_vectors and
        // splade_generation had zero tests total. This covers the happy path
        // of the counter across upsert + prune.
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "c1");

        assert_eq!(store.splade_generation().unwrap(), 0);

        store
            .upsert_sparse_vectors(&[("c1".to_string(), vec![(1u32, 0.5f32)])])
            .unwrap();
        let after_upsert = store.splade_generation().unwrap();
        assert!(after_upsert >= 1, "upsert must bump generation");

        // Second upsert bumps again.
        store
            .upsert_sparse_vectors(&[("c1".to_string(), vec![(2u32, 0.5f32)])])
            .unwrap();
        let after_second = store.splade_generation().unwrap();
        assert!(
            after_second > after_upsert,
            "second upsert must bump generation strictly"
        );
    }

    #[test]
    fn test_prune_orphan_no_rows_does_not_bump_generation() {
        // Audit Test Coverage (Adversarial): the zero-rows-deleted branch
        // that skips the generation bump. A regression flipping the
        // condition would silently invalidate the persisted SpladeIndex
        // on every `cqs index`.
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "c1");
        store
            .upsert_sparse_vectors(&[("c1".to_string(), vec![(1u32, 0.5f32)])])
            .unwrap();

        let before = store.splade_generation().unwrap();
        let pruned = store.prune_orphan_sparse_vectors().unwrap();
        assert_eq!(pruned, 0, "v19 FK cascade should leave zero orphans");
        let after = store.splade_generation().unwrap();
        assert_eq!(
            before, after,
            "zero-orphan prune must NOT bump the generation"
        );
    }

    #[test]
    fn test_prune_orphan_on_clean_db_is_noop() {
        // TC-ADV-13: the zero-rows branch must be a true no-op on a clean
        // DB that has never held any sparse vectors. A regression flipping
        // the `if affected > 0` guard to `>= 0` would bump the generation
        // on every `cqs index` even when no work was done.
        let (store, _dir) = setup_store();

        let before = store.splade_generation().unwrap();
        let pruned = store.prune_orphan_sparse_vectors().unwrap();
        assert_eq!(pruned, 0, "fresh DB must have zero orphans to prune");
        let after = store.splade_generation().unwrap();
        assert_eq!(before, after, "no-op prune on clean DB must not bump");
    }

    #[test]
    fn test_fk_cascade_removes_sparse_rows_on_chunk_delete() {
        // Audit DS-W3: v19 FK CASCADE contract. Deleting a chunk must
        // automatically remove its sparse_vectors rows.
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "c1");
        insert_test_chunk(&store, "c2");
        store
            .upsert_sparse_vectors(&[
                ("c1".to_string(), vec![(1u32, 0.5f32), (2, 0.3)]),
                ("c2".to_string(), vec![(3u32, 0.7f32)]),
            ])
            .unwrap();

        // Directly delete c1 from chunks and confirm the cascade removed
        // its sparse rows without any explicit cleanup.
        store.rt.block_on(async {
            sqlx::query("DELETE FROM chunks WHERE id = 'c1'")
                .execute(&store.pool)
                .await
                .unwrap();
        });
        let loaded = store.load_all_sparse_vectors().unwrap();
        assert_eq!(
            loaded.len(),
            1,
            "cascade should have dropped c1's sparse rows"
        );
        assert_eq!(loaded[0].0, "c2");
    }

    // ── Gap 1: chunk_splade_texts concatenation format ──────────────

    #[test]
    fn test_chunk_splade_texts_with_doc() {
        let (store, _dir) = setup_store();
        insert_chunk_with_fields(&store, "c1", "foo", "fn foo()", Some("does things"));
        let texts = store.chunk_splade_texts().unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].1, "foo fn foo() does things");
    }

    #[test]
    fn test_chunk_splade_texts_no_doc() {
        let (store, _dir) = setup_store();
        insert_chunk_with_fields(&store, "c1", "foo", "fn foo()", None);
        let texts = store.chunk_splade_texts().unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].1, "foo fn foo()");
    }

    #[test]
    fn test_chunk_splade_texts_empty_doc_treated_as_missing() {
        let (store, _dir) = setup_store();
        insert_chunk_with_fields(&store, "c1", "foo", "fn foo()", Some(""));
        let texts = store.chunk_splade_texts().unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(
            texts[0].1, "foo fn foo()",
            "empty doc should not be appended"
        );
    }

    // ── Gap 2: chunk_splade_texts_missing ───────────────────────────

    #[test]
    fn test_chunk_splade_texts_missing_skips_encoded() {
        let (store, _dir) = setup_store();
        insert_chunk_with_fields(&store, "c1", "alpha", "fn alpha()", None);
        insert_chunk_with_fields(&store, "c2", "beta", "fn beta()", None);
        insert_chunk_with_fields(&store, "c3", "gamma", "fn gamma()", None);

        // Encode c1 and c3, leave c2 without sparse vectors.
        store
            .upsert_sparse_vectors(&[
                ("c1".to_string(), vec![(1u32, 0.5f32)]),
                ("c3".to_string(), vec![(2u32, 0.7f32)]),
            ])
            .unwrap();

        let missing = store.chunk_splade_texts_missing().unwrap();
        assert_eq!(missing.len(), 1, "only the un-encoded chunk should appear");
        assert_eq!(missing[0].0, "c2");
        assert_eq!(missing[0].1, "beta fn beta()");
    }

    // ── Gap 3: load_all_sparse_vectors grouping ─────────────────────

    #[test]
    fn test_load_all_sparse_vectors_groups_multiple_tokens() {
        let (store, _dir) = setup_store();
        insert_test_chunk(&store, "c1");
        insert_test_chunk(&store, "c2");
        insert_test_chunk(&store, "c3");

        store
            .upsert_sparse_vectors(&[
                (
                    "c1".to_string(),
                    vec![(10u32, 0.1f32), (11, 0.2), (12, 0.3)],
                ),
                (
                    "c2".to_string(),
                    vec![(20u32, 0.4f32), (21, 0.5), (22, 0.6), (23, 0.7)],
                ),
                (
                    "c3".to_string(),
                    vec![(30u32, 0.8f32), (31, 0.9), (32, 1.0)],
                ),
            ])
            .unwrap();

        let loaded = store.load_all_sparse_vectors().unwrap();
        assert_eq!(loaded.len(), 3);

        // Find each chunk and verify token counts.
        let c1 = loaded.iter().find(|(id, _)| id == "c1").unwrap();
        let c2 = loaded.iter().find(|(id, _)| id == "c2").unwrap();
        let c3 = loaded.iter().find(|(id, _)| id == "c3").unwrap();
        assert_eq!(c1.1.len(), 3, "c1 should have 3 tokens");
        assert_eq!(c2.1.len(), 4, "c2 should have 4 tokens");
        assert_eq!(c3.1.len(), 3, "c3 should have 3 tokens");
    }
}
