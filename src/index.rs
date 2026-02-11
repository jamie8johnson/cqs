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
///
/// Implementations must be thread-safe (`Send + Sync`) for use in
/// async contexts like the sqlx store.
pub trait VectorIndex: Send + Sync {
    /// Search for nearest neighbors
    ///
    /// # Arguments
    /// * `query` - Query embedding (769-dim: 768 model + 1 sentiment)
    /// * `k` - Maximum number of results to return
    ///
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock VectorIndex for testing trait behavior
    struct MockIndex {
        results: Vec<IndexResult>,
        size: usize,
    }

    impl MockIndex {
        fn new(size: usize) -> Self {
            Self {
                results: Vec::new(),
                size,
            }
        }

        fn with_results(results: Vec<IndexResult>) -> Self {
            let size = results.len();
            Self { results, size }
        }
    }

    impl VectorIndex for MockIndex {
        fn search(&self, _query: &Embedding, k: usize) -> Vec<IndexResult> {
            self.results.iter().take(k).cloned().collect()
        }

        fn len(&self) -> usize {
            self.size
        }

        fn name(&self) -> &'static str {
            "Mock"
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
        let query = Embedding::new(vec![0.0; 769]);
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
