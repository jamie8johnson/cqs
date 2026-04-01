//! HNSW search implementation

use hnsw_rs::api::AnnT;

use crate::embedder::Embedding;
use crate::index::IndexResult;

use super::HnswIndex;

impl HnswIndex {
    /// Search for nearest neighbors (inherent implementation).
    /// This is the actual search implementation. The `VectorIndex` trait method
    /// delegates to this inherent method. Both methods have identical signatures
    /// and behavior - use whichever is more convenient at the call site.
    /// # Arguments
    /// * `query` - Query embedding vector (dimension detected at runtime from model)
    /// * `k` - Maximum number of results to return
    /// # Returns
    /// Vector of (chunk_id, score) pairs, sorted by descending score
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        if self.id_map.is_empty() {
            return Vec::new();
        }

        let _span =
            tracing::debug_span!("hnsw_search", k, index_size = self.id_map.len()).entered();

        if query.is_empty() || query.len() != self.dim {
            if !query.is_empty() {
                tracing::warn!(
                    expected = self.dim,
                    actual = query.len(),
                    "Query embedding dimension mismatch"
                );
            }
            return Vec::new();
        }

        // Adaptive ef_search: baseline self.ef_search or 2*k (whichever is larger),
        // capped at index size (searching more than the index is pointless for small indexes).
        let index_size = self.id_map.len();
        let ef_search = self.ef_search.max(k * 2).min(index_size);

        let neighbors = self
            .inner
            .with_hnsw(|h| h.search_neighbours(query.as_slice(), k, ef_search));

        neighbors
            .into_iter()
            .filter_map(|n| {
                let idx = n.d_id;
                if idx < self.id_map.len() {
                    // Convert distance to similarity score
                    // Cosine distance is 1 - cosine_similarity, so we convert back
                    let score = 1.0 - n.distance;
                    if !score.is_finite() {
                        tracing::warn!(
                            idx,
                            distance = n.distance,
                            "Non-finite HNSW score, skipping"
                        );
                        return None;
                    }
                    Some(IndexResult {
                        id: self.id_map[idx].clone(),
                        score,
                    })
                } else {
                    tracing::warn!(idx, "Invalid index in HNSW result");
                    None
                }
            })
            .collect()
    }
}
