//! Embedding retrieval by content hash.

use std::collections::HashMap;

use sqlx::Row;

use crate::embedder::Embedding;
use crate::store::helpers::{bytes_to_embedding, StoreError};
use crate::store::Store;

impl Store {
    /// Get embeddings for chunks with matching content hashes (batch lookup).
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
        let dim = self.dim;
        let mut result = HashMap::new();

        self.rt.block_on(async {
            for batch in hashes.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
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
                    if let Some(embedding) = bytes_to_embedding(&bytes, dim) {
                        result.insert(hash, Embedding::new(embedding));
                    }
                }
            }
            Ok(result)
        })
    }

    /// Get (chunk_id, embedding) pairs for chunks with matching content hashes.
    /// Unlike `get_embeddings_by_hashes` (which keys by content_hash), this returns
    /// the chunk ID alongside the embedding — exactly what HNSW `insert_batch` needs.
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
        let dim = self.dim;
        let mut result = Vec::new();

        self.rt.block_on(async {
            for batch in hashes.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
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
                    if let Some(embedding) = bytes_to_embedding(&bytes, dim) {
                        result.push((id, Embedding::new(embedding)));
                    }
                }
            }
            Ok(result)
        })
    }
}
