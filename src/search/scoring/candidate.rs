//! Candidate scoring, importance demotion, parent boost, and bounded heap.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::language::ChunkType;
use crate::math::cosine_similarity;
use crate::store::helpers::{SearchFilter, SearchResult};

use super::config::ScoringConfig;
use super::name_match::NameMatcher;
#[cfg(test)]
use super::note_boost::{NoteBoost, NoteBoostIndex};

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

    // PF-V1.25-14: compute which result indices need boosting in an
    // immutable-borrow phase so `parent_counts` can key on `&str` borrowed
    // from `results` instead of cloning every `parent_type_name`. Once we
    // have the `(index, boost)` list, `parent_counts` drops and the mutable
    // pass runs without overlapping borrows.
    let boosts: Vec<(usize, f32)> = {
        let mut parent_counts: HashMap<&str, usize> = HashMap::new();
        for r in results.iter() {
            if let Some(ref ptn) = r.chunk.parent_type_name {
                *parent_counts.entry(ptn.as_str()).or_insert(0) += 1;
            }
        }
        // Only proceed if any parent_type_name appears 2+ times
        if !parent_counts.values().any(|&c| c >= 2) {
            return;
        }
        let cfg = &ScoringConfig::DEFAULT;
        let max_children = (cfg.parent_boost_cap - 1.0) / cfg.parent_boost_per_child;
        results
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let is_container = matches!(
                    r.chunk.chunk_type,
                    ChunkType::Class | ChunkType::Struct | ChunkType::Interface
                );
                if !is_container {
                    return None;
                }
                let count = *parent_counts.get(r.chunk.name.as_str())?;
                if count >= 2 {
                    let boost =
                        1.0 + cfg.parent_boost_per_child * (count as f32 - 1.0).min(max_children);
                    tracing::debug!(
                        name = %r.chunk.name,
                        child_count = count,
                        boost = %boost,
                        "parent_boost: boosting container"
                    );
                    Some((i, boost))
                } else {
                    None
                }
            })
            .collect()
    };

    if boosts.is_empty() {
        return;
    }

    for (i, boost) in &boosts {
        results[*i].score *= *boost;
    }
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk.id.cmp(&b.chunk.id))
    });
}

/// Bounded min-heap for maintaining top-N search results by score.
///
/// Uses a min-heap internally so the smallest score is always at the top,
/// allowing O(log N) eviction when the heap is full. This bounds memory to
/// O(limit) instead of O(total_chunks) for the scoring phase.
///
/// The heap key wraps the id in `Reverse` so that `peek()` returns the
/// element that ranks worst under the final sort order (score desc, id asc):
/// the smallest score, and among ties the *largest* id. That is the correct
/// eviction candidate — replacing the largest-id-at-lowest-score entry with
/// a smaller-id entry preserves the "smaller id wins among ties" invariant
/// that callers rely on for determinism (e.g., `rrf_fuse` feeding from a
/// process-seed-randomized HashMap).
pub(crate) struct BoundedScoreHeap {
    heap: BinaryHeap<Reverse<(OrderedFloat, Reverse<String>)>>,
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
    /// Note: `capacity == 0` is valid but silently discards all pushes.
    /// Callers should check for zero before constructing if this is unexpected.
    pub fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity + 1),
            capacity,
        }
    }

    /// Push a scored result. If at capacity, evicts the worst entry.
    ///
    /// Tie-breaking at the eviction boundary is deterministic on id
    /// (ascending) — when scores are equal, the smaller id wins. This keeps
    /// the final top-K set independent of insertion order, which matters for
    /// callers that feed results from a HashMap or other randomly-ordered
    /// source (e.g. `rrf_fuse`).
    ///
    /// Implementation: the heap key `Reverse<(score, Reverse<id>)>` causes
    /// `peek()` to return the worst element under the final sort order
    /// (smallest score, largest id at that score). The eviction check
    /// `score > worst_score || (score == worst_score && id < worst_largest_id)`
    /// then correctly replaces the largest-id-at-lowest-score entry with a
    /// smaller-id incoming entry. Earlier versions wrapped only the score in
    /// `Reverse`, so `peek()` returned the *best* boundary entry (smallest
    /// id at lowest score) and the eviction inverted the intended invariant
    /// — under reverse push order ("c", "b", "a") the surviving set was
    /// {"c", "a"} instead of {"a", "b"}.
    pub fn push(&mut self, id: String, score: f32) {
        if !score.is_finite() {
            tracing::warn!(id = %id, score = ?score, "BoundedScoreHeap: ignoring non-finite score");
            return;
        }

        // If below capacity, always insert
        if self.heap.len() < self.capacity {
            self.heap.push(Reverse((OrderedFloat(score), Reverse(id))));
            return;
        }

        // At capacity - evict current worst if the incoming pair is better
        // under the final sort order: score desc, id asc. The peeked element
        // is `(min_score, max_id_at_min_score)`. Strict ">" on score keeps
        // first-seen on pure score ties for non-boundary cases; at the
        // eviction boundary we break score ties on id ascending so the
        // surviving set is deterministic.
        if let Some(Reverse((OrderedFloat(worst_score), Reverse(worst_id)))) = self.heap.peek() {
            let better = score > *worst_score || (score == *worst_score && id < *worst_id);
            if better {
                self.heap.pop();
                self.heap.push(Reverse((OrderedFloat(score), Reverse(id))));
            }
        }
    }

    /// Drain into a sorted Vec (highest score first).
    ///
    /// Secondary sort on id (ascending) ensures equal-score candidates
    /// have a deterministic order across process invocations — the
    /// internal `BinaryHeap` iterates in arbitrary order, so we can't
    /// rely on push-order stability here.
    pub fn into_sorted_vec(self) -> Vec<(String, f32)> {
        let mut results: Vec<_> = self
            .heap
            .into_iter()
            .map(|Reverse((OrderedFloat(score), Reverse(id)))| (id, score))
            .collect();
        results.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        results
    }
}

/// Loop-invariant scoring context.
///
/// Groups the arguments to `score_candidate` that don't change between iterations
/// in the scoring loop (query vector, filter, matchers, note index, threshold).
pub(crate) struct ScoringContext<'a> {
    pub query: &'a [f32],
    pub filter: &'a SearchFilter,
    pub name_matcher: Option<&'a NameMatcher>,
    pub glob_matcher: Option<&'a globset::GlobMatcher>,
    /// PF-V1.25-4: accepts either a freshly-built `NoteBoostIndex` borrowing
    /// from per-call notes, or a cached `Arc<OwnedNoteBoostIndex>` reused
    /// across searches. The `NoteBoost` enum dispatches at the call site.
    pub note_index: &'a super::note_boost::NoteBoost<'a>,
    pub threshold: f32,
}

/// Apply the scoring pipeline to a pre-computed base score (name boost, glob
/// filter, note boost, test demotion, threshold).
///
/// Used by `score_candidate` (base = cosine) and the hybrid search path
/// (base = alpha-weighted dense+sparse fusion).
pub(crate) fn apply_scoring_pipeline(
    embedding_score: f32,
    name: Option<&str>,
    file_part: &str,
    ctx: &ScoringContext<'_>,
) -> Option<f32> {
    // P1.16 defense-in-depth: clamp name_boost into [0.0, 1.0] regardless of
    // where it originated. CLI uses parse_unit_f32 (clap-bounded) and config
    // uses clamp_config_f32, but a future programmatic / deserialised path
    // could bypass both, in which case `(1.0 - 5.0) * embedding` would
    // sign-flip search results silently. Cheap insurance.
    let name_boost = ctx.filter.name_boost.clamp(0.0, 1.0);
    let base_score = if let Some(matcher) = ctx.name_matcher {
        let n = name.unwrap_or("");
        let name_score = matcher.score(n);
        (1.0 - name_boost) * embedding_score + name_boost * name_score
    } else {
        embedding_score
    };

    if let Some(matcher) = ctx.glob_matcher {
        if !matcher.is_match(file_part) {
            return None;
        }
    }

    let chunk_name = name.unwrap_or("");
    let mut score = base_score.max(0.0) * ctx.note_index.boost(file_part, chunk_name);

    if ctx.filter.enable_demotion {
        score *= chunk_importance(chunk_name, file_part);
    }

    if score >= ctx.threshold {
        Some(score)
    } else {
        None
    }
}

/// Score a single candidate chunk against the query.
///
/// Pure function — no database access. Combines embedding similarity, optional
/// name boosting, glob filtering, note boosting, and test-function demotion.
///
/// Returns `None` if the candidate is filtered out (glob mismatch or below threshold).
pub(crate) fn score_candidate(
    embedding: &[f32],
    name: Option<&str>,
    file_part: &str,
    ctx: &ScoringContext<'_>,
) -> Option<f32> {
    let base = cosine_similarity(ctx.query, embedding)?;
    apply_scoring_pipeline(base, name, file_part, ctx)
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

    #[test]
    fn bounded_heap_deterministic_under_reverse_push_order() {
        // Reverse insertion order ("c", "b", "a") with capacity 2: at the
        // eviction boundary "a" must displace "c" so the surviving set is
        // {"a", "b"} — the smallest-id-wins-among-ties invariant. Earlier
        // versions of `push` peeked the *best* boundary entry and ended up
        // with {"c", "a"} instead.
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("c".to_string(), 0.5);
        heap.push("b".to_string(), 0.5);
        heap.push("a".to_string(), 0.5);
        let top = heap.into_sorted_vec();
        let names: Vec<_> = top.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "b"],
            "expected smallest-id wins among ties"
        );
    }

    #[test]
    fn bounded_heap_deterministic_under_forward_push_order() {
        // Forward insertion order ("a", "b", "c") with capacity 2: "a" and
        // "b" are inserted under capacity, then "c" is rejected because
        // ("c" is not less than the worst tied id "b").
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("a".to_string(), 0.5);
        heap.push("b".to_string(), 0.5);
        heap.push("c".to_string(), 0.5);
        let top = heap.into_sorted_vec();
        let names: Vec<_> = top.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
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
                parser_version: 0,
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
        // v1.22.0 audit AC-4: `TestParseConfig` in `src/lib.go` is NOT a Go
        // test — Go tests live in `_test.go` files, not regular `.go` files.
        // The previous assertion (importance_test = 0.70) was wrong: it
        // demoted production helpers named `TestFoo` in non-test Go files.
        // After the fix, `TestParseConfig` in `src/lib.go` gets normal
        // importance (1.0), but `TestParseConfig` in `src/lib_test.go`
        // gets the test demotion via path-based detection.
        assert_eq!(chunk_importance("TestParseConfig", "src/lib.go"), 1.0);
        // In an actual _test.go file, the path pattern catches it.
        assert_eq!(
            chunk_importance("TestParseConfig", "src/lib_test.go"),
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

    /// Build a normalized EMBEDDING_DIM test vector for score_candidate tests.
    fn test_embedding(seed: f32) -> Vec<f32> {
        let mut v = vec![seed; crate::EMBEDDING_DIM];
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
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let score = score_candidate(&emb, None, "src/lib.rs", &ctx);
        assert!(score.is_some());
        assert!(
            score.unwrap() > 0.9,
            "Self-similarity should be ~1.0, got {}",
            score.unwrap()
        );
    }

    #[test]
    fn test_score_candidate_below_threshold() {
        let emb = test_embedding(1.0);
        let query = test_embedding(-1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.5,
        };

        let score = score_candidate(&emb, None, "src/lib.rs", &ctx);
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
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let glob = globset::Glob::new("src/**/*.rs").unwrap().compile_matcher();

        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: Some(&glob),
            note_index: &note_index,
            threshold: 0.0,
        };
        let score = score_candidate(&emb, None, "src/lib.rs", &ctx);
        assert!(score.is_some());

        let score = score_candidate(&emb, None, "tests/foo.py", &ctx);
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
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let matcher = NameMatcher::new("parseConfig");

        let ctx_no = ScoringContext {
            query: &query,
            filter: &filter_no_boost,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };
        let score_no = score_candidate(&emb, Some("parseConfig"), "src/a.rs", &ctx_no).unwrap();

        let ctx_yes = ScoringContext {
            query: &query,
            filter: &filter_with_boost,
            name_matcher: Some(&matcher),
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };
        let score_yes = score_candidate(&emb, Some("parseConfig"), "src/a.rs", &ctx_yes).unwrap();

        assert!(score_yes > 0.0);
        assert!(score_no > 0.0);
    }

    #[test]
    fn test_score_candidate_demotion() {
        let emb = test_embedding(1.0);
        let query = test_embedding(1.0);
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));

        let filter_no_demote = SearchFilter {
            enable_demotion: false,
            ..Default::default()
        };
        let filter_demote = SearchFilter {
            enable_demotion: true,
            ..Default::default()
        };

        let ctx_demote = ScoringContext {
            query: &query,
            filter: &filter_demote,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };
        let score_normal =
            score_candidate(&emb, Some("real_fn"), "src/lib.rs", &ctx_demote).unwrap();
        let score_test =
            score_candidate(&emb, Some("test_foo"), "src/lib.rs", &ctx_demote).unwrap();

        let ctx_no_demote = ScoringContext {
            query: &query,
            filter: &filter_no_demote,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };
        let score_no_demote =
            score_candidate(&emb, Some("test_foo"), "src/lib.rs", &ctx_no_demote).unwrap();

        assert!(score_test < score_normal, "test_ should be demoted");
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
        let note_index_boosted = NoteBoost::Borrowed(NoteBoostIndex::new(&notes));
        let note_index_empty = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));

        let ctx_boosted = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index_boosted,
            threshold: 0.0,
        };
        let score_boosted =
            score_candidate(&emb, Some("my_fn"), "src/lib.rs", &ctx_boosted).unwrap();

        let ctx_plain = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index_empty,
            threshold: 0.0,
        };
        let score_plain = score_candidate(&emb, Some("my_fn"), "src/lib.rs", &ctx_plain).unwrap();

        assert!(
            score_boosted > score_plain,
            "Positive note should boost score"
        );
    }

    // ===== Adversarial BoundedScoreHeap and score_candidate tests =====

    #[test]
    fn heap_all_nan_scores() {
        let mut heap = BoundedScoreHeap::new(5);
        heap.push("a".to_string(), f32::NAN);
        heap.push("b".to_string(), f32::NAN);
        heap.push("c".to_string(), f32::NAN);
        let results = heap.into_sorted_vec();
        assert!(
            results.is_empty(),
            "All NaN scores should produce empty results, got {} items",
            results.len()
        );
    }

    #[test]
    fn heap_mixed_valid_and_nan() {
        let mut heap = BoundedScoreHeap::new(10);
        heap.push("nan1".to_string(), f32::NAN);
        heap.push("ok1".to_string(), 0.7);
        heap.push("inf".to_string(), f32::INFINITY);
        heap.push("ok2".to_string(), 0.9);
        heap.push("nan2".to_string(), f32::NAN);
        heap.push("neginf".to_string(), f32::NEG_INFINITY);
        heap.push("ok3".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        // Only finite scores kept
        assert_eq!(results.len(), 3, "Only finite scores should be kept");
        // All results must be finite
        for (id, score) in &results {
            assert!(
                score.is_finite(),
                "Result '{id}' has non-finite score {score}"
            );
        }
        // Sorted descending
        assert_eq!(results[0].0, "ok2");
        assert_eq!(results[1].0, "ok1");
        assert_eq!(results[2].0, "ok3");
    }

    #[test]
    fn heap_negative_scores() {
        let mut heap = BoundedScoreHeap::new(5);
        heap.push("a".to_string(), -0.1);
        heap.push("b".to_string(), -0.5);
        heap.push("c".to_string(), -0.3);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 3, "All negative scores should be kept");
        // Sorted descending (least negative first)
        assert_eq!(results[0].0, "a", "Least negative should be first");
        assert_eq!(results[1].0, "c");
        assert_eq!(results[2].0, "b", "Most negative should be last");
    }

    #[test]
    fn heap_capacity_zero() {
        let mut heap = BoundedScoreHeap::new(0);
        heap.push("a".to_string(), 0.9);
        heap.push("b".to_string(), 0.8);
        let results = heap.into_sorted_vec();
        assert!(
            results.is_empty(),
            "Capacity-0 heap should always be empty, got {} items",
            results.len()
        );
    }

    #[test]
    fn score_candidate_nan_embedding_filtered() {
        let query = test_embedding(1.0);
        let mut nan_emb = vec![f32::NAN; crate::EMBEDDING_DIM];
        // Mix in some valid values to be thorough — even partial NaN should fail
        nan_emb[0] = 0.5;
        nan_emb[1] = 0.3;
        let filter = SearchFilter::default();
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let result = score_candidate(&nan_emb, Some("nan_fn"), "src/lib.rs", &ctx);
        assert!(
            result.is_none(),
            "NaN embedding should be filtered out (return None), got {:?}",
            result
        );
    }

    #[test]
    fn score_candidate_nan_query_filtered() {
        // TC-4: All-NaN query vector should not panic, should return None.
        let nan_query = vec![f32::NAN; crate::EMBEDDING_DIM];
        let normal_emb = test_embedding(1.0);
        let filter = SearchFilter::default();
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &nan_query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let result = score_candidate(&normal_emb, Some("my_fn"), "src/lib.rs", &ctx);
        assert!(
            result.is_none(),
            "NaN query should be filtered out (return None), got {:?}",
            result
        );
    }

    #[test]
    fn score_candidate_nan_both_filtered() {
        // TC-4: Both query and embedding NaN — must not panic.
        let nan_query = vec![f32::NAN; crate::EMBEDDING_DIM];
        let nan_emb = vec![f32::NAN; crate::EMBEDDING_DIM];
        let filter = SearchFilter::default();
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &nan_query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let result = score_candidate(&nan_emb, Some("fn"), "src/lib.rs", &ctx);
        assert!(
            result.is_none(),
            "All-NaN inputs should be filtered out, got {:?}",
            result
        );
    }

    #[test]
    fn score_candidate_zero_embedding() {
        let zero_query = vec![0.0f32; crate::EMBEDDING_DIM];
        let normal_emb = test_embedding(1.0);
        let filter = SearchFilter {
            query_text: "test".into(),
            ..Default::default()
        };
        let notes: Vec<NoteSummary> = vec![];
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&notes));
        let ctx = ScoringContext {
            query: &zero_query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let result = score_candidate(&normal_emb, None, "src/lib.rs", &ctx);
        match result {
            None => {}
            Some(v) => assert!(
                v.is_finite(),
                "score_candidate with zero query must return finite score, got {v}"
            ),
        }
    }

    #[test]
    fn apply_scoring_pipeline_preserves_fused_score() {
        let filter = SearchFilter::default();
        let query = test_embedding(1.0);
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let fused = 0.75;
        let result = apply_scoring_pipeline(fused, Some("my_fn"), "src/lib.rs", &ctx);
        assert_eq!(result, Some(0.75));
    }

    #[test]
    fn apply_scoring_pipeline_applies_name_boost_to_fused() {
        let mut filter = SearchFilter::default();
        filter.name_boost = 0.3;
        filter.query_text = "my_fn".to_string();
        let query = test_embedding(1.0);
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let matcher = NameMatcher::new("my_fn");
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: Some(&matcher),
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let fused = 0.6;
        let result = apply_scoring_pipeline(fused, Some("my_fn"), "src/lib.rs", &ctx).unwrap();
        assert!(
            result > fused,
            "name boost should increase fused score for exact match"
        );
    }

    #[test]
    fn apply_scoring_pipeline_applies_demotion_to_fused() {
        let filter = SearchFilter {
            enable_demotion: true,
            ..Default::default()
        };
        let query = test_embedding(1.0);
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.0,
        };

        let fused = 0.8;
        let prod = apply_scoring_pipeline(fused, Some("real_fn"), "src/lib.rs", &ctx).unwrap();
        let test = apply_scoring_pipeline(fused, Some("test_foo"), "src/lib.rs", &ctx).unwrap();
        assert!(
            test < prod,
            "test function should be demoted even with fused score"
        );
    }

    #[test]
    fn apply_scoring_pipeline_respects_threshold() {
        let filter = SearchFilter::default();
        let query = test_embedding(1.0);
        let note_index = NoteBoost::Borrowed(NoteBoostIndex::new(&[]));
        let ctx = ScoringContext {
            query: &query,
            filter: &filter,
            name_matcher: None,
            glob_matcher: None,
            note_index: &note_index,
            threshold: 0.9,
        };

        let result = apply_scoring_pipeline(0.5, Some("fn"), "src/lib.rs", &ctx);
        assert!(result.is_none());
    }
}
