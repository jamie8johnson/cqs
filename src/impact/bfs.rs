//! BFS graph traversal for impact analysis

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::store::CallGraph;

/// Maximum nodes in any reverse BFS traversal (RT-RES-1).
/// Prevents unbounded expansion on hub functions with thousands of transitive callers.
pub(super) const DEFAULT_BFS_MAX_NODES: usize = 10_000;

/// Reverse BFS from a target node, returning all ancestors with their depths.
///
/// The target itself is always included at depth 0. Callers that need only
/// actual ancestors should filter out depth-0 entries.
///
/// Expansion stops when either `max_depth` or `DEFAULT_BFS_MAX_NODES` is reached.
pub(super) fn reverse_bfs(
    graph: &CallGraph,
    target: &str,
    max_depth: usize,
) -> HashMap<String, usize> {
    let mut ancestors: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target.to_string(), 0);
    queue.push_back((target.to_string(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        if ancestors.len() >= DEFAULT_BFS_MAX_NODES {
            tracing::warn!(
                target,
                nodes = ancestors.len(),
                "reverse_bfs hit node cap, returning partial results"
            );
            break;
        }
        if let Some(callers) = graph.reverse.get(current.as_str()) {
            for caller in callers {
                if !ancestors.contains_key(caller.as_ref()) {
                    ancestors.insert(caller.to_string(), d + 1);
                    queue.push_back((caller.to_string(), d + 1));
                }
            }
        }
    }

    ancestors
}

/// Multi-source reverse BFS from multiple target nodes simultaneously.
///
/// Instead of calling `reverse_bfs()` N times (one per changed function),
/// starts BFS from all targets at once. Each node gets the minimum depth
/// from any starting node. Returns ancestors with their minimum depths.
///
/// Production code uses `reverse_bfs_multi_attributed` instead (same traversal
/// but also tracks which source produced each path). Kept for tests.
#[cfg(test)]
pub(super) fn reverse_bfs_multi(
    graph: &CallGraph,
    targets: &[&str],
    max_depth: usize,
) -> HashMap<String, usize> {
    let mut ancestors: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    for &target in targets {
        ancestors.insert(target.to_string(), 0);
        queue.push_back((target.to_string(), 0));
    }

    while let Some((current, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        if ancestors.len() >= DEFAULT_BFS_MAX_NODES {
            tracing::warn!(
                nodes = ancestors.len(),
                "reverse_bfs_multi hit node cap, returning partial results"
            );
            break;
        }
        // Skip stale queue entries: if this node was later reached via a
        // shorter path, the HashMap already has a smaller depth. Processing
        // the stale entry would propagate incorrect (longer) depths downstream.
        if ancestors.get(&current).is_some_and(|&stored| d > stored) {
            continue;
        }
        if let Some(callers) = graph.reverse.get(current.as_str()) {
            for caller in callers {
                match ancestors.entry(caller.to_string()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(d + 1);
                        queue.push_back((caller.to_string(), d + 1));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        // Update if we found a shorter path
                        if d + 1 < *e.get() {
                            *e.get_mut() = d + 1;
                            queue.push_back((caller.to_string(), d + 1));
                        }
                    }
                }
            }
        }
    }

    ancestors
}

/// Multi-source reverse BFS that tracks which target (by index) first reached each node.
///
/// Like [`reverse_bfs_multi`], starts BFS from all targets simultaneously and records
/// minimum depth. Additionally tracks which target index produced the shortest path to
/// each node, enabling per-source attribution without separate BFS calls.
///
/// Returns `HashMap<node_name, (min_depth, source_index)>` where `source_index` is
/// the index into `targets` that first reached the node at minimum depth.
pub(super) fn reverse_bfs_multi_attributed(
    graph: &CallGraph,
    targets: &[&str],
    max_depth: usize,
) -> HashMap<String, (usize, usize)> {
    // (depth, source_index)
    let mut ancestors: HashMap<String, (usize, usize)> = HashMap::new();
    // Queue entries: (node_name, depth, source_index)
    let mut queue: VecDeque<(String, usize, usize)> = VecDeque::new();

    for (idx, &target) in targets.iter().enumerate() {
        match ancestors.entry(target.to_string()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert((0, idx));
                queue.push_back((target.to_string(), 0, idx));
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                // Duplicate target name — first occurrence wins at depth 0
            }
        }
    }

    while let Some((current, d, src)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        if ancestors.len() >= DEFAULT_BFS_MAX_NODES {
            tracing::warn!(
                nodes = ancestors.len(),
                "reverse_bfs_multi_attributed hit node cap, returning partial results"
            );
            break;
        }
        // Skip stale queue entries (same logic as reverse_bfs_multi)
        if ancestors
            .get(&current)
            .is_some_and(|&(stored_d, _)| d > stored_d)
        {
            continue;
        }
        if let Some(callers) = graph.reverse.get(current.as_str()) {
            for caller in callers {
                match ancestors.entry(caller.to_string()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert((d + 1, src));
                        queue.push_back((caller.to_string(), d + 1, src));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        if d + 1 < e.get().0 {
                            *e.get_mut() = (d + 1, src);
                            queue.push_back((caller.to_string(), d + 1, src));
                        }
                    }
                }
            }
        }
    }

    ancestors
}

/// Build a test reachability map via forward BFS from all test nodes.
///
/// Walks forward edges (caller -> callee) from each test node, collecting
/// all functions reachable within `max_depth` hops. Returns a map of
/// `function_name -> count of tests that reach it`.
///
/// **Optimization (PERF-23):** Tests with identical first-hop callee sets
/// produce identical reachable sets (beyond the test node itself). We group
/// tests into equivalence classes by their direct callees and BFS once per
/// unique class, multiplying counts by the class size.
pub(crate) fn test_reachability(
    graph: &CallGraph,
    test_names: &[&str],
    max_depth: usize,
) -> HashMap<String, usize> {
    let _span = tracing::info_span!("test_reachability", tests = test_names.len()).entered();
    let mut counts: HashMap<String, usize> = HashMap::new();

    // Step 1: Group tests by their first-hop callee set.
    // Tests with the same direct callees will traverse the same subgraph
    // (forward BFS from depth 1 onward is identical), so we only BFS once.
    let mut equivalence_classes: HashMap<BTreeSet<&str>, usize> = HashMap::new();
    for &test_name in test_names {
        let callees: BTreeSet<&str> = graph
            .forward
            .get(test_name)
            .map(|v| v.iter().map(|s| s.as_ref()).collect())
            .unwrap_or_default();
        *equivalence_classes.entry(callees).or_default() += 1;
    }

    // Step 2: BFS once per unique callee set, multiply counts by class size.
    let mut visited: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    for (callee_set, class_size) in &equivalence_classes {
        if callee_set.is_empty() {
            // Tests with no callees reach nothing — skip BFS entirely
            continue;
        }

        // Reuse allocations: clear instead of reallocating
        visited.clear();
        queue.clear();

        // Seed BFS from the shared callees at depth 1 (the test node itself
        // is excluded from counts, so we start at depth 1 directly)
        for &callee in callee_set {
            visited.insert(callee.to_string(), 1);
            queue.push_back((callee.to_string(), 1));
        }

        while let Some((current, d)) = queue.pop_front() {
            if d >= max_depth {
                continue;
            }
            if visited.len() >= DEFAULT_BFS_MAX_NODES {
                tracing::warn!(
                    nodes = visited.len(),
                    "test_reachability BFS hit node cap, returning partial results"
                );
                break;
            }
            if let Some(callees) = graph.forward.get(current.as_str()) {
                for callee in callees {
                    if !visited.contains_key(callee.as_ref()) {
                        visited.insert(callee.to_string(), d + 1);
                        queue.push_back((callee.to_string(), d + 1));
                    }
                }
            }
        }

        // Every function visited is reachable from all tests in this class
        for name in visited.keys() {
            *counts.entry(name.clone()).or_default() += class_size;
        }
    }

    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_reverse_bfs_empty_graph() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let result = reverse_bfs(&graph, "target", 5);
        assert_eq!(result.len(), 1); // Just the target itself at depth 0
        assert_eq!(result["target"], 0);
    }

    #[test]
    fn test_reverse_bfs_chain() {
        let mut reverse = HashMap::new();
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let result = reverse_bfs(&graph, "C", 5);
        assert_eq!(result["C"], 0);
        assert_eq!(result["B"], 1);
        assert_eq!(result["A"], 2);
    }

    #[test]
    fn test_reverse_bfs_respects_depth() {
        let mut reverse = HashMap::new();
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let result = reverse_bfs(&graph, "C", 1);
        assert_eq!(result.len(), 2); // C at 0, B at 1
        assert!(!result.contains_key("A")); // Beyond depth limit
    }

    // ===== reverse_bfs_multi tests =====

    #[test]
    fn test_reverse_bfs_multi_empty_targets() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let result = reverse_bfs_multi(&graph, &[], 5);
        assert!(
            result.is_empty(),
            "Empty targets should produce empty result"
        );
    }

    #[test]
    fn test_reverse_bfs_multi_non_overlapping_ancestors() {
        // Two separate call chains with no shared ancestors:
        //   A -> B -> target1
        //   C -> D -> target2
        let mut reverse = HashMap::new();
        reverse.insert("target1".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        reverse.insert("target2".to_string(), vec!["D".to_string()]);
        reverse.insert("D".to_string(), vec!["C".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi(&graph, &["target1", "target2"], 5);

        // Both targets at depth 0
        assert_eq!(result["target1"], 0);
        assert_eq!(result["target2"], 0);
        // All ancestors found
        assert_eq!(result["B"], 1);
        assert_eq!(result["A"], 2);
        assert_eq!(result["D"], 1);
        assert_eq!(result["C"], 2);
        assert_eq!(result.len(), 6); // 2 targets + 4 ancestors
    }

    #[test]
    fn test_reverse_bfs_multi_shared_ancestor() {
        // Shared caller reaches both targets at different depths:
        //   shared -> mid -> target1
        //   shared -> target2
        let mut reverse = HashMap::new();
        reverse.insert("target1".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["shared".to_string()]);
        reverse.insert("target2".to_string(), vec!["shared".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi(&graph, &["target1", "target2"], 5);

        assert_eq!(result["target1"], 0);
        assert_eq!(result["target2"], 0);
        assert_eq!(result["mid"], 1);
        // shared is at depth 1 from target2 and depth 2 from target1
        // Multi-source BFS should record minimum depth = 1
        assert_eq!(
            result["shared"], 1,
            "Shared ancestor should get minimum depth across all sources"
        );
    }

    #[test]
    fn test_reverse_bfs_multi_depth_limit() {
        // Chain: A -> B -> C -> target1, D -> target2
        // With depth limit 1, should only find immediate callers
        let mut reverse = HashMap::new();
        reverse.insert("target1".to_string(), vec!["C".to_string()]);
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        reverse.insert("target2".to_string(), vec!["D".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi(&graph, &["target1", "target2"], 1);

        assert_eq!(result["target1"], 0);
        assert_eq!(result["target2"], 0);
        assert_eq!(result["C"], 1, "Direct caller of target1 should be found");
        assert_eq!(result["D"], 1, "Direct caller of target2 should be found");
        assert!(
            !result.contains_key("B"),
            "B is at depth 2 from target1, beyond limit"
        );
        assert!(
            !result.contains_key("A"),
            "A is at depth 3 from target1, beyond limit"
        );
    }

    #[test]
    fn test_reverse_bfs_multi_single_target_matches_reverse_bfs() {
        // With a single target, multi should produce the same result as single
        let mut reverse = HashMap::new();
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let single = reverse_bfs(&graph, "C", 5);
        let multi = reverse_bfs_multi(&graph, &["C"], 5);

        assert_eq!(
            single, multi,
            "Single-target multi should match reverse_bfs"
        );
    }

    #[test]
    fn test_reverse_bfs_multi_diamond_graph() {
        // Diamond: A -> B, A -> C, B -> D, C -> D
        // Starting from D: should find B(1), C(1), A(2)
        // Starting from both D and B: D(0), B(0), C(1), A(1)
        let mut reverse = HashMap::new();
        reverse.insert("D".to_string(), vec!["B".to_string(), "C".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        reverse.insert("C".to_string(), vec!["A".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi(&graph, &["D", "B"], 5);

        assert_eq!(result["D"], 0);
        assert_eq!(result["B"], 0); // Target, so depth 0
        assert_eq!(result["C"], 1); // Caller of D
        assert_eq!(
            result["A"], 1,
            "A is at depth 1 from B (target), not depth 2 from D"
        );
    }

    #[test]
    fn test_reverse_bfs_multi_stale_queue_entry() {
        // Regression test for #407: stale queue entries propagate wrong depths.
        //
        // Graph (reverse edges, i.e., "who calls"):
        //   target1 <- mid <- deep   (chain: deep calls mid calls target1)
        //   target2 <- deep          (deep also calls target2 directly)
        //
        // Multi-BFS from [target1, target2]:
        //   - "deep" is first reached at depth 2 via target1 <- mid <- deep
        //   - "deep" is then reached at depth 1 via target2 <- deep
        //   - Without the stale-entry fix, the queue still has (deep, 2) which
        //     would propagate depth 3 to deep's callers instead of depth 2.
        let mut reverse = HashMap::new();
        reverse.insert("target1".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["deep".to_string()]);
        reverse.insert("target2".to_string(), vec!["deep".to_string()]);
        reverse.insert("deep".to_string(), vec!["root".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi(&graph, &["target1", "target2"], 5);

        assert_eq!(result["target1"], 0);
        assert_eq!(result["target2"], 0);
        assert_eq!(result["mid"], 1);
        assert_eq!(
            result["deep"], 1,
            "deep should be depth 1 (from target2), not 2 (from target1->mid)"
        );
        assert_eq!(
            result["root"], 2,
            "root should be depth 2 (deep+1), not 3 (from stale queue entry)"
        );
    }

    // ===== reverse_bfs_multi_attributed tests =====

    #[test]
    fn test_attributed_empty_targets() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let result = reverse_bfs_multi_attributed(&graph, &[], 5);
        assert!(result.is_empty());
    }

    #[test]
    fn test_attributed_single_target() {
        // Chain: A -> B -> target
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi_attributed(&graph, &["target"], 5);

        assert_eq!(result["target"], (0, 0));
        assert_eq!(result["B"], (1, 0));
        assert_eq!(result["A"], (2, 0));
    }

    #[test]
    fn test_attributed_two_sources_separate_chains() {
        // A -> target0, B -> target1 (no overlap)
        let mut reverse = HashMap::new();
        reverse.insert("target0".to_string(), vec!["A".to_string()]);
        reverse.insert("target1".to_string(), vec!["B".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi_attributed(&graph, &["target0", "target1"], 5);

        assert_eq!(result["target0"], (0, 0));
        assert_eq!(result["target1"], (0, 1));
        assert_eq!(result["A"], (1, 0));
        assert_eq!(result["B"], (1, 1));
    }

    #[test]
    fn test_attributed_shared_ancestor_gets_closest_source() {
        // shared -> mid -> target0  (depth 2 from target0)
        // shared -> target1         (depth 1 from target1)
        // shared should be attributed to target1 (index 1) at depth 1
        let mut reverse = HashMap::new();
        reverse.insert("target0".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["shared".to_string()]);
        reverse.insert("target1".to_string(), vec!["shared".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi_attributed(&graph, &["target0", "target1"], 5);

        assert_eq!(result["shared"].0, 1, "min depth is 1 from target1");
        assert_eq!(result["shared"].1, 1, "attributed to target1 (index 1)");
    }

    #[test]
    fn test_attributed_depth_matches_multi() {
        // Verify depths agree with reverse_bfs_multi
        let mut reverse = HashMap::new();
        reverse.insert("target0".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["shared".to_string()]);
        reverse.insert("target1".to_string(), vec!["shared".to_string()]);
        reverse.insert("shared".to_string(), vec!["root".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);
        let targets = &["target0", "target1"];

        let multi = reverse_bfs_multi(&graph, targets, 5);
        let attributed = reverse_bfs_multi_attributed(&graph, targets, 5);

        for (name, &depth) in &multi {
            assert_eq!(
                attributed[name].0, depth,
                "depth mismatch for {name}: multi={depth}, attributed={}",
                attributed[name].0
            );
        }
        assert_eq!(multi.len(), attributed.len());
    }

    #[test]
    fn test_attributed_stale_queue_entry() {
        // Same graph as test_reverse_bfs_multi_stale_queue_entry
        // target1 <- mid <- deep, target2 <- deep, deep <- root
        let mut reverse = HashMap::new();
        reverse.insert("target1".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["deep".to_string()]);
        reverse.insert("target2".to_string(), vec!["deep".to_string()]);
        reverse.insert("deep".to_string(), vec!["root".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi_attributed(&graph, &["target1", "target2"], 5);

        assert_eq!(result["deep"].0, 1, "depth 1 from target2");
        assert_eq!(result["deep"].1, 1, "attributed to target2 (index 1)");
        assert_eq!(result["root"].0, 2, "root at depth 2, not 3");
        assert_eq!(result["root"].1, 1, "root attributed via target2's chain");
    }

    #[test]
    fn test_attributed_depth_limit() {
        // A -> B -> C -> target0, D -> target1 with depth limit 1
        let mut reverse = HashMap::new();
        reverse.insert("target0".to_string(), vec!["C".to_string()]);
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        reverse.insert("target1".to_string(), vec!["D".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let result = reverse_bfs_multi_attributed(&graph, &["target0", "target1"], 1);

        assert_eq!(result["C"], (1, 0));
        assert_eq!(result["D"], (1, 1));
        assert!(!result.contains_key("B"), "B beyond depth limit");
        assert!(!result.contains_key("A"), "A beyond depth limit");
    }

    // ===== test_reachability tests =====

    #[test]
    fn test_reachability_empty_graph() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let result = test_reachability(&graph, &["test_a"], 5);
        assert!(
            result.is_empty(),
            "No forward edges means nothing reachable"
        );
    }

    #[test]
    fn test_reachability_single_test() {
        // test_a -> B -> C
        let mut forward = HashMap::new();
        forward.insert("test_a".to_string(), vec!["B".to_string()]);
        forward.insert("B".to_string(), vec!["C".to_string()]);
        let graph = CallGraph::from_string_maps(forward, HashMap::new());
        let result = test_reachability(&graph, &["test_a"], 5);
        assert_eq!(result.get("B"), Some(&1), "B reachable from test_a");
        assert_eq!(result.get("C"), Some(&1), "C reachable from test_a");
        assert!(
            !result.contains_key("test_a"),
            "Test itself excluded (depth 0)"
        );
    }

    #[test]
    fn test_reachability_multiple_tests_shared_target() {
        // test_a -> target, test_b -> target
        let mut forward = HashMap::new();
        forward.insert("test_a".to_string(), vec!["target".to_string()]);
        forward.insert("test_b".to_string(), vec!["target".to_string()]);
        let graph = CallGraph::from_string_maps(forward, HashMap::new());
        let result = test_reachability(&graph, &["test_a", "test_b"], 5);
        assert_eq!(
            result.get("target"),
            Some(&2),
            "target reachable from both tests"
        );
    }

    #[test]
    fn test_reachability_respects_depth() {
        // test_a -> B -> C -> D
        let mut forward = HashMap::new();
        forward.insert("test_a".to_string(), vec!["B".to_string()]);
        forward.insert("B".to_string(), vec!["C".to_string()]);
        forward.insert("C".to_string(), vec!["D".to_string()]);
        let graph = CallGraph::from_string_maps(forward, HashMap::new());
        let result = test_reachability(&graph, &["test_a"], 2);
        assert_eq!(result.get("B"), Some(&1), "B at depth 1");
        assert_eq!(result.get("C"), Some(&1), "C at depth 2");
        assert!(!result.contains_key("D"), "D beyond depth limit");
    }
}
