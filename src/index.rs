//! Vector index trait for nearest neighbor search
//!
//! Abstracts over different index implementations (HNSW, CAGRA, etc.)
//! to enable runtime selection based on hardware availability.

use crate::embedder::Embedding;

/// Result from a vector index search
#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Chunk ID (matches Store chunk IDs)
    pub id: String,
    /// Similarity score (0.0 to 1.0, higher is more similar)
    pub score: f32,
}

/// Trait for vector similarity search indexes
/// Implementations must be thread-safe (`Send + Sync`) for use in
/// async contexts like the sqlx store.
pub trait VectorIndex: Send + Sync {
    /// Search for nearest neighbors
    /// # Arguments
    /// * `query` - Query embedding vector (dimension depends on configured model)
    /// * `k` - Maximum number of results to return
    /// # Returns
    /// Results sorted by descending similarity score
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult>;

    /// Number of vectors in the index
    fn len(&self) -> usize;

    /// Check if the index is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Index type name (e.g., "HNSW", "CAGRA")
    fn name(&self) -> &'static str;

    /// Embedding dimension of vectors in this index
    fn dim(&self) -> usize;

    /// Search with traversal-time filtering.
    ///
    /// The predicate receives a chunk_id and returns true to keep the candidate.
    /// HNSW overrides this with traversal-time filtering (skips non-matching nodes
    /// during graph walk). Default impl over-fetches and post-filters.
    fn search_with_filter(
        &self,
        query: &Embedding,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        // Default: over-fetch unfiltered, post-filter by chunk_id
        let results: Vec<IndexResult> = self
            .search(query, k * 3)
            .into_iter()
            .filter(|r| filter(&r.id))
            .take(k)
            .collect();
        // AC-7: Warn when post-filter yields fewer results than requested.
        // This indicates the filter is too restrictive relative to the over-fetch
        // multiplier (3x), or the index is too small.
        if results.len() < k && self.len() >= k {
            tracing::warn!(
                returned = results.len(),
                requested = k,
                index_size = self.len(),
                "Filter-aware search under-returned"
            );
        }
        results
    }

    /// RM-V1.25-19: Has this index observed a panic / poisoned mutex mid-op?
    ///
    /// The default answer is `false`; only the CAGRA GPU backend currently
    /// tracks this. When `true`, the caller should discard the index and
    /// rebuild rather than continue searching against possibly-corrupted
    /// CUDA state (stream in a bad posture, buffer ownership ambiguous,
    /// etc.). HNSW / brute-force search don't manage any GPU context so
    /// they never surface poisoning.
    fn is_poisoned(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock VectorIndex for testing trait behavior
    struct MockIndex {
        results: Vec<IndexResult>,
        size: usize,
        dim: usize,
    }

    impl MockIndex {
        /// Creates a new instance with an empty results vector and a specified size capacity.
        /// # Arguments
        /// * `size` - The maximum capacity or size limit for this instance
        /// # Returns
        /// A new `Self` instance with an empty results vector and the given size value
        fn new(size: usize) -> Self {
            Self {
                results: Vec::new(),
                size,
                dim: crate::EMBEDDING_DIM,
            }
        }

        /// Creates a new instance with the given index results.
        /// # Arguments
        /// * `results` - A vector of IndexResult items to store in this instance
        /// # Returns
        /// A new Self instance initialized with the provided results and their count.
        fn with_results(results: Vec<IndexResult>) -> Self {
            let size = results.len();
            Self {
                results,
                size,
                dim: crate::EMBEDDING_DIM,
            }
        }
    }

    impl VectorIndex for MockIndex {
        /// Retrieves the top k search results from the stored results.
        /// # Arguments
        /// * `_query` - An embedding query (unused in this implementation)
        /// * `k` - The number of top results to return
        /// # Returns
        /// A vector of up to k `IndexResult` items, cloned from the internal results storage.
        fn search(&self, _query: &Embedding, k: usize) -> Vec<IndexResult> {
            self.results.iter().take(k).cloned().collect()
        }

        /// Returns the number of elements currently stored in the collection.
        /// # Returns
        /// The total count of elements in the collection as a `usize`.
        fn len(&self) -> usize {
            self.size
        }

        /// Returns the name of this mock object.
        /// # Returns
        /// A static string slice containing the name "Mock".
        fn name(&self) -> &'static str {
            "Mock"
        }

        fn dim(&self) -> usize {
            self.dim
        }
    }

    #[test]
    fn test_index_result_fields() {
        let result = IndexResult {
            id: "chunk_1".to_string(),
            score: 0.95,
        };
        assert_eq!(result.id, "chunk_1");
        assert!((result.score - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn test_default_is_empty() {
        let empty = MockIndex::new(0);
        assert!(empty.is_empty());

        let nonempty = MockIndex::new(5);
        assert!(!nonempty.is_empty());
    }

    #[test]
    fn test_mock_search() {
        let index = MockIndex::with_results(vec![
            IndexResult {
                id: "a".into(),
                score: 0.9,
            },
            IndexResult {
                id: "b".into(),
                score: 0.8,
            },
            IndexResult {
                id: "c".into(),
                score: 0.7,
            },
        ]);
        let query = Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        let results = index.search(&query, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "a");
        assert_eq!(results[1].id, "b");
    }

    #[test]
    fn test_trait_object_dispatch() {
        let index: Box<dyn VectorIndex> = Box::new(MockIndex::new(42));
        assert_eq!(index.len(), 42);
        assert!(!index.is_empty());
        assert_eq!(index.name(), "Mock");
    }
}
