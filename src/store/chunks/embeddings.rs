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
                    match bytes_to_embedding(&bytes, dim) {
                        Ok(embedding) => {
                            result.insert(hash, Embedding::new(embedding));
                        }
                        Err(e) => {
                            tracing::warn!(hash = %hash, error = %e, "Corrupt embedding blob, skipping — run 'cqs index --force' to rebuild");
                        }
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
                    match bytes_to_embedding(&bytes, dim) {
                        Ok(embedding) => {
                            result.push((id, Embedding::new(embedding)));
                        }
                        Err(e) => {
                            tracing::trace!(chunk_id = %id, error = %e, "Skipping embedding");
                        }
                    }
                }
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::embedder::Embedding;
    use crate::parser::{Chunk, ChunkType, Language};
    use crate::store::{ModelInfo, Store};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn mock_embedding(seed: f32) -> Embedding {
        let mut v = vec![seed; crate::EMBEDDING_DIM];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Embedding::new(v)
    }

    fn test_chunk(name: &str, content: &str) -> Chunk {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("test.rs:1:{}", &hash[..8]),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    fn make_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_get_chunk_ids_and_embeddings_by_hashes_roundtrip() {
        let (store, _dir) = make_store();

        let chunk1 = test_chunk("alpha", "fn alpha() { 1 }");
        let chunk2 = test_chunk("beta", "fn beta() { 2 }");
        let chunk3 = test_chunk("gamma", "fn gamma() { 3 }");

        let emb1 = mock_embedding(1.0);
        let emb2 = mock_embedding(2.0);
        let emb3 = mock_embedding(3.0);

        store.upsert_chunk(&chunk1, &emb1, Some(100)).unwrap();
        store.upsert_chunk(&chunk2, &emb2, Some(100)).unwrap();
        store.upsert_chunk(&chunk3, &emb3, Some(100)).unwrap();

        let hashes: Vec<&str> = vec![
            chunk1.content_hash.as_str(),
            chunk2.content_hash.as_str(),
            chunk3.content_hash.as_str(),
        ];
        let result = store
            .get_chunk_ids_and_embeddings_by_hashes(&hashes)
            .unwrap();

        assert_eq!(result.len(), 3, "Should return all 3 inserted chunks");

        // Build a lookup by chunk id for order-independent assertions
        let by_id: std::collections::HashMap<&str, &Embedding> =
            result.iter().map(|(id, emb)| (id.as_str(), emb)).collect();

        assert!(by_id.contains_key(chunk1.id.as_str()));
        assert!(by_id.contains_key(chunk2.id.as_str()));
        assert!(by_id.contains_key(chunk3.id.as_str()));

        // Verify embeddings match (cosine similarity ~1.0 with themselves)
        let cos1 =
            crate::math::cosine_similarity(by_id[chunk1.id.as_str()].as_slice(), emb1.as_slice())
                .unwrap();
        let cos2 =
            crate::math::cosine_similarity(by_id[chunk2.id.as_str()].as_slice(), emb2.as_slice())
                .unwrap();
        let cos3 =
            crate::math::cosine_similarity(by_id[chunk3.id.as_str()].as_slice(), emb3.as_slice())
                .unwrap();
        assert!(cos1 > 0.99, "emb1 round-trip similarity: {cos1}");
        assert!(cos2 > 0.99, "emb2 round-trip similarity: {cos2}");
        assert!(cos3 > 0.99, "emb3 round-trip similarity: {cos3}");
    }
}
