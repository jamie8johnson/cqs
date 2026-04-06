//! In-memory inverted index for SPLADE sparse vectors.
//!
//! Loaded from SQLite at startup, queried during search.
//! Supports filtered search with the same chunk_type/language predicate
//! used by HNSW traversal-time filtering.

use std::collections::HashMap;

use crate::index::IndexResult;

use super::SparseVector;

/// In-memory inverted index for sparse vector search.
///
/// Structure: `token_id → [(chunk_index, weight)]`. For each vocabulary
/// token, stores which chunks contain it and how important it is.
pub struct SpladeIndex {
    /// Inverted postings: token_id → [(chunk_index, weight)]
    postings: HashMap<u32, Vec<(usize, f32)>>,
    /// Sequential chunk ID map (chunk_index → chunk_id string)
    id_map: Vec<String>,
}

impl SpladeIndex {
    /// Build from a list of (chunk_id, sparse_vector) pairs.
    pub fn build(chunks: Vec<(String, SparseVector)>) -> Self {
        let _span = tracing::info_span!("splade_index_build", chunks = chunks.len()).entered();

        let mut postings: HashMap<u32, Vec<(usize, f32)>> = HashMap::new();
        let mut id_map = Vec::with_capacity(chunks.len());

        for (idx, (chunk_id, sparse)) in chunks.into_iter().enumerate() {
            for &(token_id, weight) in &sparse {
                postings.entry(token_id).or_default().push((idx, weight));
            }
            id_map.push(chunk_id);
        }

        tracing::info!(
            unique_tokens = postings.len(),
            chunks = id_map.len(),
            "SPLADE index built"
        );

        Self { postings, id_map }
    }

    /// Search the inverted index (unfiltered).
    pub fn search(&self, query: &SparseVector, k: usize) -> Vec<IndexResult> {
        self.search_with_filter(query, k, &|_: &str| true)
    }

    /// Search with a chunk_id predicate filter.
    ///
    /// Computes dot product between query sparse vector and each document's
    /// sparse vector via the inverted index. Non-matching chunks (per filter)
    /// are skipped during score accumulation.
    pub fn search_with_filter(
        &self,
        query: &SparseVector,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        let _span = tracing::debug_span!(
            "splade_index_search",
            k,
            query_terms = query.len(),
            index_size = self.id_map.len()
        )
        .entered();

        if query.is_empty() || self.id_map.is_empty() {
            return Vec::new();
        }

        // Accumulate dot product scores per chunk
        let mut scores: HashMap<usize, f32> = HashMap::new();
        for &(token_id, query_weight) in query {
            if let Some(posting_list) = self.postings.get(&token_id) {
                for &(chunk_idx, doc_weight) in posting_list {
                    // Apply filter
                    if let Some(chunk_id) = self.id_map.get(chunk_idx) {
                        if !filter(chunk_id) {
                            continue;
                        }
                    } else {
                        continue;
                    }
                    *scores.entry(chunk_idx).or_insert(0.0) += query_weight * doc_weight;
                }
            }
        }

        // Sort by score descending, take top-k
        let mut results: Vec<_> = scores
            .into_iter()
            .filter_map(|(idx, score)| {
                self.id_map.get(idx).map(|id| IndexResult {
                    id: id.clone(),
                    score,
                })
            })
            .collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);

        tracing::debug!(results = results.len(), "SPLADE search complete");
        results
    }

    /// Number of chunks in the index.
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }

    /// Number of unique tokens in the index.
    pub fn unique_tokens(&self) -> usize {
        self.postings.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_index() -> SpladeIndex {
        SpladeIndex::build(vec![
            ("chunk_a".to_string(), vec![(1, 0.5), (2, 0.3), (3, 0.8)]),
            ("chunk_b".to_string(), vec![(1, 0.7), (4, 0.6)]),
            ("chunk_c".to_string(), vec![(2, 0.9), (3, 0.1), (5, 0.4)]),
        ])
    }

    #[test]
    fn test_build_empty() {
        let index = SpladeIndex::build(vec![]);
        assert!(index.is_empty());
        assert_eq!(index.unique_tokens(), 0);
    }

    #[test]
    fn test_build_and_search() {
        let index = make_test_index();
        assert_eq!(index.len(), 3);

        // Query that matches token 1 (in chunk_a and chunk_b)
        let results = index.search(&vec![(1, 1.0)], 10);
        assert!(!results.is_empty());
        // chunk_b has weight 0.7 for token 1, chunk_a has 0.5
        assert_eq!(results[0].id, "chunk_b");
        assert_eq!(results[1].id, "chunk_a");
    }

    #[test]
    fn test_dot_product_correct() {
        let index = make_test_index();
        // Query: token 1 (w=1.0) + token 2 (w=1.0)
        // chunk_a: 1*0.5 + 1*0.3 = 0.8
        // chunk_b: 1*0.7 + 0 = 0.7
        // chunk_c: 0 + 1*0.9 = 0.9
        let results = index.search(&vec![(1, 1.0), (2, 1.0)], 10);
        assert_eq!(results[0].id, "chunk_c"); // 0.9
        assert!((results[0].score - 0.9).abs() < 1e-5);
        assert_eq!(results[1].id, "chunk_a"); // 0.8
        assert!((results[1].score - 0.8).abs() < 1e-5);
        assert_eq!(results[2].id, "chunk_b"); // 0.7
        assert!((results[2].score - 0.7).abs() < 1e-5);
    }

    #[test]
    fn test_search_filter() {
        let index = make_test_index();
        // Filter: only chunk_a
        let results = index.search_with_filter(&vec![(1, 1.0)], 10, &|id: &str| id == "chunk_a");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "chunk_a");
    }

    #[test]
    fn test_search_no_match() {
        let index = make_test_index();
        // Query with token not in index
        let results = index.search(&vec![(999, 1.0)], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_empty_query() {
        let index = make_test_index();
        let results = index.search(&vec![], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_respects_k() {
        let index = make_test_index();
        let results = index.search(&vec![(1, 1.0), (2, 1.0), (3, 1.0)], 2);
        assert_eq!(results.len(), 2);
    }
}
