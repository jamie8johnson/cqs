//! Safety tests for LoadedHnsw self-referential pattern
//!
//! The LoadedHnsw struct uses a raw pointer and lifetime transmute to handle
//! hnsw_rs's borrowing API. These tests verify memory safety invariants.
//!
//! # CRITICAL: hnsw_rs version dependency
//!
//! The safety of LoadedHnsw depends on hnsw_rs internals:
//! - `HnswIo::load_hnsw()` must return `Hnsw<'a>` borrowing from `&'a mut HnswIo`
//! - The `Hnsw` must only read (not mutate) data owned by `HnswIo`
//! - Memory layout of `Hnsw` must not change in incompatible ways
//!
//! If upgrading hnsw_rs, re-run these tests and verify no UB with miri if possible.

#[cfg(test)]
mod tests {
    use std::mem::{align_of, size_of};
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    use hnsw_rs::anndists::dist::distances::DistCosine;
    use hnsw_rs::hnsw::Hnsw;
    use hnsw_rs::hnswio::HnswIo;

    use crate::embedder::Embedding;
    use crate::hnsw::{HnswIndex, HnswInner, LoadedHnsw};
    use crate::EMBEDDING_DIM;

    /// Create a well-separated embedding for testing.
    ///
    /// Uses one-hot-like vectors: a strong signal in a unique dimension per seed,
    /// with a small baseline everywhere else. This produces cosine similarity ~0.01
    /// between different seeds and 1.0 for same seed, ensuring HNSW always ranks
    /// the correct match first.
    fn make_embedding(seed: u32) -> Embedding {
        let mut v = vec![0.01f32; EMBEDDING_DIM];
        // Place a strong signal in a unique dimension for each seed
        let idx = (seed as usize) % EMBEDDING_DIM;
        v[idx] = 1.0;
        // L2 normalize
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for val in &mut v {
            *val /= norm;
        }
        Embedding::new(v)
    }

    /// Test that loaded index survives multiple search operations.
    /// This exercises the self-referential pattern under repeated use.
    #[test]
    fn test_loaded_index_multiple_searches() {
        let tmp = TempDir::new().unwrap();

        // Build and save an index with several vectors
        let embeddings: Vec<_> = (1..=10)
            .map(|i| (format!("chunk{}", i), make_embedding(i)))
            .collect();
        let index = HnswIndex::build(embeddings).unwrap();
        index.save(tmp.path(), "safety_test").unwrap();

        // Load and perform many searches
        let loaded = HnswIndex::load(tmp.path(), "safety_test").unwrap();
        assert_eq!(loaded.len(), 10);

        // Multiple searches should all succeed without memory corruption
        for i in 1..=10 {
            let query = make_embedding(i);
            let results = loaded.search(&query, 5);
            assert!(!results.is_empty(), "Search {} should return results", i);

            // With well-separated embeddings, the correct chunk should always be first
            let expected_id = format!("chunk{}", i);
            assert_eq!(
                results[0].id, expected_id,
                "Search {} should find chunk{} as top result, got: {:?}",
                i, i, results[0].id
            );

            // The best match should have high similarity
            assert!(
                results[0].score > 0.9,
                "Best match should have high similarity, got {}",
                results[0].score
            );
        }
    }

    /// Test that loading, searching, and dropping work correctly in sequence.
    /// Verifies drop order doesn't cause use-after-free.
    #[test]
    fn test_loaded_index_lifecycle() {
        let tmp = TempDir::new().unwrap();

        let embeddings = vec![
            ("a".to_string(), make_embedding(100)),
            ("b".to_string(), make_embedding(200)),
            ("c".to_string(), make_embedding(300)),
        ];
        HnswIndex::build(embeddings)
            .unwrap()
            .save(tmp.path(), "lifecycle")
            .unwrap();

        // Load-search-drop cycle multiple times
        for cycle in 0..5 {
            let loaded = HnswIndex::load(tmp.path(), "lifecycle").unwrap();
            let results = loaded.search(&make_embedding(100), 3);
            assert_eq!(results[0].id, "a", "Cycle {} failed", cycle);
            // Drop happens here
        }
    }

    /// Test concurrent access from multiple threads.
    /// LoadedHnsw is marked Send+Sync, this verifies it's actually safe.
    #[test]
    fn test_loaded_index_threaded_access() {
        let tmp = TempDir::new().unwrap();

        let embeddings: Vec<_> = (1..=20)
            .map(|i| (format!("item{}", i), make_embedding(i)))
            .collect();
        HnswIndex::build(embeddings)
            .unwrap()
            .save(tmp.path(), "threaded")
            .unwrap();

        let loaded = Arc::new(HnswIndex::load(tmp.path(), "threaded").unwrap());

        // Spawn multiple threads doing concurrent searches
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let index = Arc::clone(&loaded);
                thread::spawn(move || {
                    for i in 1..=20 {
                        let query = make_embedding(i);
                        let results = index.search(&query, 3);
                        assert!(!results.is_empty(), "Thread {} search {} failed", t, i);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("Thread panicked");
        }
    }

    /// Verify the memory layout assumptions documented in LoadedHnsw.
    /// These are compile-time checks via static assertions.
    #[test]
    fn test_layout_invariants() {
        // LoadedHnsw must be a reasonable size (not accidentally huge)
        let loaded_size = size_of::<LoadedHnsw>();
        assert!(
            loaded_size < 1024,
            "LoadedHnsw unexpectedly large: {} bytes",
            loaded_size
        );

        // Pointer alignment check
        assert_eq!(
            align_of::<*mut HnswIo>(),
            align_of::<usize>(),
            "Pointer alignment unexpected"
        );

        // HnswInner should be efficient - no excessive padding
        let inner_size = size_of::<HnswInner>();
        let owned_size = size_of::<Hnsw<'static, f32, DistCosine>>();
        // Inner should be at most slightly larger than the largest variant
        assert!(
            inner_size <= owned_size + 32,
            "HnswInner has excessive padding: {} vs {}",
            inner_size,
            owned_size
        );
    }

    /// Test behavior with a minimal index (single vector).
    /// Note: hnsw_rs cannot save/load empty indexes, so we test with 1 vector.
    #[test]
    fn test_loaded_minimal_index() {
        let tmp = TempDir::new().unwrap();

        let index = HnswIndex::build(vec![("only".to_string(), make_embedding(42))]).unwrap();
        index.save(tmp.path(), "minimal").unwrap();

        let loaded = HnswIndex::load(tmp.path(), "minimal").unwrap();
        assert_eq!(loaded.len(), 1);

        let results = loaded.search(&make_embedding(42), 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "only");
    }
}
