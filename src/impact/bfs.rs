//! BFS graph traversal for impact analysis

use std::collections::{HashMap, VecDeque};

use crate::store::CallGraph;

/// Reverse BFS from a target node, returning all ancestors with their depths
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
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), d + 1);
                    queue.push_back((caller.clone(), d + 1));
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
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                match ancestors.entry(caller.clone()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(d + 1);
                        queue.push_back((caller.clone(), d + 1));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        // Update if we found a shorter path
                        if d + 1 < *e.get() {
                            *e.get_mut() = d + 1;
                            queue.push_back((caller.clone(), d + 1));
                        }
                    }
                }
            }
        }
    }

    ancestors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_reverse_bfs_empty_graph() {
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        };
        let result = reverse_bfs(&graph, "target", 5);
        assert_eq!(result.len(), 1); // Just the target itself at depth 0
        assert_eq!(result["target"], 0);
    }

    #[test]
    fn test_reverse_bfs_chain() {
        let mut reverse = HashMap::new();
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        let result = reverse_bfs(&graph, "C", 1);
        assert_eq!(result.len(), 2); // C at 0, B at 1
        assert!(!result.contains_key("A")); // Beyond depth limit
    }

    // ===== reverse_bfs_multi tests =====

    #[test]
    fn test_reverse_bfs_multi_empty_targets() {
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        };
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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };

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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };

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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };

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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };

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
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };

        let result = reverse_bfs_multi(&graph, &["D", "B"], 5);

        assert_eq!(result["D"], 0);
        assert_eq!(result["B"], 0); // Target, so depth 0
        assert_eq!(result["C"], 1); // Caller of D
        assert_eq!(
            result["A"], 1,
            "A is at depth 1 from B (target), not depth 2 from D"
        );
    }
}
