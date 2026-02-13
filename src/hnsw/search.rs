//! HNSW search implementation

use hnsw_rs::api::AnnT;

use crate::embedder::Embedding;
use crate::index::IndexResult;
use crate::EMBEDDING_DIM;

use super::{HnswIndex, EF_SEARCH};

impl HnswIndex {
    /// Search for nearest neighbors (inherent implementation).
    ///
    /// This is the actual search implementation. The `VectorIndex` trait method
    /// delegates to this inherent method. Both methods have identical signatures
    /// and behavior - use whichever is more convenient at the call site.
    ///
    /// # Arguments
    /// * `query` - Query embedding (769-dim: 768 model + 1 sentiment)
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of (chunk_id, score) pairs, sorted by descending score
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        if self.id_map.is_empty() {
            return Vec::new();
        }

        if query.len() != EMBEDDING_DIM {
            tracing::warn!(
                "Query dimension mismatch: expected {}, got {}",
                EMBEDDING_DIM,
                query.len()
            );
            return Vec::new();
        }

        let neighbors = self
            .inner
            .with_hnsw(|h| h.search_neighbours(query.as_slice(), k, EF_SEARCH));

        neighbors
            .into_iter()
            .filter_map(|n| {
                let idx = n.d_id;
                if idx < self.id_map.len() {
                    // Convert distance to similarity score
                    // Cosine distance is 1 - cosine_similarity, so we convert back
                    let score = 1.0 - n.distance;
                    Some(IndexResult {
                        id: self.id_map[idx].clone(),
                        score,
                    })
                } else {
                    tracing::warn!("Invalid index {} in HNSW result", idx);
                    None
                }
            })
            .collect()
    }
}
