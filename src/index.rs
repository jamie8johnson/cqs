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
/// async contexts like the MCP server.
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
