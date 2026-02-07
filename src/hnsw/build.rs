//! HNSW index construction

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;

use crate::embedder::Embedding;
use crate::EMBEDDING_DIM;

use super::{HnswError, HnswIndex, HnswInner, EF_CONSTRUCTION, MAX_LAYER, MAX_NB_CONNECTION};

impl HnswIndex {
    /// Build a new HNSW index from embeddings (single-pass).
    ///
    /// # When to use `build` vs `build_batched`
    ///
    /// - **`build`**: Use when all embeddings fit comfortably in memory (<50k chunks,
    ///   ~150MB for 50k × 769 × 4 bytes). Slightly higher graph quality since all
    ///   vectors are available during construction.
    ///
    /// - **`build_batched`**: Use for large indexes (>50k chunks) or memory-constrained
    ///   environments. Streams embeddings in batches to avoid OOM. Graph quality is
    ///   marginally lower but negligible for practical search accuracy.
    ///
    /// **Warning:** This loads all embeddings into memory at once.
    /// For large indexes (>50k chunks), prefer `build_batched()` to avoid OOM.
    ///
    /// # Deprecation Notice
    ///
    /// This method is soft-deprecated for new code. Prefer `build_batched()` which:
    /// - Streams embeddings in configurable batch sizes
    /// - Avoids OOM on large indexes
    /// - Has negligible quality difference in practice
    ///
    /// # Arguments
    /// * `embeddings` - Vector of (chunk_id, embedding) pairs
    pub fn build(embeddings: Vec<(String, Embedding)>) -> Result<Self, HnswError> {
        if embeddings.is_empty() {
            // Create empty index
            let hnsw = Hnsw::new(MAX_NB_CONNECTION, 1, MAX_LAYER, EF_CONSTRUCTION, DistCosine);
            return Ok(Self {
                inner: HnswInner::Owned(hnsw),
                id_map: Vec::new(),
            });
        }

        // Validate dimensions
        for (id, emb) in &embeddings {
            if emb.len() != EMBEDDING_DIM {
                return Err(HnswError::DimensionMismatch {
                    expected: EMBEDDING_DIM,
                    actual: emb.len(),
                });
            }
            tracing::trace!("Adding {} to HNSW index", id);
        }

        let nb_elem = embeddings.len();
        tracing::info!("Building HNSW index with {} vectors", nb_elem);

        // Create HNSW with cosine distance
        let mut hnsw = Hnsw::new(
            MAX_NB_CONNECTION,
            nb_elem,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistCosine,
        );

        // Build ID map and prepare data for insertion
        let mut id_map = Vec::with_capacity(nb_elem);
        let mut data_for_insert: Vec<(&Vec<f32>, usize)> = Vec::with_capacity(nb_elem);

        for (idx, (chunk_id, embedding)) in embeddings.iter().enumerate() {
            id_map.push(chunk_id.clone());
            data_for_insert.push((embedding.as_vec(), idx));
        }

        // Parallel insert for performance
        hnsw.parallel_insert_data(&data_for_insert);

        tracing::info!("HNSW index built successfully");

        Ok(Self {
            inner: HnswInner::Owned(hnsw),
            id_map,
        })
    }

    /// Build HNSW index incrementally from batches (memory-efficient).
    ///
    /// Processes embeddings in batches to avoid loading everything into RAM.
    /// Each batch is inserted via `parallel_insert`, building the graph incrementally.
    ///
    /// Memory usage: O(batch_size) instead of O(total_embeddings).
    /// Trade-off: Slightly lower graph quality vs. single-pass build, but
    /// negligible for practical search accuracy.
    ///
    /// # Arguments
    /// * `batches` - Iterator yielding `Result<Vec<(id, embedding)>>` batches
    /// * `estimated_total` - Hint for HNSW capacity (can be approximate)
    ///
    /// # Example
    /// ```ignore
    /// let index = HnswIndex::build_batched(
    ///     store.embedding_batches(10_000),
    ///     store.chunk_count()?,
    /// )?;
    /// ```
    pub fn build_batched<I, E>(batches: I, estimated_total: usize) -> Result<Self, HnswError>
    where
        I: Iterator<Item = Result<Vec<(String, Embedding)>, E>>,
        E: std::fmt::Display,
    {
        let capacity = estimated_total.max(1);
        tracing::info!(
            "Building HNSW index incrementally (estimated {} vectors)",
            capacity
        );

        let mut hnsw = Hnsw::new(
            MAX_NB_CONNECTION,
            capacity,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistCosine,
        );

        let mut id_map: Vec<String> = Vec::with_capacity(capacity);
        let mut total_inserted = 0usize;

        for batch_result in batches {
            let batch = batch_result
                .map_err(|e| HnswError::Internal(format!("Batch fetch failed: {}", e)))?;

            if batch.is_empty() {
                continue;
            }

            // Validate dimensions for this batch
            for (id, emb) in &batch {
                if emb.len() != EMBEDDING_DIM {
                    return Err(HnswError::DimensionMismatch {
                        expected: EMBEDDING_DIM,
                        actual: emb.len(),
                    });
                }
                tracing::trace!("Adding {} to HNSW index", id);
            }

            // Build insertion data for this batch
            // IDs are assigned sequentially starting from current id_map length
            let base_idx = id_map.len();
            let mut data_for_insert: Vec<(&Vec<f32>, usize)> = Vec::with_capacity(batch.len());

            for (i, (chunk_id, embedding)) in batch.iter().enumerate() {
                id_map.push(chunk_id.clone());
                data_for_insert.push((embedding.as_vec(), base_idx + i));
            }

            // Insert this batch (hnsw_rs supports consecutive parallel_insert calls)
            hnsw.parallel_insert_data(&data_for_insert);

            total_inserted += batch.len();
            let progress_pct = if capacity > 0 {
                (total_inserted * 100) / capacity
            } else {
                100
            };
            tracing::info!(
                "HNSW build progress: {} / ~{} vectors ({}%)",
                total_inserted,
                capacity,
                progress_pct
            );
        }

        if id_map.is_empty() {
            tracing::info!("HNSW index built (empty)");
            return Ok(Self {
                inner: HnswInner::Owned(Hnsw::new(
                    MAX_NB_CONNECTION,
                    1,
                    MAX_LAYER,
                    EF_CONSTRUCTION,
                    DistCosine,
                )),
                id_map: Vec::new(),
            });
        }

        tracing::info!("HNSW index built: {} vectors", id_map.len());

        Ok(Self {
            inner: HnswInner::Owned(hnsw),
            id_map,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_embedding(seed: u32) -> Embedding {
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        for (i, val) in v.iter_mut().enumerate() {
            *val = ((seed as f32 * 0.1) + (i as f32 * 0.001)).sin();
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut v {
                *val /= norm;
            }
        }
        Embedding::new(v)
    }

    #[test]
    fn test_build_and_search() {
        let embeddings = vec![
            ("chunk1".to_string(), make_embedding(1)),
            ("chunk2".to_string(), make_embedding(2)),
            ("chunk3".to_string(), make_embedding(3)),
        ];

        let index = HnswIndex::build(embeddings).unwrap();
        assert_eq!(index.len(), 3);

        // Search for something similar to chunk1
        let query = make_embedding(1);
        let results = index.search(&query, 3);

        assert!(!results.is_empty());
        // The most similar should be chunk1 itself
        assert_eq!(results[0].id, "chunk1");
        assert!(results[0].score > 0.9); // Should be very similar
    }

    #[test]
    fn test_empty_index() {
        let index = HnswIndex::build(vec![]).unwrap();
        assert!(index.is_empty());

        let query = make_embedding(1);
        let results = index.search(&query, 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_build_batched() {
        // Simulate streaming batches like Store::embedding_batches would provide
        let all_embeddings: Vec<(String, Embedding)> = (1..=25)
            .map(|i| (format!("chunk{}", i), make_embedding(i)))
            .collect();

        // Split into batches of 10 (simulating LIMIT/OFFSET pagination)
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> =
            all_embeddings
                .chunks(10)
                .map(|chunk| Ok(chunk.to_vec()))
                .collect();

        let index = HnswIndex::build_batched(batches.into_iter(), 25).unwrap();
        assert_eq!(index.len(), 25);

        // Search should work correctly
        let query = make_embedding(1);
        let results = index.search(&query, 5);
        assert!(!results.is_empty());
        // chunk1 should be in top results
        assert!(results.iter().any(|r| r.id == "chunk1"));
    }

    #[test]
    fn test_build_batched_empty() {
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = vec![];
        let index = HnswIndex::build_batched(batches.into_iter(), 0).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_build_batched_vs_regular_equivalence() {
        // Build same index both ways, verify similar search results
        let embeddings: Vec<(String, Embedding)> = (1..=20)
            .map(|i| (format!("item{}", i), make_embedding(i)))
            .collect();

        let regular = HnswIndex::build(embeddings.clone()).unwrap();

        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = embeddings
            .chunks(7) // Odd batch size to test edge cases
            .map(|chunk| Ok(chunk.to_vec()))
            .collect();
        let batched = HnswIndex::build_batched(batches.into_iter(), 20).unwrap();

        assert_eq!(regular.len(), batched.len());

        // Both should find the same items (though scores may differ slightly)
        let query = make_embedding(10);
        let regular_results = regular.search(&query, 5);
        let batched_results = batched.search(&query, 5);

        // item10 should be top result for both
        assert_eq!(regular_results[0].id, "item10");
        assert_eq!(batched_results[0].id, "item10");
    }
}
