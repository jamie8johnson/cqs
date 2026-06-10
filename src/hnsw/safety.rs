//! Safety tests for LoadedHnsw self-referential pattern
//!
//! LoadedHnsw uses `self_cell` to manage the self-referential `HnswIo` → `Hnsw`
//! relationship. These tests verify the pattern works correctly under various
//! conditions: repeated searches, lifecycle, concurrent access, and layout.
//!
//! # hnsw_rs version dependency
//!
//! The `HnswIoCell` wrapper uses one `UnsafeCell` access during construction.
//! If upgrading hnsw_rs, re-run these tests and verify behavior.

#[cfg(test)]
mod tests {
    use std::mem::size_of;
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    use crate::embedder::Embedding;
    use crate::hnsw::{HnswIndex, HnswInner, LoadedHnsw};
    use crate::EMBEDDING_DIM;

    /// Create a well-separated embedding for testing.
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
        let embeddings: Vec<_> = (1..=20)
            .map(|i| (format!("chunk{}", i), make_embedding(i)))
            .collect();
        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), "safety_test").unwrap();

        // Load and perform many searches
        let loaded =
            HnswIndex::load_with_dim(tmp.path(), "safety_test", crate::EMBEDDING_DIM).unwrap();
        assert_eq!(loaded.len(), 20);

        // Multiple searches should all succeed without memory corruption.
        // This test validates the self-referential LoadedHnsw pattern,
        // not HNSW recall accuracy — so we only assert results are non-empty.
        for i in 1..=20 {
            let query = make_embedding(i);
            let results = loaded.search(&query, 5);
            assert!(
                !results.is_empty(),
                "Search {} should return results (memory corruption check)",
                i
            );
            // Verify result IDs are valid (not garbage from memory corruption)
            for r in &results {
                assert!(
                    r.id.starts_with("chunk"),
                    "Result ID '{}' looks corrupted",
                    r.id
                );
            }
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
        // Env lock scoped to the build: build_with_dim reads CQS_HNSW_*
        // and must not race a concurrent env-override test.
        {
            let _env = crate::hnsw::HNSW_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM)
                .unwrap()
                .save(tmp.path(), "lifecycle")
                .unwrap();
        }

        // Load-search-drop cycle multiple times.
        //
        // This test pins the self-referential `LoadedHnsw` lifecycle — that
        // load → search → drop does not trigger use-after-free or return
        // corrupted IDs. It deliberately does NOT assert HNSW recall.
        //
        // Why no recall assertion: the graph is built via
        // `parallel_insert_data` (rayon) and hnsw_rs seeds its layer
        // generator from OS entropy (`StdRng::from_os_rng`). Under CPU
        // saturation the concurrent entry-point race in `check_entry_point`
        // produces a degenerate topology on ~1-2% of builds where even the
        // exact self-match vector (cosine distance 0 to "a") is unreachable.
        // Measured: parallel insert = 52/3000 self-misses under 16-core load;
        // sequential insert = 0/3000. This is an hnsw_rs concurrent-build
        // recall characteristic, not a cqs soundness bug — search on a fixed
        // index is deterministic (0/100k misses). Asserting "a" appears would
        // make this test flake under full-suite parallelism. Lifecycle
        // soundness is what safety.rs guards; recall lives in build/persist
        // tests with bounded build retries.
        for cycle in 0..5 {
            let loaded =
                HnswIndex::load_with_dim(tmp.path(), "lifecycle", crate::EMBEDDING_DIM).unwrap();
            assert_eq!(loaded.len(), 3, "Cycle {} loaded wrong count", cycle);
            let results = loaded.search(&make_embedding(100), 3);
            // Soundness, not recall: results are non-empty and every returned
            // ID is one of the three we inserted (garbage IDs would indicate
            // memory corruption in the self-cell pattern).
            assert!(!results.is_empty(), "Cycle {} returned no results", cycle);
            for r in &results {
                assert!(
                    matches!(r.id.as_str(), "a" | "b" | "c"),
                    "Cycle {} returned corrupted ID {:?}",
                    cycle,
                    r.id
                );
                assert!(
                    r.score.is_finite(),
                    "Cycle {} returned non-finite score for {:?}",
                    cycle,
                    r.id
                );
            }
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
        HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM)
            .unwrap()
            .save(tmp.path(), "threaded")
            .unwrap();

        let loaded = Arc::new(
            HnswIndex::load_with_dim(tmp.path(), "threaded", crate::EMBEDDING_DIM).unwrap(),
        );

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

    /// Verify the memory layout is reasonable.
    #[test]
    fn test_layout_invariants() {
        // LoadedHnsw must be a reasonable size (not accidentally huge)
        let loaded_size = size_of::<LoadedHnsw>();
        assert!(
            loaded_size < 1024,
            "LoadedHnsw unexpectedly large: {} bytes",
            loaded_size
        );

        // HnswInner should be a reasonable size
        let inner_size = size_of::<HnswInner>();
        assert!(
            inner_size < 2048,
            "HnswInner unexpectedly large: {} bytes",
            inner_size
        );
    }

    /// Test behavior with a minimal index (single vector).
    /// Note: hnsw_rs cannot save/load empty indexes, so we test with 1 vector.
    #[test]
    fn test_loaded_minimal_index() {
        let tmp = TempDir::new().unwrap();

        let index = HnswIndex::build_with_dim(
            vec![("only".to_string(), make_embedding(42))],
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        index.save(tmp.path(), "minimal").unwrap();

        let loaded = HnswIndex::load_with_dim(tmp.path(), "minimal", crate::EMBEDDING_DIM).unwrap();
        assert_eq!(loaded.len(), 1);

        let results = loaded.search(&make_embedding(42), 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "only");
    }
}
