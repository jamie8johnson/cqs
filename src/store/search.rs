//! Search methods for the Store (FTS, name search, RRF fusion).

use std::collections::HashMap;
use std::sync::OnceLock;

use super::helpers::{self, ChunkRow, SearchResult};
use super::{sanitize_fts_query, ChunkSummary, Store, StoreError};
use crate::nl::normalize_for_fts;

/// Config-level override for RRF K, set by CLI before first search.
static RRF_K_CONFIG_OVERRIDE: OnceLock<f32> = OnceLock::new();

/// Set the RRF K override from a `ScoringOverrides` config.
/// Must be called before the first search; subsequent calls are no-ops (OnceLock).
pub fn set_rrf_k_from_config(overrides: &crate::config::ScoringOverrides) {
    if let Some(k) = overrides.rrf_k {
        let _ = RRF_K_CONFIG_OVERRIDE.set(k);
    }
}

/// PF-2: RRF constant K, cached on first access. Defaults to 60.0.
/// Priority: config override > `CQS_RRF_K` env var > default 60.0.
fn rrf_k() -> f32 {
    static K: OnceLock<f32> = OnceLock::new();
    *K.get_or_init(|| {
        // EXT-5: Config override takes precedence over env var
        if let Some(&k) = RRF_K_CONFIG_OVERRIDE.get() {
            return k;
        }
        std::env::var("CQS_RRF_K")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60.0)
    })
}

impl Store {
    /// Search FTS5 index for keyword matches.
    ///
    /// # Search Method Overview
    ///
    /// The Store provides several search methods with different characteristics:
    ///
    /// - **`search_fts`**: Full-text keyword search using SQLite FTS5. Returns chunk IDs.
    ///   Best for: Exact keyword matches, symbol lookup by name fragment.
    ///
    /// - **`search_by_name`**: Definition search by function/struct name. Uses FTS5 with
    ///   heavy weighting on the name column. Returns full `SearchResult` with scores.
    ///   Best for: "Where is X defined?" queries.
    ///
    /// - **`search_filtered`** (in `search/query.rs`): Semantic search with optional
    ///   language/path filters. Can use RRF hybrid search combining semantic + FTS scores.
    ///   Best for: Natural language queries like "retry with exponential backoff".
    ///
    /// - **`search_filtered_with_index`** (in `search/query.rs`): Like `search_filtered`
    ///   but uses HNSW/CAGRA vector index for O(log n) candidate retrieval instead of
    ///   brute force. Best for: Large indexes (>5k chunks) where brute force is slow.
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let _span = tracing::info_span!("search_fts", limit).entered();
        let normalized_query = sanitize_fts_query(&normalize_for_fts(query));
        if normalized_query.is_empty() {
            tracing::debug!(
                original_query = %query,
                "Query normalized to empty string, returning no FTS results"
            );
            return Ok(vec![]);
        }

        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
            )
            .bind(&normalized_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.into_iter().map(|(id,)| id).collect())
        })
    }

    /// Search for chunks by name (definition search).
    ///
    /// Searches the FTS5 name column for exact or prefix matches.
    /// Use this for "where is X defined?" queries instead of semantic search.
    pub fn search_by_name(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!("search_by_name", %name, limit).entered();
        let limit = limit.min(100);
        let normalized = sanitize_fts_query(&normalize_for_fts(name));
        if normalized.is_empty() {
            return Ok(vec![]);
        }

        // Pre-lowercase query once for score_name_match_pre_lower (PF-3)
        let lower_name = name.to_lowercase();

        // Search name column specifically using FTS5 column filter
        // Use * for prefix matching (e.g., "parse" matches "parse_config")
        // SEC-10: Runtime guard — sanitize_fts_query strips `"` but defense-in-depth
        // prevents FTS5 injection if sanitization logic ever changes.
        if normalized.contains('"') {
            tracing::warn!(
                name = %name,
                "FTS injection guard: double quote in sanitized name, returning empty"
            );
            return Ok(vec![]);
        }
        let fts_query = format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized);

        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id, c.parent_type_name
                 FROM chunks c
                 JOIN chunks_fts f ON c.id = f.id
                 WHERE chunks_fts MATCH ?1
                 ORDER BY bm25(chunks_fts, 10.0, 1.0, 1.0, 1.0) -- Heavy weight on name column
                 LIMIT ?2",
            )
            .bind(&fts_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            let mut results = rows
                .into_iter()
                .map(|row| {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(&row));
                    let name_lower = chunk.name.to_lowercase();
                    let score = helpers::score_name_match_pre_lower(&name_lower, &lower_name);
                    SearchResult { chunk, score }
                })
                .collect::<Vec<_>>();

            // Re-sort by name-match score (FTS bm25 ordering may differ).
            // Secondary sort on chunk id ensures equal-score candidates have a
            // deterministic order across process invocations.
            results.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.chunk.id.cmp(&b.chunk.id))
            });

            Ok(results)
        })
    }

    /// Compute RRF (Reciprocal Rank Fusion) scores for combining two ranked lists.
    ///
    /// Pre-allocates the HashMap with capacity for both input lists (PERF-28).
    /// Input size varies (limit*3 semantic + limit*3 FTS) but is always known upfront.
    ///
    /// PF-V1.25-2: uses `BoundedScoreHeap` for the final top-`limit` extraction
    /// instead of full-sorting every candidate. Asymptotic: O(n log n) → O(n log limit),
    /// which saves meaningful work on large candidate pools (100k returned for top-100).
    pub(crate) fn rrf_fuse(
        semantic_ids: &[&str],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        // K=60 is the standard RRF constant from the Cormack et al. (2009) paper,
        // originally tuned for web search. For code search with smaller corpora
        // (10k-100k chunks), the optimal K may differ. Empirically, K=60 performs
        // well on our eval set (90.9% R@1). Override via CQS_RRF_K env var.
        let k = rrf_k();

        let mut scores: HashMap<&str, f32> =
            HashMap::with_capacity(semantic_ids.len() + fts_ids.len());

        // Deduplicate semantic_ids — keep first occurrence (best rank) only.
        // Duplicates would get RRF contributions at multiple ranks, inflating score.
        let mut seen_semantic = std::collections::HashSet::with_capacity(semantic_ids.len());
        for (rank, id) in semantic_ids.iter().enumerate() {
            if !seen_semantic.insert(*id) {
                continue; // skip duplicate
            }
            // RRF formula: 1 / (K + rank). The + 1.0 converts 0-indexed enumerate()
            // to 1-indexed ranks (first result = rank 1, not rank 0).
            let contribution = 1.0 / (k + rank as f32 + 1.0);
            *scores.entry(id).or_insert(0.0) += contribution;
        }

        // AC-9: Deduplicate fts_ids — symmetric with semantic_ids dedup above.
        let mut seen_fts = std::collections::HashSet::with_capacity(fts_ids.len());
        for (rank, id) in fts_ids.iter().enumerate() {
            if !seen_fts.insert(id.as_str()) {
                continue; // skip duplicate
            }
            // Same conversion: enumerate's 0-index -> RRF's 1-indexed rank
            let contribution = 1.0 / (k + rank as f32 + 1.0);
            *scores.entry(id.as_str()).or_insert(0.0) += contribution;
        }

        // Bounded heap keeps top-`limit` in O(n log limit) instead of the full
        // O(n log n) sort+truncate. `BoundedScoreHeap::into_sorted_vec` applies
        // the id tie-breaker so results are deterministic across process
        // invocations (the HashMap above iterates in random order).
        let mut heap = crate::search::scoring::BoundedScoreHeap::new(limit);
        for (id, score) in scores {
            heap.push(id.to_string(), score);
        }
        heap.into_sorted_vec()
    }

    /// Exposed for property testing only
    #[cfg(test)]
    pub(crate) fn rrf_fuse_test(
        semantic_ids: &[String],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        let refs: Vec<&str> = semantic_ids.iter().map(|s| s.as_str()).collect();
        Self::rrf_fuse(&refs, fts_ids, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ===== Property-based tests for RRF =====

    proptest! {
        /// Property: RRF scores are always positive
        #[test]
        fn prop_rrf_scores_positive(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..20),
            fts in prop::collection::vec("[a-z]{1,5}", 0..20),
            limit in 1usize..50
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            for (_, score) in &result {
                prop_assert!(*score > 0.0, "RRF score should be positive: {}", score);
            }
        }

        /// Property: RRF scores are bounded
        /// Note: Duplicates in input lists can accumulate extra points.
        /// Max theoretical: sum of 1/(K+r+1) for all appearances across both lists.
        #[test]
        fn prop_rrf_scores_bounded(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..20),
            fts in prop::collection::vec("[a-z]{1,5}", 0..20),
            limit in 1usize..50
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            // Conservative upper bound: sum of first N terms of 1/(K+r+1) for both lists
            // where N is max list length (20). With duplicates, actual max is ~0.3
            let max_possible = 0.5; // generous bound accounting for duplicates
            for (id, score) in &result {
                prop_assert!(
                    *score <= max_possible,
                    "RRF score {} for '{}' exceeds max {}",
                    score, id, max_possible
                );
            }
        }

        /// Property: RRF respects limit
        #[test]
        fn prop_rrf_respects_limit(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..30),
            fts in prop::collection::vec("[a-z]{1,5}", 0..30),
            limit in 1usize..20
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            prop_assert!(
                result.len() <= limit,
                "Result length {} exceeds limit {}",
                result.len(), limit
            );
        }

        /// Property: RRF results are sorted by score descending
        #[test]
        fn prop_rrf_sorted_descending(
            semantic in prop::collection::vec("[a-z]{1,5}", 1..20),
            fts in prop::collection::vec("[a-z]{1,5}", 1..20),
            limit in 1usize..50
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            for window in result.windows(2) {
                prop_assert!(
                    window[0].1 >= window[1].1,
                    "Results not sorted: {} < {}",
                    window[0].1, window[1].1
                );
            }
        }

        /// Property: Items appearing in both lists get higher scores
        /// Note: Uses hash_set to ensure unique IDs - duplicates in input lists
        /// accumulate scores which can violate the "overlap wins" property.
        #[test]
        fn prop_rrf_rewards_overlap(
            common_id in "[a-z]{3}",
            only_semantic in prop::collection::hash_set("[A-Z]{3}", 1..5),
            only_fts in prop::collection::hash_set("[0-9]{3}", 1..5)
        ) {
            let mut semantic = vec![common_id.clone()];
            semantic.extend(only_semantic);
            let mut fts = vec![common_id.clone()];
            fts.extend(only_fts);

            let result = Store::rrf_fuse_test(&semantic, &fts, 100);

            let common_score = result.iter()
                .find(|(id, _)| id == &common_id)
                .map(|(_, s)| *s)
                .unwrap_or(0.0);

            let max_single = result.iter()
                .filter(|(id, _)| id != &common_id)
                .map(|(_, s)| *s)
                .fold(0.0f32, |a, b| a.max(b));

            prop_assert!(
                common_score >= max_single,
                "Common item score {} should be >= single-list max {}",
                common_score, max_single
            );
        }
    }
}
