//! Candidate scoring, importance demotion, parent boost, and bounded heap.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::language::ChunkType;
use crate::math::cosine_similarity;
use crate::store::helpers::{SearchFilter, SearchResult};

use super::config::ScoringConfig;
use super::name_match::NameMatcher;
use super::note_boost::NoteBoostIndex;

/// Compute search-time importance multiplier for a chunk.
///
/// Demotes test functions (via [`is_test_chunk`](crate::is_test_chunk)) and
/// underscore-prefixed private helpers.
/// Applied as a multiplier like `note_boost`, so it composes: `score * note_boost * importance`.
///
/// | Signal                   | Detection                           | Multiplier |
/// |--------------------------|-------------------------------------|------------|
/// | Test chunk               | `crate::is_test_chunk(name, path)`  | 0.70       |
/// | Underscore-prefixed      | name starts with `_` (not `__`)     | 0.80       |
///
/// Returns 1.0 (no change) when demotion doesn't apply.
pub(crate) fn chunk_importance(name: &str, file_path: &str) -> f32 {
    let cfg = &ScoringConfig::DEFAULT;
    if crate::is_test_chunk(name, file_path) {
        return cfg.importance_test;
    }
    // Underscore-prefixed private (but not dunder like __init__)
    if name.starts_with('_') && !name.starts_with("__") {
        return cfg.importance_private;
    }
    1.0
}

/// Boost container chunks (Class, Struct, Interface) when multiple child methods
/// from the same parent appear in search results.
///
/// When a query semantically matches several methods of one class, the class
/// itself is usually the best answer — the methods individually match fragments
/// of the query, but the class embodies the whole concept (e.g., "circuit breaker
/// pattern" → `CircuitBreaker` class, not `recordFailure` method).
///
/// Algorithm: count how many results have `parent_type_name == X`. If a
/// Class/Struct/Interface chunk named `X` also appears in results, boost it.
///
/// Boost magnitude: `1.0 + parent_boost_per_child × (child_count - 1)`, capped at `parent_boost_cap`.
/// With 2 children → 1.05×, 3 → 1.10×, 4+ → 1.15×.
///
/// Re-sorts results by score after boosting.
pub(crate) fn apply_parent_boost(results: &mut [SearchResult]) {
    if results.len() < 3 {
        return; // Need at least a container + 2 children
    }

    // Count how many results share each parent_type_name
    let mut parent_counts: HashMap<String, usize> = HashMap::new();
    for r in results.iter() {
        if let Some(ref ptn) = r.chunk.parent_type_name {
            *parent_counts.entry(ptn.clone()).or_insert(0) += 1;
        }
    }

    // Only proceed if any parent_type_name appears 2+ times
    if !parent_counts.values().any(|&c| c >= 2) {
        return;
    }

    let cfg = &ScoringConfig::DEFAULT;
    let max_children = (cfg.parent_boost_cap - 1.0) / cfg.parent_boost_per_child;
    let mut boosted = false;
    for r in results.iter_mut() {
        let is_container = matches!(
            r.chunk.chunk_type,
            ChunkType::Class | ChunkType::Struct | ChunkType::Interface
        );
        if !is_container {
            continue;
        }
        if let Some(&count) = parent_counts.get(&r.chunk.name) {
            if count >= 2 {
                let boost =
                    1.0 + cfg.parent_boost_per_child * (count as f32 - 1.0).min(max_children);
                tracing::debug!(
                    name = %r.chunk.name,
                    child_count = count,
                    boost = %boost,
                    "parent_boost: boosting container"
                );
                r.score *= boost;
                boosted = true;
            }
        }
    }

    if boosted {
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
}

/// Bounded min-heap for maintaining top-N search results by score.
///
/// Uses a min-heap internally so the smallest score is always at the top,
/// allowing O(log N) eviction when the heap is full. This bounds memory to
/// O(limit) instead of O(total_chunks) for the scoring phase.
pub(crate) struct BoundedScoreHeap {
    heap: BinaryHeap<Reverse<(OrderedFloat, String)>>,
    capacity: usize,
}

/// Wrapper for f32 that implements Ord for use in BinaryHeap.
/// Uses total_cmp for consistent ordering (NaN sorts to the end).
#[derive(Clone, Copy, PartialEq)]
struct OrderedFloat(f32);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    /// Compares two values and returns an ordering, wrapped in `Option`.
    ///
    /// # Arguments
    ///
    /// * `other` - The value to compare against
    ///
    /// # Returns
    ///
    /// Returns `Some(Ordering)` indicating whether `self` is less than, equal to, or greater than `other`.
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    /// Compares two values using total ordering on their inner floating-point values.
    ///
    /// # Arguments
    ///
    /// * `other` - The value to compare against
    ///
    /// # Returns
    ///
    /// An `Ordering` indicating whether `self` is less than, equal to, or greater than `other`. Uses total ordering semantics where NaN values are comparable.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl BoundedScoreHeap {
    /// Creates a new bounded priority queue with the specified capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - The maximum number of elements the queue can hold
    ///
    /// # Returns
    ///
    /// A new `BoundedPriorityQueue` instance with the given capacity. The internal heap is pre-allocated with space for `capacity + 1` elements.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity + 1),
            capacity,
        }
    }

    /// Push a scored result. If at capacity, evicts the lowest score.
    pub fn push(&mut self, id: String, score: f32) {
        if !score.is_finite() {
            tracing::warn!("BoundedScoreHeap: ignoring non-finite score");
            return;
        }

        // If below capacity, always insert
        if self.heap.len() < self.capacity {
            self.heap.push(Reverse((OrderedFloat(score), id)));
            return;
        }

        // At capacity - only insert if strictly better than current minimum.
        // Using > (not >=) gives first-indexed stability: when scores are equal,
        // earlier items are kept. This prevents last-wins bias where later-indexed
        // chunks systematically replace earlier ones at equal scores.
        if let Some(Reverse((OrderedFloat(min_score), _))) = self.heap.peek() {
            if score > *min_score {
                self.heap.pop();
                self.heap.push(Reverse((OrderedFloat(score), id)));
            }
        }
    }

    /// Drain into a sorted Vec (highest score first).
    pub fn into_sorted_vec(self) -> Vec<(String, f32)> {
        let mut results: Vec<_> = self
            .heap
            .into_iter()
            .map(|Reverse((OrderedFloat(score), id))| (id, score))
            .collect();
        results.sort_by(|a, b| b.1.total_cmp(&a.1));
        results
    }
}

/// Score a single candidate chunk against the query.
///
/// Pure function — no database access. Combines embedding similarity, optional
/// name boosting, glob filtering, note boosting, and test-function demotion.
///
/// Returns `None` if the candidate is filtered out (glob mismatch or below threshold).
#[allow(clippy::too_many_arguments)]
pub(crate) fn score_candidate(
    embedding: &[f32],
    query: &[f32],
    name: Option<&str>,
    file_part: &str,
    filter: &SearchFilter,
    name_matcher: Option<&NameMatcher>,
    glob_matcher: Option<&globset::GlobMatcher>,
    note_index: &NoteBoostIndex<'_>,
    threshold: f32,
) -> Option<f32> {
    let embedding_score = cosine_similarity(query, embedding)?;

    let base_score = if let Some(matcher) = name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
    } else {
        embedding_score
    };

    if let Some(matcher) = glob_matcher {
        if !matcher.is_match(file_part) {
            return None;
        }
    }

    // Apply note-based boost: notes mentioning this chunk's file or name
    // adjust its score by up to ±15%
    let chunk_name = name.unwrap_or("");
    let mut score = base_score * note_index.boost(file_part, chunk_name);

    // Apply demotion for test functions and underscore-prefixed names
    if filter.enable_demotion {
        score *= chunk_importance(chunk_name, file_part);
    }

    if score >= threshold {
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::helpers::{ChunkSummary, NoteSummary, SearchFilter};

    // ===== BoundedScoreHeap tests =====

    #[test]
    fn test_bounded_heap_equal_scores() {
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("a".to_string(), 0.5);
        heap.push("b".to_string(), 0.5);
        heap.push("c".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 2);
        // First-indexed stability: equal scores don't replace existing entries,
        // so "a" and "b" are kept, "c" is rejected.
        assert!(results.iter().any(|(id, _)| id == "a"));
        assert!(results.iter().any(|(id, _)| id == "b"));
    }

    #[test]
    fn test_bounded_heap_evicts_lowest() {
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("low".to_string(), 0.1);
        heap.push("mid".to_string(), 0.5);
        heap.push("high".to_string(), 0.9);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "high");
        assert_eq!(results[1].0, "mid");
    }

    #[test]
    fn test_bounded_heap_ignores_non_finite() {
        let mut heap = BoundedScoreHeap::new(5);
        heap.push("nan".to_string(), f32::NAN);
        heap.push("inf".to_string(), f32::INFINITY);
        heap.push("neginf".to_string(), f32::NEG_INFINITY);
        heap.push("ok".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "ok");
    }

    #[test]
    fn test_bounded_heap_empty() {
        let heap = BoundedScoreHeap::new(5);
        let results = heap.into_sorted_vec();
        assert!(results.is_empty());
    }

    // ===== parent_boost tests =====

    /// Constructs a test SearchResult with minimal required fields populated.
    fn make_result(
        name: &str,
        chunk_type: ChunkType,
        parent_type_name: Option<&str>,
        score: f32,
    ) -> SearchResult {
        SearchResult {
            chunk: ChunkSummary {
                id: name.to_string(),
                file: std::path::PathBuf::from("test.ts"),
                language: crate::parser::Language::TypeScript,
                chunk_type,
                name: name.to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 1,
                line_end: 10,
                parent_id: None,
                parent_type_name: parent_type_name.map(|s| s.to_string()),
                content_hash: String::new(),
                window_idx: None,
            },
            score,
        }
    }

    #[test]
    fn test_parent_boost_circuit_breaker() {
        // CircuitBreaker class at rank 4, its methods rank 1-3
        let mut results = vec![
            make_result(
                "recordFailure",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.88,
            ),
            make_result(
                "retryWithBackoff",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.86,
            ),
            make_result(
                "shouldAllow",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.85,
            ),
            make_result("CircuitBreaker", ChunkType::Class, None, 0.82),
        ];
        apply_parent_boost(&mut results);
        // 3 children → boost = 1.10, 0.82 * 1.10 = 0.902 > 0.88
        assert_eq!(results[0].chunk.name, "CircuitBreaker");
        assert!(results[0].score > 0.90);
    }

    #[test]
    fn test_parent_boost_no_effect_on_standalone_functions() {
        // Sort variants — standalone functions, no parent_type_name
        let mut results = vec![
            make_result("_insertionSortSmall", ChunkType::Function, None, 0.88),
            make_result("insertionSort", ChunkType::Function, None, 0.85),
            make_result("mergeSort", ChunkType::Function, None, 0.80),
        ];
        let scores_before: Vec<f32> = results.iter().map(|r| r.score).collect();
        apply_parent_boost(&mut results);
        let scores_after: Vec<f32> = results.iter().map(|r| r.score).collect();
        assert_eq!(scores_before, scores_after);
    }

    #[test]
    fn test_parent_boost_needs_minimum_two_children() {
        // Only 1 method from the class — no boost
        let mut results = vec![
            make_result(
                "recordFailure",
                ChunkType::Method,
                Some("CircuitBreaker"),
                0.88,
            ),
            make_result("CircuitBreaker", ChunkType::Class, None, 0.82),
            make_result("unrelatedFn", ChunkType::Function, None, 0.80),
        ];
        apply_parent_boost(&mut results);
        // CircuitBreaker should stay at rank 2
        assert_eq!(results[0].chunk.name, "recordFailure");
        assert_eq!(results[1].chunk.name, "CircuitBreaker");
    }

    #[test]
    fn test_parent_boost_caps_at_1_15() {
        // 5 children → should cap at 1.15, not 1.20
        let mut results = vec![
            make_result("m1", ChunkType::Method, Some("BigClass"), 0.88),
            make_result("m2", ChunkType::Method, Some("BigClass"), 0.87),
            make_result("m3", ChunkType::Method, Some("BigClass"), 0.86),
            make_result("m4", ChunkType::Method, Some("BigClass"), 0.85),
            make_result("m5", ChunkType::Method, Some("BigClass"), 0.84),
            make_result("BigClass", ChunkType::Class, None, 0.78),
        ];
        apply_parent_boost(&mut results);
        // max boost = 1.15, 0.78 * 1.15 = 0.897
        let class_score = results
            .iter()
            .find(|r| r.chunk.name == "BigClass")
            .unwrap()
            .score;
        assert!(
            (class_score - 0.897).abs() < 0.001,
            "Expected ~0.897, got {class_score}"
        );
    }

    #[test]
    fn test_parent_boost_too_few_results() {
        // Only 2 results — function returns early
        let mut results = vec![
            make_result("foo", ChunkType::Method, Some("Bar"), 0.88),
            make_result("Bar", ChunkType::Class, None, 0.82),
        ];
        let score_before = results[1].score;
        apply_parent_boost(&mut results);
        assert_eq!(results[1].score, score_before);
    }

    // ===== chunk_importance tests =====

    #[test]
    fn test_chunk_importance_normal() {
        assert_eq!(chunk_importance("parse_config", "src/lib.rs"), 1.0);
    }

    #[test]
    fn test_chunk_importance_test_prefix() {
        assert_eq!(chunk_importance("test_parse_config", "src/lib.rs"), 0.70);
    }

    #[test]
    fn test_chunk_importance_test_upper() {
        // Go convention: TestFoo
        assert_eq!(
            chunk_importance("TestParseConfig", "src/lib.go"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    #[test]
    fn test_chunk_importance_underscore() {
        assert_eq!(
            chunk_importance("_helper", "src/lib.rs"),
            ScoringConfig::DEFAULT.importance_private
        );
    }

    #[test]
    fn test_chunk_importance_dunder_not_demoted() {
        // Python dunders like __init__ should NOT be demoted
        assert_eq!(chunk_importance("__init__", "src/lib.py"), 1.0);
    }

    #[test]
    fn test_chunk_importance_test_file() {
        // File named foo_test.rs → demotion via filename
        assert_eq!(
            chunk_importance("helper_fn", "src/foo_test.rs"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    #[test]
    fn test_chunk_importance_test_dir_demoted() {
        // Files in tests/ directory are test infrastructure → demoted
        assert_eq!(
            chunk_importance("real_fn", "tests/fixtures/eval.rs"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    #[test]
    fn test_chunk_importance_test_name_beats_path() {
        // test_ name triggers demotion even in normal directory
        assert_eq!(
            chunk_importance("test_foo", "src/lib.rs"),
            ScoringConfig::DEFAULT.importance_test
        );
    }

    // ===== score_candidate tests =====

    /// Build a normalized 768-dim test vector for score_candidate tests.
    fn test_embedding(seed: f32) -> Vec<f32> {
        let mut v = vec![seed; 768];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    fn make_note(sentiment: f32, mentions: &[&str]) -> NoteSummary {
        NoteSummary {
            id: "note:test".to_string(),
            text: "test note".to_string(),
            sentiment,
            mentions: mentions.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn test_score_candidate_basic() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoostIndex::new(&[]);

        let score = score_candidate(
            &emb,
            &query,
            None,
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index,
            0.0,
        );
        assert!(score.is_some());
        assert!(
            score.unwrap() > 0.9,
            "Self-similarity should be ~1.0, got {}",
            score.unwrap()
        );
    }

    #[test]
    fn test_score_candidate_below_threshold() {
        // Near-orthogonal vectors → low cosine similarity
        let emb = test_embedding(1.0);
        let query = test_embedding(-1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoostIndex::new(&[]);

        let score = score_candidate(
            &emb,
            &query,
            None,
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index,
            0.5,
        );
        assert!(
            score.is_none(),
            "Opposite vectors should be below 0.5 threshold"
        );
    }

    #[test]
    fn test_score_candidate_glob_filters() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoostIndex::new(&[]);
        let glob = globset::Glob::new("src/**/*.rs").unwrap().compile_matcher();

        // Matching path
        let score = score_candidate(
            &emb,
            &query,
            None,
            "src/lib.rs",
            &filter,
            None,
            Some(&glob),
            &note_index,
            0.0,
        );
        assert!(score.is_some());

        // Non-matching path
        let score = score_candidate(
            &emb,
            &query,
            None,
            "tests/foo.py",
            &filter,
            None,
            Some(&glob),
            &note_index,
            0.0,
        );
        assert!(score.is_none());
    }

    #[test]
    fn test_score_candidate_name_boost() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter_no_boost = SearchFilter::default();
        let filter_with_boost = SearchFilter {
            name_boost: 0.3,
            query_text: "parseConfig".to_string(),
            ..Default::default()
        };
        let note_index = NoteBoostIndex::new(&[]);
        let matcher = NameMatcher::new("parseConfig");

        let score_no = score_candidate(
            &emb,
            &query,
            Some("parseConfig"),
            "src/a.rs",
            &filter_no_boost,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();
        let score_yes = score_candidate(
            &emb,
            &query,
            Some("parseConfig"),
            "src/a.rs",
            &filter_with_boost,
            Some(&matcher),
            None,
            &note_index,
            0.0,
        )
        .unwrap();

        assert!(score_yes > 0.0);
        assert!(score_no > 0.0);
    }

    #[test]
    fn test_score_candidate_demotion() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let note_index = NoteBoostIndex::new(&[]);

        let filter_no_demote = SearchFilter {
            enable_demotion: false,
            ..Default::default()
        };
        let filter_demote = SearchFilter {
            enable_demotion: true,
            ..Default::default()
        };

        let score_normal = score_candidate(
            &emb,
            &query,
            Some("real_fn"),
            "src/lib.rs",
            &filter_demote,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();
        let score_test = score_candidate(
            &emb,
            &query,
            Some("test_foo"),
            "src/lib.rs",
            &filter_demote,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();
        let score_no_demote = score_candidate(
            &emb,
            &query,
            Some("test_foo"),
            "src/lib.rs",
            &filter_no_demote,
            None,
            None,
            &note_index,
            0.0,
        )
        .unwrap();

        // With demotion, test_ function should score lower than normal
        assert!(score_test < score_normal, "test_ should be demoted");
        // Without demotion flag, test_ function scores the same as normal
        assert!(
            (score_no_demote - score_normal).abs() < 0.001,
            "No demotion without flag"
        );
    }

    #[test]
    fn test_score_candidate_note_boost() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let filter = SearchFilter::default();

        let notes = vec![make_note(1.0, &["lib.rs"])];
        let note_index_boosted = NoteBoostIndex::new(&notes);
        let note_index_empty = NoteBoostIndex::new(&[]);

        let score_boosted = score_candidate(
            &emb,
            &query,
            Some("my_fn"),
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index_boosted,
            0.0,
        )
        .unwrap();
        let score_plain = score_candidate(
            &emb,
            &query,
            Some("my_fn"),
            "src/lib.rs",
            &filter,
            None,
            None,
            &note_index_empty,
            0.0,
        )
        .unwrap();

        assert!(
            score_boosted > score_plain,
            "Positive note should boost score"
        );
    }
}
