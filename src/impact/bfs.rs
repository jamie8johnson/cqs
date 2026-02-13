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
}
