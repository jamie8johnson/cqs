//! Shared test-map algorithm: reverse BFS from a target function to find
//! which test functions can reach it through the call graph.
//!
//! Used by both `cmd_test_map` (CLI) and `dispatch_test_map` (batch handler).

use std::collections::{HashMap, VecDeque};

use crate::store::{CallGraph, ChunkSummary};

/// A test function that reaches the target through the call graph.
#[derive(Debug, Clone)]
pub struct TestMatch {
    /// Test function name
    pub name: String,
    /// Source file (relative display path)
    pub file: String,
    /// Line number of the test function
    pub line: u32,
    /// Call depth from the test to the target
    pub depth: usize,
    /// Call chain from the test down to the target
    pub chain: Vec<String>,
}

/// Find test functions that can reach `target_name` through the call graph
/// via reverse BFS, up to `max_depth` hops.
///
/// Returns matches sorted by depth (ascending), then name (alphabetical).
/// Only returns tests at depth > 0 (direct matches at depth 0 are the target itself).
pub fn find_test_matches(
    graph: &CallGraph,
    test_chunks: &[ChunkSummary],
    target_name: &str,
    max_depth: usize,
    rel_display: impl Fn(&ChunkSummary) -> String,
) -> Vec<TestMatch> {
    let _span = tracing::info_span!("find_test_matches", target = target_name).entered();

    // Reverse BFS from target, tracking parent for chain reconstruction
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target_name.to_string(), (0, String::new()));
    queue.push_back((target_name.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(current.as_str()) {
            for caller in callers {
                if !ancestors.contains_key(caller.as_ref()) {
                    ancestors.insert(caller.to_string(), (depth + 1, current.clone()));
                    queue.push_back((caller.to_string(), depth + 1));
                }
            }
        }
    }

    // Collect matching tests and reconstruct call chains
    let chain_limit = max_depth + 1;
    let mut matches: Vec<TestMatch> = Vec::new();
    for test in test_chunks {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                while !current.is_empty() && chain.len() < chain_limit {
                    chain.push(current.clone());
                    if current == target_name {
                        break;
                    }
                    current = match ancestors.get(&current) {
                        Some((_, p)) if !p.is_empty() => p.clone(),
                        _ => {
                            tracing::debug!(node = %current, "Chain walk hit dead end");
                            break;
                        }
                    };
                }
                matches.push(TestMatch {
                    name: test.name.clone(),
                    file: rel_display(test),
                    line: test.line_start,
                    depth: *depth,
                    chain,
                });
            }
        }
    }

    matches.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ChunkType, Language};
    use std::path::PathBuf;

    fn make_test_chunk(name: &str) -> ChunkSummary {
        ChunkSummary {
            id: name.to_string(),
            file: PathBuf::from(format!("tests/{name}.rs")),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 10,
            content_hash: String::new(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
        }
    }

    #[test]
    fn test_find_test_matches_simple_chain() {
        // test_foo -> mid -> target
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["test_foo".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let test_chunks = vec![make_test_chunk("test_foo")];
        let matches = find_test_matches(&graph, &test_chunks, "target", 5, |c| {
            c.file.display().to_string()
        });

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "test_foo");
        assert_eq!(matches[0].depth, 2);
        assert_eq!(matches[0].chain, vec!["test_foo", "mid", "target"]);
    }

    #[test]
    fn test_find_test_matches_depth_limit() {
        // test_foo -> mid -> target, but max_depth = 1
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["test_foo".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let test_chunks = vec![make_test_chunk("test_foo")];
        let matches = find_test_matches(&graph, &test_chunks, "target", 1, |c| {
            c.file.display().to_string()
        });

        assert!(
            matches.is_empty(),
            "test_foo at depth 2 exceeds max_depth 1"
        );
    }

    #[test]
    fn test_find_test_matches_no_tests() {
        let graph = CallGraph::from_string_maps(HashMap::new(), HashMap::new());
        let matches = find_test_matches(&graph, &[], "target", 5, |c| c.file.display().to_string());
        assert!(matches.is_empty());
    }
}
