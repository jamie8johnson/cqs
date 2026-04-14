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
                        // TC-ADV-1: route through `Embedding::try_new` so NaN/
                        // Inf embeddings (corrupt blob, interrupted embedder
                        // run, bit rot, downstream writer bug) are rejected
                        // before they can poison HNSW build or query paths.
                        // `try_new` enforces finiteness; previously the code
                        // used the unchecked `Embedding::new` constructor.
                        Ok(embedding) => match Embedding::try_new(embedding) {
                            Ok(e) => {
                                result.insert(hash, e);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    hash = %hash,
                                    error = %e,
                                    "Non-finite embedding values (NaN/Inf), skipping — \
                                     run 'cqs index --force' to rebuild"
                                );
                            }
                        },
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
                        // TC-ADV-1: same finiteness guard as
                        // `get_embeddings_by_hashes`. NaN/Inf values would
                        // produce non-finite cosine distances inside HNSW
                        // build and corrupt the graph; skip the row with a
                        // warn instead.
                        Ok(embedding) => match Embedding::try_new(embedding) {
                            Ok(e) => {
                                result.push((id, e));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    chunk_id = %id,
                                    error = %e,
                                    "Non-finite embedding values (NaN/Inf), skipping — \
                                     run 'cqs index --force' to rebuild"
                                );
                            }
                        },
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

    /// Build a `Vec<u8>` embedding blob containing at least one `f32::NAN`
    /// so `bytes_to_embedding` returns Ok but `Embedding::try_new` rejects it.
    fn nan_embedding_bytes() -> Vec<u8> {
        let mut v = vec![0.5f32; crate::EMBEDDING_DIM];
        // Drop a NaN somewhere inside the vector so byte-length is correct
        // but finiteness is violated.
        v[crate::EMBEDDING_DIM / 2] = f32::NAN;
        bytemuck::cast_slice::<f32, u8>(&v).to_vec()
    }

    /// TC-ADV-1: `get_embeddings_by_hashes` must not propagate NaN-containing
    /// embeddings into HNSW. The production write path never produces NaN,
    /// but a corrupt blob (interrupted embedder run, bit rot, downstream
    /// writer bug) could land in the `chunks` table. This test directly
    /// inserts NaN bytes and verifies the reader skips the row with a warn.
    #[test]
    fn test_get_embeddings_by_hashes_skips_nan_blobs() {
        let (store, _dir) = make_store();

        // Insert a well-formed chunk + embedding for the control case.
        let good = test_chunk("good", "fn good() { 1 }");
        store
            .upsert_chunk(&good, &mock_embedding(1.0), Some(100))
            .unwrap();

        // Insert a chunk whose embedding bytes decode to a NaN-containing
        // vector. We bypass upsert_chunk (which accepts only an `Embedding`,
        // already constructed) and write raw bytes via sqlx.
        let bad_hash = "a".repeat(64); // 64-char hex, distinct from `good`
        let bad_id = format!("test.rs:9:{}", &bad_hash[..8]);
        let bad_bytes = nan_embedding_bytes();
        store.rt.block_on(async {
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                 signature, content, content_hash, doc, line_start, line_end, embedding,
                 source_mtime, created_at, updated_at)
                 VALUES (?1, 'nan.rs', 'file', 'rust', 'function', 'nanfn',
                 'fn nanfn()', 'fn nanfn() {}', ?2, NULL, 1, 5, ?3, 0,
                 '1970-01-01', '1970-01-01')",
            )
            .bind(&bad_id)
            .bind(&bad_hash)
            .bind(&bad_bytes)
            .execute(&store.pool)
            .await
            .unwrap();
        });

        let result = store
            .get_embeddings_by_hashes(&[good.content_hash.as_str(), bad_hash.as_str()])
            .unwrap();

        assert!(
            result.contains_key(&good.content_hash),
            "good embedding should be returned"
        );
        assert!(
            !result.contains_key(&bad_hash),
            "NaN-containing embedding must be filtered out, got keys: {:?}",
            result.keys().collect::<Vec<_>>()
        );
        for (h, e) in &result {
            assert!(
                e.as_slice().iter().all(|v| v.is_finite()),
                "all returned embeddings must be finite (hash={h})"
            );
        }
    }

    /// TC-ADV-1 paired test: same guard on the sibling
    /// `get_chunk_ids_and_embeddings_by_hashes` path (the one HNSW build
    /// actually consumes). A NaN-containing chunk must not appear in the
    /// returned `(id, embedding)` pairs.
    #[test]
    fn test_get_chunk_ids_and_embeddings_by_hashes_skips_nan_blobs() {
        let (store, _dir) = make_store();

        let good = test_chunk("good2", "fn good2() { 2 }");
        store
            .upsert_chunk(&good, &mock_embedding(1.0), Some(100))
            .unwrap();

        let bad_hash = "b".repeat(64);
        let bad_id = format!("test2.rs:9:{}", &bad_hash[..8]);
        let bad_bytes = nan_embedding_bytes();
        store.rt.block_on(async {
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                 signature, content, content_hash, doc, line_start, line_end, embedding,
                 source_mtime, created_at, updated_at)
                 VALUES (?1, 'nan2.rs', 'file', 'rust', 'function', 'nanfn2',
                 'fn nanfn2()', 'fn nanfn2() {}', ?2, NULL, 1, 5, ?3, 0,
                 '1970-01-01', '1970-01-01')",
            )
            .bind(&bad_id)
            .bind(&bad_hash)
            .bind(&bad_bytes)
            .execute(&store.pool)
            .await
            .unwrap();
        });

        let result = store
            .get_chunk_ids_and_embeddings_by_hashes(&[
                good.content_hash.as_str(),
                bad_hash.as_str(),
            ])
            .unwrap();

        // Good chunk present, bad chunk absent.
        let ids: Vec<&str> = result.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&good.id.as_str()), "good id missing: {ids:?}");
        assert!(
            !ids.contains(&bad_id.as_str()),
            "NaN-containing chunk id must be filtered out: {ids:?}"
        );
        // All returned embeddings are finite.
        for (id, emb) in &result {
            assert!(
                emb.as_slice().iter().all(|v| v.is_finite()),
                "embedding for id={id} must be finite"
            );
        }
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
