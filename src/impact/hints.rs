//! Hint computation and risk scoring

use crate::store::CallGraph;
use crate::Store;

use super::bfs::reverse_bfs;
use super::types::{FunctionHints, RiskLevel, RiskScore};
use super::DEFAULT_MAX_TEST_SEARCH_DEPTH;

/// Core implementation — accepts pre-loaded graph and test chunks.
///
/// Use this when processing multiple functions to avoid loading the graph
/// N times (e.g., scout, which processes 10+ functions).
///
/// `max_test_depth` controls BFS depth for test discovery (default: [`DEFAULT_MAX_TEST_SEARCH_DEPTH`]).
pub fn compute_hints_with_graph(
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    function_name: &str,
    prefetched_caller_count: Option<usize>,
) -> FunctionHints {
    compute_hints_with_graph_depth(
        graph,
        test_chunks,
        function_name,
        prefetched_caller_count,
        DEFAULT_MAX_TEST_SEARCH_DEPTH,
    )
}

/// Like [`compute_hints_with_graph`] but with configurable BFS depth.
pub fn compute_hints_with_graph_depth(
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    function_name: &str,
    prefetched_caller_count: Option<usize>,
    max_test_depth: usize,
) -> FunctionHints {
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
    let ancestors = reverse_bfs(graph, function_name, max_test_depth);
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
///
/// Convenience wrapper that loads graph internally. Pass `prefetched_caller_count`
/// to avoid re-querying callers when the caller already has them (e.g., `explain`
/// fetches callers before this).
pub fn compute_hints(
    store: &Store,
    function_name: &str,
    prefetched_caller_count: Option<usize>,
) -> anyhow::Result<FunctionHints> {
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

/// Compute risk scores for a batch of function names.
///
/// Uses pre-loaded call graph and test chunks to avoid repeated queries.
/// Formula: `score = caller_count * (1.0 - coverage)` where
/// `coverage = min(test_count / max(caller_count, 1), 1.0)`.
///
/// Entry-point handling: functions with 0 callers and 0 tests get `Medium`
/// risk (likely entry points that should have tests).
pub fn compute_risk_batch(
    names: &[&str],
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
) -> Vec<RiskScore> {
    let _span = tracing::info_span!("compute_risk_batch", count = names.len()).entered();

    names
        .iter()
        .map(|name| {
            let hints = compute_hints_with_graph(graph, test_chunks, name, None);
            let caller_count = hints.caller_count;
            let test_count = hints.test_count;
            let coverage = if caller_count == 0 {
                if test_count > 0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                (test_count as f32 / caller_count as f32).min(1.0)
            };
            let score = caller_count as f32 * (1.0 - coverage);
            let risk_level = if caller_count == 0 && test_count == 0 {
                // Entry point with no tests — flag as medium
                RiskLevel::Medium
            } else if score >= 5.0 {
                RiskLevel::High
            } else if score >= 2.0 {
                RiskLevel::Medium
            } else {
                RiskLevel::Low
            };
            RiskScore {
                name: name.to_string(),
                caller_count,
                test_count,
                coverage,
                risk_level,
                score,
            }
        })
        .collect()
}

/// Find the most-called functions in the codebase (hotspots).
///
/// Returns `(function_name, caller_count)` sorted by caller count descending.
pub fn find_hotspots(graph: &CallGraph, top_n: usize) -> Vec<(String, usize)> {
    let _span = tracing::info_span!("find_hotspots", top_n).entered();

    let mut hotspots: Vec<(String, usize)> = graph
        .reverse
        .iter()
        .map(|(name, callers)| (name.clone(), callers.len()))
        .collect();
    hotspots.sort_by(|a, b| b.1.cmp(&a.1));
    hotspots.truncate(top_n);
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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", None);
        assert_eq!(hints.caller_count, 2, "Should count callers from graph");
        assert_eq!(hints.test_count, 0, "No test chunks means no tests");
    }

    #[test]
    fn test_compute_hints_with_graph_stale_test_ancestor() {
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["middle".to_string()]);
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
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
        }];
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", None);
        assert_eq!(hints.test_count, 0, "Unreachable test should not count");
        assert_eq!(hints.caller_count, 1, "middle is a caller");
    }

    #[test]
    fn test_compute_hints_with_graph_prefetched_caller_count() {
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        };
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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
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
        }];
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert_eq!(scores[0].risk_level, RiskLevel::Low);
        // 2 callers, 1 test -> coverage = 0.5 -> score = 2 * 0.5 = 1.0
        assert!((scores[0].score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_risk_entry_point_no_callers_no_tests() {
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        };
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
        let graph = CallGraph { forward, reverse };
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
            },
        ];
        let scores = compute_risk_batch(&["target"], &graph, &test_chunks);
        assert!(
            scores[0].coverage <= 1.0,
            "Coverage should be capped at 1.0, got {}",
            scores[0].coverage
        );
        assert_eq!(scores[0].risk_level, RiskLevel::Low);
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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        let hotspots = find_hotspots(&graph, 2);
        assert_eq!(hotspots.len(), 2);
        assert_eq!(hotspots[0].0, "hot");
        assert_eq!(hotspots[0].1, 3);
        assert_eq!(hotspots[1].0, "warm");
        assert_eq!(hotspots[1].1, 2);
    }
}
