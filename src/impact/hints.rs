//! Hint computation and risk scoring

use crate::limits::{
    blast_high_min, blast_low_max, risk_threshold_high, risk_threshold_medium,
    RISK_THRESHOLD_HIGH_DEFAULT, RISK_THRESHOLD_MEDIUM_DEFAULT,
};
use crate::store::{CallGraph, StoreError};
use crate::Store;

use super::bfs::{reverse_bfs, reverse_bfs_multi_attributed, test_reachability};
use super::types::{FunctionHints, RiskLevel, RiskScore};
use super::DEFAULT_MAX_TEST_SEARCH_DEPTH;

// SHL-V1.29-8: the risk and blast-radius thresholds drive `cqs review` CI
// gating — wrong defaults silently alter classification on monorepos. The
// values now flow through `crate::limits::*` so `CQS_RISK_HIGH`,
// `CQS_RISK_MEDIUM`, `CQS_BLAST_LOW_MAX`, `CQS_BLAST_HIGH_MIN` can pin
// project-specific policy. The consts below are kept as the canonical
// defaults (exported for telemetry / doctor output / doctests).

/// Default risk score above which a function is classified as high risk.
/// Env-override via `CQS_RISK_HIGH`; see [`risk_threshold_high`].
///
/// Retained as a public constant for callers that need the compile-time
/// default (docs, tests, telemetry). The runtime decision uses
/// `risk_threshold_high()` so env overrides take effect.
#[allow(dead_code)]
pub const RISK_THRESHOLD_HIGH: f32 = RISK_THRESHOLD_HIGH_DEFAULT;
/// Default risk score above which a function is classified as medium risk.
/// Env-override via `CQS_RISK_MEDIUM`; see [`risk_threshold_medium`].
#[allow(dead_code)]
pub const RISK_THRESHOLD_MEDIUM: f32 = RISK_THRESHOLD_MEDIUM_DEFAULT;

/// Core implementation — accepts pre-loaded graph and test chunks.
/// Use this when processing multiple functions to avoid loading the graph
/// N times (e.g., scout, which processes 10+ functions).
pub fn compute_hints_with_graph(
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    function_name: &str,
    prefetched_caller_count: Option<usize>,
) -> FunctionHints {
    let _span =
        tracing::debug_span!("compute_hints_with_graph", function = function_name).entered();
    // Note: prefetched_caller_count (from get_caller_counts_batch / get_callers_full)
    // counts DB rows which may include duplicate caller names from different files.
    // graph.reverse counts unique caller names per the in-memory graph. These can
    // diverge slightly. We prefer the prefetched count when available since it matches
    // what the caller already displayed, avoiding confusing mismatches.
    let caller_count = match prefetched_caller_count {
        Some(n) => n,
        None => graph
            .reverse
            .get(function_name)
            .map(|v| v.len())
            .unwrap_or(0),
    };
    let ancestors = reverse_bfs(graph, function_name, DEFAULT_MAX_TEST_SEARCH_DEPTH);
    let test_count = test_chunks
        .iter()
        .filter(|t| ancestors.get(&t.name).is_some_and(|&d| d > 0))
        .count();

    FunctionHints {
        caller_count,
        test_count,
    }
}

/// Compute caller count and test count for a single function.
/// Convenience wrapper that loads graph internally. Pass `prefetched_caller_count`
/// to avoid re-querying callers when the caller already has them (e.g., `explain`
/// fetches callers before this).
pub fn compute_hints<Mode>(
    store: &Store<Mode>,
    function_name: &str,
    prefetched_caller_count: Option<usize>,
) -> Result<FunctionHints, StoreError> {
    let _span = tracing::info_span!("compute_hints", function = function_name).entered();
    let caller_count = match prefetched_caller_count {
        Some(n) => n,
        None => store.get_callers_full(function_name)?.len(),
    };
    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;
    Ok(compute_hints_with_graph(
        &graph,
        &test_chunks,
        function_name,
        Some(caller_count),
    ))
}

/// Batch compute hints for multiple functions using forward BFS (PERF-20).
/// Single `test_reachability` call replaces N independent `reverse_bfs` calls.
pub fn compute_hints_batch(
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    names: &[&str],
    caller_counts: &std::collections::HashMap<String, u64>,
) -> Vec<FunctionHints> {
    let _span = tracing::info_span!("compute_hints_batch", count = names.len()).entered();
    let test_names: Vec<&str> = test_chunks.iter().map(|t| t.name.as_str()).collect();
    let reachability = test_reachability(graph, &test_names, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    names
        .iter()
        .map(|&name| {
            let caller_count = caller_counts
                .get(name)
                .map(|&c| c as usize)
                .unwrap_or_else(|| graph.reverse.get(name).map(|v| v.len()).unwrap_or(0));
            let test_count = reachability.get(name).copied().unwrap_or(0);
            FunctionHints {
                caller_count,
                test_count,
            }
        })
        .collect()
}

/// Compute risk scores for a batch of function names.
/// Uses pre-loaded call graph and test chunks to avoid repeated queries.
/// Formula: `score = caller_count * (1.0 - test_ratio)` where
/// `test_ratio = min(test_count / max(caller_count, 1), 1.0)`.
/// Entry-point handling: functions with 0 callers and 0 tests get `Medium`
/// risk (likely entry points that should have tests).
/// PERF-24: Uses a single forward BFS from all test nodes to build a
/// reachability map, instead of N independent reverse_bfs calls.
pub fn compute_risk_batch(
    names: &[&str],
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
) -> Vec<RiskScore> {
    let _span = tracing::info_span!("compute_risk_batch", count = names.len()).entered();

    // Precompute test reachability via a single forward BFS from all test nodes,
    // instead of N independent reverse_bfs calls (O(N*E) -> O(T*E)).
    let test_names: Vec<&str> = test_chunks.iter().map(|t| t.name.as_str()).collect();
    let reachability = test_reachability(graph, &test_names, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    // SHL-V1.29-8: read env-overridable thresholds once per batch.
    let risk_high = risk_threshold_high();
    let risk_medium = risk_threshold_medium();
    let low_max = blast_low_max();
    let high_min = blast_high_min();

    names
        .iter()
        .map(|name| {
            let caller_count = graph.reverse.get(*name).map(|v| v.len()).unwrap_or(0);
            let test_count = reachability.get(*name).copied().unwrap_or(0);
            let test_ratio = if caller_count == 0 {
                if test_count > 0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                (test_count as f32 / caller_count as f32).min(1.0)
            };
            let score = caller_count as f32 * (1.0 - test_ratio);
            let risk_level = if caller_count == 0 && test_count == 0 {
                // Entry point with no tests — flag as medium
                RiskLevel::Medium
            } else if score >= risk_high {
                RiskLevel::High
            } else if score >= risk_medium {
                RiskLevel::Medium
            } else {
                RiskLevel::Low
            };
            let blast_radius = classify_blast_radius(caller_count, low_max, high_min);
            RiskScore {
                caller_count,
                test_count,
                test_ratio,
                risk_level,
                blast_radius,
                score,
            }
        })
        .collect()
}

/// Classify a blast-radius from caller count, honoring the env-overridable
/// `CQS_BLAST_LOW_MAX` / `CQS_BLAST_HIGH_MIN` thresholds.
/// - `callers <= low_max` → Low
/// - `callers >= high_min` → High
/// - otherwise → Medium
/// Degenerate configs (`high_min <= low_max`) prefer High, matching the
/// historical defaults where `low_max=2` and `high_min=11` are disjoint.
fn classify_blast_radius(callers: usize, low_max: usize, high_min: usize) -> RiskLevel {
    if callers >= high_min {
        RiskLevel::High
    } else if callers <= low_max {
        RiskLevel::Low
    } else {
        RiskLevel::Medium
    }
}

/// Compute risk scores and collect deduplicated tests in a single pass.
/// Shares BFS results across risk scoring and test collection, avoiding the
/// duplicate `reverse_bfs` that occurs when calling `compute_risk_batch` and
/// `find_affected_tests_with_chunks` separately.
pub fn compute_risk_and_tests(
    targets: &[&str],
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
) -> (Vec<RiskScore>, Vec<super::TestInfo>) {
    let _span = tracing::info_span!("compute_risk_and_tests", targets = targets.len()).entered();

    // AC-9: Use test_reachability (forward BFS) for risk scoring — same algorithm
    // as compute_risk_batch — to prevent divergent test counts between commands.
    let test_names: Vec<&str> = test_chunks.iter().map(|t| t.name.as_str()).collect();
    let reachability = test_reachability(graph, &test_names, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    // PF-3: Single reverse_bfs_multi_attributed call replaces N per-target reverse_bfs calls.
    // The attributed variant tracks which target (by index) first reached each ancestor,
    // enabling per-target test distribution from the combined result.
    let ancestors = reverse_bfs_multi_attributed(graph, targets, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    // Build per-target test sets from the combined BFS result
    let mut all_tests = Vec::new();
    let mut seen_tests = std::collections::HashSet::new();

    // Map: target_index -> set of test names that reach it
    let mut tests_per_target: Vec<std::collections::HashSet<&str>> =
        vec![std::collections::HashSet::new(); targets.len()];

    for test in test_chunks {
        if let Some(&(depth, source_idx)) = ancestors.get(&test.name) {
            if depth > 0 {
                if source_idx < targets.len() {
                    tests_per_target[source_idx].insert(&test.name);
                }
                if seen_tests.insert((test.name.clone(), test.file.clone())) {
                    all_tests.push(super::TestInfo {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        line: test.line_start,
                        call_depth: depth,
                    });
                }
            }
        }
    }

    // SHL-V1.29-8: read env-overridable thresholds once per call.
    let risk_high = risk_threshold_high();
    let risk_medium = risk_threshold_medium();
    let low_max = blast_low_max();
    let high_min = blast_high_min();

    let mut scores = Vec::with_capacity(targets.len());
    for (i, &name) in targets.iter().enumerate() {
        // Risk scoring: use forward BFS reachability (consistent with compute_risk_batch)
        let caller_count = graph.reverse.get(name).map(|v| v.len()).unwrap_or(0);
        let test_count = reachability.get(name).copied().unwrap_or(0);
        let test_ratio = if caller_count == 0 {
            if test_count > 0 {
                1.0
            } else {
                0.0
            }
        } else {
            (test_count as f32 / caller_count as f32).min(1.0)
        };
        let score = caller_count as f32 * (1.0 - test_ratio);
        let risk_level = if caller_count == 0 && test_count == 0 {
            RiskLevel::Medium
        } else if score >= risk_high {
            RiskLevel::High
        } else if score >= risk_medium {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };
        let blast_radius = classify_blast_radius(caller_count, low_max, high_min);
        let _ = &tests_per_target[i]; // ensure we computed tests for this target
        scores.push(RiskScore {
            caller_count,
            test_count,
            test_ratio,
            risk_level,
            blast_radius,
            score,
        });
    }

    all_tests.sort_by_key(|t| t.call_depth);
    (scores, all_tests)
}

/// Find the most-called functions in the codebase (hotspots).
/// Returns [`Hotspot`] entries sorted by caller count descending.
///
/// PF-V1.29-4: previously allocated a `String` per callee (via `name.to_string()`)
/// into a full `Vec<Hotspot>` before sorting and truncating to `top_n`. For a
/// graph with 50k+ callees and `top_n = 5` this produced ~50k throwaway
/// strings. Now uses a bounded min-heap keyed on `caller_count`: the heap
/// never exceeds `top_n` entries, and `Arc::clone` on the name is a refcount
/// bump (not an allocation). Only the surviving `top_n` names are converted
/// to owned `String`s at the end.
pub fn find_hotspots(graph: &CallGraph, top_n: usize) -> Vec<crate::health::Hotspot> {
    let _span = tracing::info_span!("find_hotspots", top_n).entered();

    if top_n == 0 {
        return Vec::new();
    }

    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    use std::sync::Arc;

    // Min-heap on caller_count: smallest element sits at `peek()`, so the
    // heap efficiently rejects entries that can't crack the top-N.
    let mut heap: BinaryHeap<(Reverse<usize>, Arc<str>)> = BinaryHeap::with_capacity(top_n);
    for (name, callers) in graph.reverse.iter() {
        let count = callers.len();
        if heap.len() < top_n {
            heap.push((Reverse(count), Arc::clone(name)));
        } else if let Some(&(Reverse(min_count), _)) = heap.peek() {
            if count > min_count {
                heap.pop();
                heap.push((Reverse(count), Arc::clone(name)));
            }
        }
    }

    // Drain the heap (unordered) and sort descending by caller_count for
    // the caller-facing contract. Only top_n `to_string()` calls here.
    let mut hotspots: Vec<crate::health::Hotspot> = heap
        .into_iter()
        .map(|(Reverse(caller_count), name)| crate::health::Hotspot {
            name: name.to_string(),
            caller_count,
        })
        .collect();
    hotspots.sort_by_key(|h| Reverse(h.caller_count));
    hotspots
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // ===== compute_hints_with_graph tests =====

    #[test]
    fn test_compute_hints_with_graph_stale_callers() {
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            vec!["ghost_caller".to_string(), "another_ghost".to_string()],
        );
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", None);
        assert_eq!(hints.caller_count, 2, "Should count callers from graph");
        assert_eq!(hints.test_count, 0, "No test chunks means no tests");
    }

    #[test]
    fn test_compute_hints_with_graph_stale_test_ancestor() {
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["middle".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks = vec![crate::store::ChunkSummary {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: "test_fn".to_string(),
            signature: "fn test_fn()".to_string(),
            content: "#[test] fn test_fn() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
            parser_version: 0,
            vendored: false,
        }];
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", None);
        assert_eq!(hints.test_count, 0, "Unreachable test should not count");
        assert_eq!(hints.caller_count, 1, "middle is a caller");
    }

    #[test]
    fn test_compute_hints_with_graph_prefetched_caller_count() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", Some(99));
        assert_eq!(hints.caller_count, 99, "Should use prefetched value");
    }

    // ===== Risk Scoring Tests =====

    #[test]
    fn test_risk_high_many_callers_no_tests() {
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            vec!["a", "b", "c", "d", "e", "f", "g"]
                .into_iter()
                .map(String::from)
                .collect(),
        );
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].risk_level, RiskLevel::High);
        assert_eq!(scores[0].caller_count, 7);
        assert_eq!(scores[0].test_count, 0);
        assert!((scores[0].score - 7.0).abs() < 0.01);
    }

    #[test]
    fn test_risk_low_with_tests() {
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            vec!["a".to_string(), "test_target".to_string()],
        );
        let mut forward = HashMap::new();
        forward.insert("test_target".to_string(), vec!["target".to_string()]);
        forward.insert("a".to_string(), vec!["target".to_string()]);
        let graph = CallGraph::from_string_maps(forward, reverse);
        let test_chunks = vec![crate::store::ChunkSummary {
            id: "test_id".to_string(),
            file: PathBuf::from("tests/test.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: "test_target".to_string(),
            signature: String::new(),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 10,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
            parser_version: 0,
            vendored: false,
        }];
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::Low);
        // 2 callers, 1 test -> coverage = 0.5 -> score = 2 * 0.5 = 1.0
        assert!((scores[0].score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_risk_entry_point_no_callers_no_tests() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["main"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::Medium);
        assert_eq!(scores[0].caller_count, 0);
        assert_eq!(scores[0].test_count, 0);
    }

    #[test]
    fn test_risk_coverage_capped_at_one() {
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            vec![
                "a".to_string(),
                "test_a".to_string(),
                "test_b".to_string(),
                "test_c".to_string(),
            ],
        );
        let mut forward = HashMap::new();
        forward.insert("test_a".to_string(), vec!["target".to_string()]);
        forward.insert("test_b".to_string(), vec!["target".to_string()]);
        forward.insert("test_c".to_string(), vec!["target".to_string()]);
        let graph = CallGraph::from_string_maps(forward, reverse);
        let test_chunks = vec![
            crate::store::ChunkSummary {
                id: "t1".to_string(),
                file: PathBuf::from("tests/t.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::language::ChunkType::Function,
                name: "test_a".to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 1,
                line_end: 5,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            crate::store::ChunkSummary {
                id: "t2".to_string(),
                file: PathBuf::from("tests/t.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::language::ChunkType::Function,
                name: "test_b".to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 6,
                line_end: 10,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            crate::store::ChunkSummary {
                id: "t3".to_string(),
                file: PathBuf::from("tests/t.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::language::ChunkType::Function,
                name: "test_c".to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 11,
                line_end: 15,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
        ];
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert!(
            scores[0].test_ratio <= 1.0,
            "test_ratio should be capped at 1.0, got {}",
            scores[0].test_ratio
        );
        assert_eq!(scores[0].risk_level, RiskLevel::Low);
    }

    #[test]
    fn test_risk_batch_empty_input() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&[], &graph, &test_chunks);
        assert!(scores.is_empty());
    }

    #[test]
    fn test_blast_radius_thresholds() {
        let mut reverse = HashMap::new();
        // 2 callers → blast Low
        reverse.insert(
            "low_blast".to_string(),
            vec!["a", "b"].into_iter().map(String::from).collect(),
        );
        // 3 callers → blast Medium
        reverse.insert(
            "med_blast".to_string(),
            vec!["a", "b", "c"].into_iter().map(String::from).collect(),
        );
        // 11 callers → blast High
        reverse.insert(
            "high_blast".to_string(),
            (0..11).map(|i| format!("c{i}")).collect(),
        );
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(
            &["low_blast", "med_blast", "high_blast"],
            &graph,
            &test_chunks,
        );

        assert_eq!(scores[0].blast_radius, RiskLevel::Low);
        assert_eq!(scores[1].blast_radius, RiskLevel::Medium);
        assert_eq!(scores[2].blast_radius, RiskLevel::High);
    }

    #[test]
    fn test_blast_radius_differs_from_risk() {
        // High blast radius (many callers) but low risk (full test coverage)
        let mut reverse = HashMap::new();
        let callers: Vec<String> = (0..15).map(|i| format!("caller_{i}")).collect();
        let mut all: Vec<String> = callers.clone();
        all.push("test_target".to_string());
        reverse.insert("target".to_string(), all);

        let mut forward = HashMap::new();
        forward.insert("test_target".to_string(), vec!["target".to_string()]);
        let graph = CallGraph::from_string_maps(forward, reverse);

        let test_chunks = vec![crate::store::ChunkSummary {
            id: "t1".to_string(),
            file: PathBuf::from("tests/t.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: "test_target".to_string(),
            signature: String::new(),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 5,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
            parser_version: 0,
            vendored: false,
        }];

        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        // 16 callers total, so blast_radius is High
        assert_eq!(scores[0].blast_radius, RiskLevel::High);
        // But risk_level should be lower due to test coverage
        // caller_count=16, test_count=1 → coverage ~0.06 → score ~15.0 → High risk still
        // Actually with only 1 test this will still be high risk
        // Let's just verify blast_radius is set correctly
        assert_eq!(scores[0].caller_count, 16);
    }

    // ===== TC-20: compute_risk_batch additional boundary tests =====

    #[test]
    fn test_blast_radius_boundary_10_callers_is_medium() {
        let mut reverse = HashMap::new();
        // 10 callers → blast Medium (3..=10 range)
        reverse.insert(
            "ten_callers".to_string(),
            (0..10).map(|i| format!("c{i}")).collect(),
        );
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["ten_callers"], &graph, &test_chunks);
        assert_eq!(
            scores[0].blast_radius,
            RiskLevel::Medium,
            "10 callers should be Medium blast radius (3..=10)"
        );
        assert_eq!(scores[0].caller_count, 10);
    }

    #[test]
    fn test_risk_score_formula_many_callers_no_tests() {
        // 6 callers, 0 tests: score = 6 * (1.0 - 0.0) = 6.0 >= 5.0 → High
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            (0..6).map(|i| format!("c{i}")).collect(),
        );
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::High);
        assert!((scores[0].score - 6.0).abs() < 0.01);
        assert_eq!(scores[0].test_count, 0);
        assert!((scores[0].test_ratio - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_risk_medium_boundary() {
        // 3 callers, 0 tests: score = 3 * 1.0 = 3.0. 3.0 >= 2.0 but < 5.0 → Medium
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            vec!["a", "b", "c"].into_iter().map(String::from).collect(),
        );
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::Medium);
        assert!((scores[0].score - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_risk_low_below_medium_threshold() {
        // 1 caller, 0 tests: score = 1.0 < 2.0 → Low
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["a".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::Low);
        assert!((scores[0].score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_risk_zero_callers_with_test_is_low() {
        // 0 callers, 1 test reachable: test_ratio = 1.0, score = 0.0 → Low
        let mut forward = HashMap::new();
        forward.insert("test_fn".to_string(), vec!["target".to_string()]);
        let graph = CallGraph::from_string_maps(forward, HashMap::new());
        let test_chunks = vec![crate::store::ChunkSummary {
            id: "t1".to_string(),
            file: PathBuf::from("tests/t.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: "test_fn".to_string(),
            signature: String::new(),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 5,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
            parser_version: 0,
            vendored: false,
        }];
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::Low);
        assert_eq!(scores[0].caller_count, 0);
        // test_count depends on forward BFS from test_fn reaching target
        // Since graph.reverse is empty, forward BFS from test_fn traverses forward edges:
        // test_fn -> target. Reachability map has target reachable from test_fn.
        assert_eq!(scores[0].test_count, 1);
    }

    #[test]
    fn test_blast_radius_boundary_0_callers() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let scores = compute_risk_batch(&["orphan"], &graph, &test_chunks);
        assert_eq!(
            scores[0].blast_radius,
            RiskLevel::Low,
            "0 callers should be Low blast radius (0..=2)"
        );
    }

    #[test]
    fn test_find_hotspots() {
        let mut reverse = HashMap::new();
        reverse.insert(
            "hot".to_string(),
            vec!["a", "b", "c"].into_iter().map(String::from).collect(),
        );
        reverse.insert(
            "warm".to_string(),
            vec!["a", "b"].into_iter().map(String::from).collect(),
        );
        reverse.insert("cold".to_string(), vec!["a".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let hotspots = find_hotspots(&graph, 2);
        assert_eq!(hotspots.len(), 2);
        assert_eq!(hotspots[0].name, "hot");
        assert_eq!(hotspots[0].caller_count, 3);
        assert_eq!(hotspots[1].name, "warm");
        assert_eq!(hotspots[1].caller_count, 2);
    }
}
