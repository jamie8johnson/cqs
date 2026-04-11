// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Sparse vector storage for SPLADE hybrid search.

use super::Store;
use crate::splade::SparseVector;
use crate::store::StoreError;

use sqlx::Row;

impl Store {
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

            // Batched DELETE for all chunk IDs (PF-11: N→ceil(N/333) SQL statements)
            let chunk_ids: Vec<&str> = vectors.iter().map(|(id, _)| id.as_str()).collect();
            for batch in chunk_ids.chunks(333) {
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
            // adds another bound column to the row tuple.
            //
            // Iterate across chunks AND rows together so each batch fills
            // close to capacity, instead of starting a fresh batch per chunk
            // and producing tiny INSERTs for chunks with few tokens.
            const SQLITE_MAX_VARIABLES: usize = 32766; // SQLite default since v3.32
            const VARS_PER_ROW: usize = 3; // chunk_id, token_id, weight
            const SAFETY_MARGIN_VARS: usize = 300; // headroom for one extra column on a max-size batch
            const ROWS_PER_INSERT: usize =
                (SQLITE_MAX_VARIABLES - SAFETY_MARGIN_VARS) / VARS_PER_ROW;
            let mut pending: Vec<(&str, u32, f32)> = Vec::with_capacity(ROWS_PER_INSERT);
            for (chunk_id, sparse) in vectors {
                for &(token_id, weight) in sparse {
                    pending.push((chunk_id.as_str(), token_id, weight));
                    if pending.len() >= ROWS_PER_INSERT {
                        let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
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
            sqlx::query("CREATE INDEX IF NOT EXISTS idx_sparse_token ON sparse_vectors(token_id)")
                .execute(&mut *tx)
                .await?;

            // Bump the SPLADE generation counter so any on-disk SpladeIndex
            // persisted from the previous generation fails its load check
            // and gets rebuilt on the next query. The counter lives in the
            // metadata table; missing rows are treated as generation 0.
            let current: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_optional(&mut *tx)
                    .await?;
            let next: u64 = current
                .and_then(|(s,)| s.parse::<u64>().ok())
                .unwrap_or(0)
                .saturating_add(1);
            sqlx::query(
                "INSERT INTO metadata (key, value) VALUES ('splade_generation', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(next.to_string())
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            tracing::info!(
                entries = total,
                chunks = vectors.len(),
                "Sparse vectors upserted"
            );
            Ok(total)
        })
    }

    /// Load all sparse vectors for building the in-memory SpladeIndex.
    /// Returns Vec of (chunk_id, sparse_vector).
    pub fn load_all_sparse_vectors(&self) -> Result<Vec<(String, SparseVector)>, StoreError> {
        let _span = tracing::info_span!("load_all_sparse_vectors").entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT chunk_id, token_id, weight FROM sparse_vectors ORDER BY chunk_id",
            )
            .fetch_all(&self.pool)
            .await?;

            // Group by chunk_id
            let mut result: Vec<(String, SparseVector)> = Vec::new();
            let mut current_id: Option<String> = None;
            let mut current_vec: SparseVector = Vec::new();

            for row in &rows {
                let chunk_id: String = row.get("chunk_id");
                let token_id: i64 = row.get("token_id");
                let weight: f64 = row.get("weight");

                if current_id.as_ref() != Some(&chunk_id) {
                    if let Some(id) = current_id.take() {
                        result.push((id, std::mem::take(&mut current_vec)));
                    }
                    current_id = Some(chunk_id);
                }
                if token_id < 0 || token_id > u32::MAX as i64 {
                    tracing::warn!(token_id, chunk_id = %current_id.as_deref().unwrap_or("?"), "Invalid token_id, skipping");
                    continue;
                }
                current_vec.push((token_id as u32, weight as f32));
            }
            if let Some(id) = current_id {
                result.push((id, current_vec));
            }

            tracing::info!(
                chunks = result.len(),
                total_entries = rows.len(),
                "Sparse vectors loaded"
            );
            Ok(result)
        })
    }

    /// Get (id, text) pairs for SPLADE encoding.
    /// Text is name + signature + doc comment — the most informative NL-like fields.
    pub fn chunk_splade_texts(&self) -> Result<Vec<(String, String)>, StoreError> {
        let _span = tracing::info_span!("chunk_splade_texts").entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, name, signature, doc FROM chunks")
                .fetch_all(&self.pool)
                .await?;
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

    /// Delete sparse vectors for chunks that no longer exist.
    pub fn prune_orphan_sparse_vectors(&self) -> Result<usize, StoreError> {
        let _span = tracing::debug_span!("prune_orphan_sparse_vectors").entered();
        self.rt.block_on(async {
            let result = sqlx::query(
                "DELETE FROM sparse_vectors WHERE chunk_id NOT IN \
                 (SELECT DISTINCT id FROM chunks)",
            )
            .execute(&self.pool)
            .await?;
            // If any rows were actually deleted, bump the SPLADE generation
            // so stale on-disk indexes get invalidated. A prune that removes
            // zero rows leaves sparse_vectors byte-identical, so the existing
            // generation is still valid and no bump is needed.
            if result.rows_affected() > 0 {
                let current: Option<(String,)> =
                    sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                        .fetch_optional(&self.pool)
                        .await?;
                let next: u64 = current
                    .and_then(|(s,)| s.parse::<u64>().ok())
                    .unwrap_or(0)
                    .saturating_add(1);
                sqlx::query(
                    "INSERT INTO metadata (key, value) VALUES ('splade_generation', ?1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                )
                .bind(next.to_string())
                .execute(&self.pool)
                .await?;
            }
            Ok(result.rows_affected() as usize)
        })
    }

    /// Read the current SPLADE generation counter from the metadata table.
    /// Returns 0 when the key is missing (fresh DB, no sparse vectors ever
    /// written, or a schema predating this field).
    ///
    /// This is read on every SpladeIndex load so persisted files can be
    /// cheaply checked for staleness without walking `sparse_vectors`.
    pub fn splade_generation(&self) -> Result<u64, StoreError> {
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_optional(&self.pool)
                    .await?;
            Ok(row.and_then(|(s,)| s.parse::<u64>().ok()).unwrap_or(0))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_sparse_roundtrip() {
        let (store, _dir) = setup_store();

        // Insert a dummy chunk first (sparse vectors have FK-like relationship)
        // Actually sparse_vectors doesn't have FK constraint, just a logical relationship
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
}
