//! HNSW index construction

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;

use crate::embedder::Embedding;

use super::{HnswError, HnswIndex, HnswInner, MAX_LAYER};

impl HnswIndex {
    /// Build a new HNSW index from embeddings (single-pass).
    ///
    /// # When to use `build_with_dim` vs `build_batched_with_dim`
    ///
    /// - **`build_with_dim`**: Use when all embeddings fit comfortably in memory
    ///   (<50k chunks, ~150MB for 50k x 768 x 4 bytes). Slightly higher graph
    ///   quality since all vectors are available during construction.
    ///
    /// - **`build_batched_with_dim`**: Use for large indexes (>50k chunks) or
    ///   memory-constrained environments. Streams embeddings in batches to avoid
    ///   OOM. Graph quality is marginally lower but negligible for practical
    ///   search accuracy.
    ///
    /// **Warning:** This loads all embeddings into memory at once.
    /// For large indexes (>50k chunks), prefer `build_batched_with_dim()` to
    /// avoid OOM.
    ///
    /// # Deprecation Notice
    ///
    /// This method is soft-deprecated for new code. Prefer
    /// `build_batched_with_dim()` which:
    /// - Streams embeddings in configurable batch sizes
    /// - Avoids OOM on large indexes
    /// - Has negligible quality difference in practice
    ///
    /// # Production routing
    ///
    /// `build_hnsw_index()` in `cli/commands/index.rs` unconditionally uses
    /// `build_batched_with_dim()` with 10k-row batches for all index sizes.
    /// This method is only used in tests.
    ///
    /// (Historical note: the earlier `build_batched()` convenience wrapper that
    /// hardcoded 768-dim was removed in v0.9.0 as part of the configurable-model
    /// migration — see "configurable models disaster" in `CLAUDE.md`.)
    ///
    /// # Arguments
    /// * `embeddings` - Vector of (chunk_id, embedding) pairs
    /// * `dim` - Expected embedding dimension
    pub fn build_with_dim(
        embeddings: Vec<(String, Embedding)>,
        dim: usize,
    ) -> Result<Self, HnswError> {
        let _span = tracing::debug_span!("hnsw_build").entered();
        if dim == 0 {
            return Err(HnswError::Build("Embedding dimension must be > 0".into()));
        }
        if embeddings.is_empty() {
            // Create empty index
            let hnsw = Hnsw::new(
                super::max_nb_connection(),
                1,
                MAX_LAYER,
                super::ef_construction(),
                DistCosine,
            );
            return Ok(Self {
                inner: HnswInner::Owned(hnsw),
                id_map: Vec::new(),
                ef_search: super::ef_search(),
                dim,
                _lock_file: None,
            });
        }

        let (id_map, data, nb_elem) = super::prepare_index_data(embeddings, dim)?;

        tracing::info!(count = nb_elem, "Building HNSW index");

        // Create HNSW with cosine distance
        let mut hnsw = Hnsw::new(
            super::max_nb_connection(),
            nb_elem,
            MAX_LAYER,
            super::ef_construction(),
            DistCosine,
        );

        // Test-only path: allocates the full Vec<Vec<f32>> double-buffer here.
        // Production code uses `build_batched` to avoid this peak allocation.
        // Reconstruct Vec<f32> chunks from flat buffer for hnsw_rs API
        let chunks: Vec<Vec<f32>> = data.chunks_exact(dim).map(|c| c.to_vec()).collect();
        let data_for_insert: Vec<(&Vec<f32>, usize)> =
            chunks.iter().enumerate().map(|(i, v)| (v, i)).collect();

        // Parallel insert for performance
        hnsw.parallel_insert_data(&data_for_insert);

        tracing::info!("HNSW index built successfully");

        Ok(Self {
            inner: HnswInner::Owned(hnsw),
            id_map,
            ef_search: super::ef_search(),
            dim,
            _lock_file: None,
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
    /// * `dim` - Expected embedding dimension
    ///
    /// # Example
    /// ```ignore
    /// let index = HnswIndex::build_batched_with_dim(
    ///     store.embedding_batches(10_000),
    ///     store.chunk_count()?,
    ///     768,
    /// )?;
    /// ```
    pub fn build_batched_with_dim<I, E>(
        batches: I,
        estimated_total: usize,
        dim: usize,
    ) -> Result<Self, HnswError>
    where
        I: Iterator<Item = Result<Vec<(String, Embedding)>, E>>,
        E: std::fmt::Display,
    {
        let _span = tracing::debug_span!("hnsw_build_batched", estimated_total).entered();
        if dim == 0 {
            return Err(HnswError::Build("Embedding dimension must be > 0".into()));
        }
        let capacity = estimated_total.max(1);
        tracing::info!(
            "Building HNSW index incrementally (estimated {} vectors)",
            capacity
        );

        let mut hnsw = Hnsw::new(
            super::max_nb_connection(),
            capacity,
            MAX_LAYER,
            super::ef_construction(),
            DistCosine,
        );

        let mut id_map: Vec<String> = Vec::with_capacity(capacity);
        let mut total_inserted = 0usize;
        let mut batch_num = 0usize;

        for batch_result in batches {
            let batch = batch_result
                .map_err(|e| HnswError::Internal(format!("Batch fetch failed: {}", e)))?;

            if batch.is_empty() {
                continue;
            }

            // Validate dimensions and build insertion data in a single pass.
            // Use a separate insertion counter (not loop index) because zero-vector
            // skips would desync base_idx+i from id_map positions. (RT-DATA-1)
            let mut data_for_insert: Vec<(&Vec<f32>, usize)> = Vec::with_capacity(batch.len());

            for (chunk_id, embedding) in batch.iter() {
                if embedding.len() != dim {
                    return Err(HnswError::DimensionMismatch {
                        expected: dim,
                        actual: embedding.len(),
                    });
                }
                // Skip zero-vector embeddings — they produce NaN cosine distances
                // PERF-37: short-circuit on first non-zero element instead of full L2 norm
                if !embedding.as_vec().iter().any(|x| *x != 0.0) {
                    tracing::warn!(chunk_id = %chunk_id, "Skipping zero-vector embedding");
                    continue;
                }
                let insert_idx = id_map.len();
                tracing::trace!("Adding {} to HNSW index at {}", chunk_id, insert_idx);
                id_map.push(chunk_id.clone());
                data_for_insert.push((embedding.as_vec(), insert_idx));
            }

            // Insert this batch (hnsw_rs supports consecutive parallel_insert calls)
            hnsw.parallel_insert_data(&data_for_insert);

            total_inserted += data_for_insert.len();
            batch_num += 1;
            tracing::debug!(
                batch = batch_num,
                vectors_so_far = total_inserted,
                "HNSW batch inserted"
            );
            let progress_pct = (total_inserted * 100).checked_div(capacity).unwrap_or(100);
            // PF-V1.25-15: demoted from info! to debug!. A 60k-chunk build
            // emits 50+ batches; at info level they drown out meaningful
            // log messages with no user-actionable signal per batch.
            // The final "HNSW index built" message at end-of-build (emitted
            // as info!) is sufficient for progress tracking.
            tracing::debug!(
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
                    super::max_nb_connection(),
                    1,
                    MAX_LAYER,
                    super::ef_construction(),
                    DistCosine,
                )),
                id_map: Vec::new(),
                ef_search: super::ef_search(),
                dim,
                _lock_file: None,
            });
        }

        tracing::info!(count = id_map.len(), "HNSW index built");

        Ok(Self {
            inner: HnswInner::Owned(hnsw),
            id_map,
            ef_search: super::ef_search(),
            dim,
            _lock_file: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::hnsw::make_test_embedding as make_embedding;

    #[test]
    fn test_build_and_search() {
        let embeddings = vec![
            ("chunk1".to_string(), make_embedding(1)),
            ("chunk2".to_string(), make_embedding(2)),
            ("chunk3".to_string(), make_embedding(3)),
        ];

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
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
        let index = HnswIndex::build_with_dim(vec![], crate::EMBEDDING_DIM).unwrap();
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

        let index =
            HnswIndex::build_batched_with_dim(batches.into_iter(), 25, crate::EMBEDDING_DIM)
                .unwrap();
        assert_eq!(index.len(), 25);

        // Search should work correctly. Top-10 (not top-5) for the same
        // reason `test_build_batched_vs_regular_equivalence` uses top-10:
        // HNSW batched builds on small N (25 here) have suboptimal recall —
        // the unseeded random projections occasionally bump the matching
        // chunk out of a top-5 window. Issue #1104 surfaced this as a CI
        // flake; the test exists to verify batched-build wiring round-trips
        // the right item, not to assert perfect recall.
        let query = make_embedding(1);
        let results = index.search(&query, 10);
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.id == "chunk1"));
    }

    #[test]
    fn test_build_batched_empty() {
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = vec![];
        let index = HnswIndex::build_batched_with_dim(batches.into_iter(), 0, crate::EMBEDDING_DIM)
            .unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_build_batched_vs_regular_equivalence() {
        // Build same index both ways, verify similar search results
        let embeddings: Vec<(String, Embedding)> = (1..=20)
            .map(|i| (format!("item{}", i), make_embedding(i)))
            .collect();

        let regular = HnswIndex::build_with_dim(embeddings.clone(), crate::EMBEDDING_DIM).unwrap();

        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = embeddings
            .chunks(7) // Odd batch size to test edge cases
            .map(|chunk| Ok(chunk.to_vec()))
            .collect();
        let batched =
            HnswIndex::build_batched_with_dim(batches.into_iter(), 20, crate::EMBEDDING_DIM)
                .unwrap();

        assert_eq!(regular.len(), batched.len());

        // Both should find the same items (though scores may differ slightly
        // due to HNSW's approximate nature and different graph construction order)
        let query = make_embedding(10);
        let regular_results = regular.search(&query, 10);
        let batched_results = batched.search(&query, 10);

        // item10 should appear in top results for both (use top-10 since
        // HNSW batched builds on small datasets can have suboptimal recall)
        let regular_found = regular_results.iter().any(|r| r.id == "item10");
        let batched_found = batched_results.iter().any(|r| r.id == "item10");
        assert!(regular_found, "Regular build should find item10 in top 10");
        assert!(batched_found, "Batched build should find item10 in top 10");
    }

    // ===== TC-31: multi-model dim-threading (HNSW build) =====

    /// Create a deterministic normalized embedding of arbitrary dimension.
    fn make_embedding_dim(seed: u32, dim: usize) -> Embedding {
        let mut v = vec![0.0f32; dim];
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
    fn tc31_build_batched_with_dim_1024() {
        // TC-31.4: Build HNSW index with 1024-dim embeddings via build_batched_with_dim.
        let all_embeddings: Vec<(String, Embedding)> = (1..=10)
            .map(|i| (format!("chunk{}", i), make_embedding_dim(i, 1024)))
            .collect();

        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> =
            all_embeddings
                .chunks(5)
                .map(|chunk| Ok(chunk.to_vec()))
                .collect();

        let index = HnswIndex::build_batched_with_dim(batches.into_iter(), 10, 1024).unwrap();
        assert_eq!(index.len(), 10, "HNSW index should have 10 vectors");
        assert_eq!(index.dim, 1024, "HNSW index dim should be 1024");

        // Search should work with 1024-dim queries
        let query = make_embedding_dim(1, 1024);
        let results = index.search(&query, 3);
        assert!(!results.is_empty(), "Search should return results");
        assert_eq!(results[0].id, "chunk1", "Nearest neighbor should be chunk1");
    }

    #[test]
    fn tc31_build_with_dim_1024() {
        // TC-31.4 variant: Single-pass build with 1024-dim.
        let embeddings: Vec<(String, Embedding)> = (1..=5)
            .map(|i| (format!("item{}", i), make_embedding_dim(i, 1024)))
            .collect();

        let index = HnswIndex::build_with_dim(embeddings, 1024).unwrap();
        assert_eq!(index.len(), 5);
        assert_eq!(index.dim, 1024);

        let query = make_embedding_dim(3, 1024);
        let results = index.search(&query, 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "item3");
    }

    #[test]
    fn tc31_build_batched_dim_mismatch_rejected() {
        // TC-31.4b: Feeding 128-dim embeddings to a 1024-dim build should fail.
        let bad_embeddings: Vec<(String, Embedding)> = (1..=3)
            .map(|i| {
                let mut v = vec![0.0f32; 128]; // intentionally wrong dim
                v[0] = i as f32;
                (format!("chunk{}", i), Embedding::new(v))
            })
            .collect();

        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> =
            vec![Ok(bad_embeddings)];

        let result = HnswIndex::build_batched_with_dim(batches.into_iter(), 3, 1024);
        assert!(
            result.is_err(),
            "build_batched_with_dim should reject dimension mismatch"
        );
        match result {
            Err(HnswError::DimensionMismatch { expected, actual }) => {
                assert_eq!(expected, 1024);
                assert_eq!(actual, 128);
            }
            Err(other) => panic!("Expected DimensionMismatch, got: {:?}", other),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn tc40_build_batched_with_dim_zero() {
        // TC-40 / RB-34: dim=0 should return an error (not panic on chunks_exact(0))
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = vec![];
        let result = HnswIndex::build_batched_with_dim(batches.into_iter(), 0, 0);
        assert!(result.is_err(), "dim=0 should return error");
        let err = result.err().unwrap();
        assert!(
            err.to_string().contains("must be > 0"),
            "error message should mention dimension requirement"
        );
    }

    #[test]
    fn tc40_build_batched_with_dim_zero_nonempty_errors() {
        // TC-40: dim=0 with actual embeddings should fail on dimension mismatch
        let embeddings: Vec<(String, Embedding)> = vec![("chunk1".to_string(), make_embedding(1))];
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> =
            vec![Ok(embeddings)];
        let result = HnswIndex::build_batched_with_dim(batches.into_iter(), 1, 0);
        assert!(
            result.is_err(),
            "dim=0 with non-empty embeddings should error on dimension mismatch"
        );
    }

    // ===== TC-17: HnswIndex::search_filtered predicate tests =====

    #[test]
    fn tc17_search_filtered_accepts_only_matching_ids() {
        // Build a 20-item index where IDs are "chunk_0" .. "chunk_19".
        // Filter accepts only even-numbered IDs. Assert no odd IDs appear.
        let embeddings: Vec<(String, Embedding)> = (0..20)
            .map(|i| (format!("chunk_{}", i), make_embedding(i)))
            .collect();

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        assert_eq!(index.len(), 20);

        let query = make_embedding(4); // bias toward chunk_4 (even)
        let results = index.search_filtered(&query, 10, &|id: &str| {
            // Accept only even-numbered chunk IDs
            id.strip_prefix("chunk_")
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n % 2 == 0)
        });

        // Every returned result must have an even ID
        for r in &results {
            let num: u32 = r.id.strip_prefix("chunk_").unwrap().parse().unwrap();
            assert_eq!(
                num % 2,
                0,
                "Rejected ID '{}' appeared in search_filtered results",
                r.id
            );
        }
        // Should have returned some results (we have 10 even IDs, asked for 10)
        assert!(
            !results.is_empty(),
            "search_filtered should return results when matches exist"
        );
    }

    #[test]
    fn tc17_search_filtered_all_rejected_returns_empty() {
        // Build a small index, then filter that rejects everything.
        let embeddings: Vec<(String, Embedding)> = (0..10)
            .map(|i| (format!("chunk_{}", i), make_embedding(i)))
            .collect();

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();

        let query = make_embedding(1);
        let results = index.search_filtered(&query, 5, &|_id: &str| false);

        assert!(
            results.is_empty(),
            "search_filtered with all-reject predicate should return empty, got {} results",
            results.len()
        );
    }
}
