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

            for (chunk_id, sparse) in vectors {
                // Insert new entries in batches
                // 3 params per row, batch of 333 = 999 < SQLite 999 limit
                const BATCH_SIZE: usize = 333;
                for batch in sparse.chunks(BATCH_SIZE) {
                    let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                        "INSERT INTO sparse_vectors (chunk_id, token_id, weight)",
                    );
                    qb.push_values(batch.iter(), |mut b, &(token_id, weight)| {
                        b.push_bind(chunk_id)
                            .push_bind(token_id as i64)
                            .push_bind(weight);
                    });
                    qb.build().execute(&mut *tx).await?;
                    total += batch.len();
                }
            }

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
            Ok(result.rows_affected() as usize)
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
