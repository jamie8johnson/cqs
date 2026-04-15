//! HNSW search implementation

use hnsw_rs::api::AnnT;
use hnsw_rs::filter::FilterT;
use hnsw_rs::prelude::DataId;

use crate::embedder::Embedding;
use crate::index::IndexResult;

use super::HnswIndex;

/// Wraps a `dyn Fn(&usize) -> bool` to satisfy hnsw_rs's `FilterT` trait.
struct PredicateFilter<'a>(&'a dyn Fn(&usize) -> bool);

impl FilterT for PredicateFilter<'_> {
    fn hnsw_filter(&self, id: &DataId) -> bool {
        (self.0)(id)
    }
}

impl HnswIndex {
    /// Search for nearest neighbors (unfiltered).
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        self.search_impl(query, k, None)
    }

    /// Search with traversal-time filtering.
    ///
    /// The predicate receives a chunk_id and returns true to keep the candidate.
    /// During HNSW graph traversal, non-matching nodes are skipped — the index
    /// returns exactly k matching results (or fewer if <k matches exist).
    pub fn search_filtered(
        &self,
        query: &Embedding,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        // Build a predicate over DataId (usize index into id_map)
        let id_filter = |id: &usize| -> bool {
            self.id_map
                .get(*id)
                .is_some_and(|chunk_id| filter(chunk_id))
        };
        self.search_impl(query, k, Some(&id_filter))
    }

    fn search_impl(
        &self,
        query: &Embedding,
        k: usize,
        filter: Option<&dyn Fn(&usize) -> bool>,
    ) -> Vec<IndexResult> {
        if self.id_map.is_empty() {
            return Vec::new();
        }

        let _span = tracing::debug_span!(
            "hnsw_search",
            k,
            index_size = self.id_map.len(),
            filtered = filter.is_some()
        )
        .entered();

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

        // TC-ADV-2: reject non-finite query vectors before they reach the
        // dense library. `hnsw_rs`/`anndists` asserts `dist_unchecked >= ε`
        // on the cosine distance result; a NaN query produces NaN and the
        // assert panics. We would rather return an empty result than crash
        // the search loop on a malformed query (encoder bug, query-cache
        // corruption, bit rot).
        if !query.as_slice().iter().all(|v| v.is_finite()) {
            tracing::warn!(
                "Query embedding contains non-finite values (NaN/Inf), \
                 returning empty results"
            );
            return Vec::new();
        }

        // Adaptive ef_search: baseline self.ef_search or 2*k (whichever is larger),
        // capped at index size (searching more than the index is pointless for small indexes).
        let index_size = self.id_map.len();
        let ef_search = self.ef_search.max(k * 2).min(index_size);

        let neighbors = match filter {
            Some(f) => {
                let wrapper = PredicateFilter(f);
                self.inner
                    .with_hnsw(|h| h.search_filter(query.as_slice(), k, ef_search, Some(&wrapper)))
            }
            None => self
                .inner
                .with_hnsw(|h| h.search_neighbours(query.as_slice(), k, ef_search)),
        };

        neighbors
            .into_iter()
            .filter_map(|n| {
                let idx = n.d_id;
                if idx < self.id_map.len() {
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
