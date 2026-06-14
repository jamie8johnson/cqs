//! Reciprocal Rank Fusion (RRF) — combine N ranked candidate lists into one.

use std::collections::HashMap;

use super::knob;
use super::BoundedScoreHeap;

/// Push a `[scoring]` section's knob overrides into the shared resolver.
/// Must be called before the first search; subsequent calls are no-ops
/// (delegates to [`knob::set_overrides_from_config`], which is OnceLock).
pub fn set_rrf_k_from_config(overrides: &crate::config::ScoringOverrides) {
    knob::set_overrides_from_config(&overrides.knobs);
}

/// RRF constant K. Resolved via the shared knob table — see
/// `src/search/scoring/knob.rs` for the resolution order
/// (config → `CQS_RRF_K` env → default 60.0).
fn rrf_k() -> f32 {
    knob::resolve_knob("rrf_k")
}

/// Compute RRF (Reciprocal Rank Fusion) scores for combining N ranked lists.
///
/// Any number of ranked sources contribute on a single uniform pipeline
/// (semantic embedding + FTS keyword + SPLADE sparse + name-fingerprint +
/// future signals all become slice elements). Each list is deduped
/// independently — duplicates within a list collapse to first-occurrence
/// rank, but a chunk that appears in multiple lists still accumulates one
/// RRF contribution per list, which is the desired "rewards overlap"
/// property.
///
/// Pre-allocates the HashMap with the total candidate budget across all
/// inputs. Uses `BoundedScoreHeap` for the final top-`limit` extraction
/// instead of full-sorting every candidate, with the id tie-breaker for
/// deterministic ordering.
pub(crate) fn rrf_fuse_n(ranked_lists: &[&[&str]], limit: usize) -> Vec<(String, f32)> {
    // K=60 is the standard RRF constant from the Cormack et al. (2009)
    // paper, tuned for web search. For code search with smaller corpora
    // (10k-100k chunks), the optimal K may differ; K=60 performs well on
    // our eval set. Override via CQS_RRF_K env var.
    let k = rrf_k();

    let total_capacity: usize = ranked_lists.iter().map(|l| l.len()).sum();
    let mut scores: HashMap<&str, f32> = HashMap::with_capacity(total_capacity);

    for list in ranked_lists {
        // Per-list dedup — duplicates within a list collapse to
        // first-occurrence rank (best score). Cross-list overlap is
        // intentional and gets the "rewards overlap" boost.
        let mut seen = std::collections::HashSet::with_capacity(list.len());
        for (rank, id) in list.iter().enumerate() {
            if !seen.insert(*id) {
                continue;
            }
            // RRF formula: 1 / (K + rank). The + 1.0 converts the
            // 0-indexed `enumerate()` to 1-indexed ranks (first result
            // = rank 1, not rank 0).
            let contribution = 1.0 / (k + rank as f32 + 1.0);
            *scores.entry(*id).or_insert(0.0) += contribution;
        }
    }

    let mut heap = BoundedScoreHeap::new(limit);
    for (id, score) in scores {
        heap.push(id.to_string(), score);
    }
    heap.into_sorted_vec()
}

/// 2-list wrapper for the semantic + FTS pairing. Prefer [`rrf_fuse_n`]
/// directly to plug additional signals without touching this signature.
pub(crate) fn rrf_fuse(
    semantic_ids: &[&str],
    fts_ids: &[String],
    limit: usize,
) -> Vec<(String, f32)> {
    let fts_refs: Vec<&str> = fts_ids.iter().map(|s| s.as_str()).collect();
    rrf_fuse_n(&[semantic_ids, &fts_refs], limit)
}

/// Exposed for property testing only
#[cfg(test)]
pub(crate) fn rrf_fuse_test(
    semantic_ids: &[String],
    fts_ids: &[String],
    limit: usize,
) -> Vec<(String, f32)> {
    let refs: Vec<&str> = semantic_ids.iter().map(|s| s.as_str()).collect();
    rrf_fuse(&refs, fts_ids, limit)
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
            let result = rrf_fuse_test(&semantic, &fts, limit);
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
            let result = rrf_fuse_test(&semantic, &fts, limit);
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
            let result = rrf_fuse_test(&semantic, &fts, limit);
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
            let result = rrf_fuse_test(&semantic, &fts, limit);
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

            let result = rrf_fuse_test(&semantic, &fts, 100);

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

    // ===== Direct rrf_fuse_n tests =====
    //
    // The 2-list rrf_fuse path is exhaustively covered by the proptests above
    // (it delegates to rrf_fuse_n). These tests exercise the generic surface
    // with shapes the wrapper doesn't reach: empty input list, a single list,
    // three lists, and "rewards overlap" across 3+ sources.

    #[test]
    fn rrf_fuse_n_empty_input_returns_empty() {
        let r = rrf_fuse_n(&[], 10);
        assert!(r.is_empty());
    }

    #[test]
    fn rrf_fuse_n_each_list_independently_dedupes() {
        // A list with the same id at rank 1 and rank 5 should contribute
        // exactly the rank-1 score, not their sum.
        let list: &[&str] = &["a", "b", "c", "d", "a", "e"];
        let r = rrf_fuse_n(&[list], 10);
        let a_score = r.iter().find(|(id, _)| id == "a").map(|(_, s)| *s).unwrap();
        // RRF formula: 1 / (K + 1) where K = rrf_k() (default 60). Should NOT
        // include the second occurrence's contribution.
        let k = rrf_k();
        let expected = 1.0 / (k + 1.0);
        assert!(
            (a_score - expected).abs() < 1e-6,
            "a should score {} (rank-1 only, dedup wins), got {}",
            expected,
            a_score
        );
    }

    #[test]
    fn rrf_fuse_n_three_lists_cumulative_overlap() {
        // A chunk that appears at rank 1 in all three lists should score
        // 3× the rank-1 contribution. Single-list participants score 1×.
        let l_sem: &[&str] = &["common", "x", "y"];
        let l_fts: &[&str] = &["common", "z"];
        let l_splade: &[&str] = &["common", "w"];

        let r = rrf_fuse_n(&[l_sem, l_fts, l_splade], 10);
        let k = rrf_k();
        let single = 1.0 / (k + 1.0);
        let triple = 3.0 * single;

        let common_score = r
            .iter()
            .find(|(id, _)| id == "common")
            .map(|(_, s)| *s)
            .unwrap();
        let x_score = r.iter().find(|(id, _)| id == "x").map(|(_, s)| *s).unwrap();

        assert!(
            (common_score - triple).abs() < 1e-6,
            "common at rank 1 in 3 lists should score {} (3× single), got {}",
            triple,
            common_score
        );
        assert!(
            (x_score - 1.0 / (k + 2.0)).abs() < 1e-6,
            "x at rank 2 in semantic-only should score {}, got {}",
            1.0 / (k + 2.0),
            x_score
        );
        // Common must outrank single-list participants — preserves the
        // "rewards overlap" property at N=3.
        assert!(common_score > x_score);
    }

    #[test]
    fn rrf_fuse_n_respects_limit_with_many_lists() {
        let l1: &[&str] = &["a", "b"];
        let l2: &[&str] = &["c", "d"];
        let l3: &[&str] = &["e", "f"];
        let l4: &[&str] = &["g", "h"];
        let r = rrf_fuse_n(&[l1, l2, l3, l4], 3);
        assert_eq!(r.len(), 3);
        // Sorted descending — first ≥ second ≥ third.
        assert!(r[0].1 >= r[1].1);
        assert!(r[1].1 >= r[2].1);
    }

    #[test]
    fn rrf_fuse_wrapper_produces_nonempty_fused_results() {
        // Adequacy regression (mutation-testing test-fire): the 2-list
        // `rrf_fuse` wrapper is the production hybrid path
        // (`search/query.rs` calls it for semantic + FTS fusion). Every
        // existing test exercising it does so through the proptests, which
        // assert only properties that an EMPTY vector satisfies vacuously
        // ("all scores positive", "len <= limit", "sorted descending",
        // "overlap wins" via `unwrap_or(0.0)`). Mutating the body to
        // `vec![]` therefore left the whole suite green — the wrapper looked
        // tested but nothing pinned that it returns the fused results at all.
        //
        // This test pins non-emptiness AND the exact fused scores, so a
        // `vec![]` (or any drop-the-results) mutation goes red.
        let semantic: &[&str] = &["common", "sem_only"];
        let fts: Vec<String> = vec!["common".to_string(), "fts_only".to_string()];

        let r = rrf_fuse(semantic, &fts, 10);

        // Non-emptiness: the load-bearing property the proptests can't see.
        assert!(
            !r.is_empty(),
            "rrf_fuse must return fused results for non-empty inputs, got empty"
        );

        let k = rrf_k();
        let score_of = |id: &str| r.iter().find(|(i, _)| i == id).map(|(_, s)| *s);

        // "common" is rank 1 in both lists → two rank-1 contributions.
        let common = score_of("common").expect("common must be present in fused output");
        let expected_common = 2.0 * (1.0 / (k + 1.0));
        assert!(
            (common - expected_common).abs() < 1e-6,
            "common (rank 1 in both lists) should score {expected_common}, got {common}"
        );

        // Single-list participants are present at their single rank-2 score.
        let sem_only = score_of("sem_only").expect("sem_only must be present");
        let fts_only = score_of("fts_only").expect("fts_only must be present");
        let expected_single = 1.0 / (k + 2.0);
        assert!((sem_only - expected_single).abs() < 1e-6);
        assert!((fts_only - expected_single).abs() < 1e-6);

        // Overlap actually wins (non-vacuously — both sides are real scores).
        assert!(
            common > sem_only,
            "overlapping id must outrank single-list ids: {common} vs {sem_only}"
        );
    }
}
