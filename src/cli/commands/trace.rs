//! Trace command — find shortest call path between two functions

use std::collections::{HashMap, VecDeque};

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::Store;

use super::resolve::resolve_target;
use crate::cli::OutputFormat;

pub(crate) fn cmd_trace(
    ctx: &crate::cli::CommandContext,
    source: &str,
    target: &str,
    max_depth: usize,
    format: &OutputFormat,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_trace", source, target).entered();

    let store = &ctx.store;
    let root = &ctx.root;

    // Resolve source and target to chunk names
    let source_resolved = resolve_target(store, source)?;
    let source_chunk = source_resolved.chunk;
    let target_resolved = resolve_target(store, target)?;
    let target_chunk = target_resolved.chunk;

    let source_name = source_chunk.name.clone();
    let target_name = target_chunk.name.clone();

    // Trivial case: source == target
    if source_name == target_name {
        if matches!(format, OutputFormat::Json) {
            let rel_file = cqs::rel_display(&source_chunk.file, root);
            let result = serde_json::json!({
                "source": source_name,
                "target": target_name,
                "path": [{"name": source_name, "file": rel_file, "line": source_chunk.line_start, "signature": source_chunk.signature}],
                "depth": 0
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else if matches!(format, OutputFormat::Mermaid) {
            let rel_file = cqs::rel_display(&source_chunk.file, root);
            println!("graph TD");
            println!(
                "    A[\"{} ({}:{})\"]",
                mermaid_escape(&source_name),
                mermaid_escape(&rel_file),
                source_chunk.line_start
            );
        } else {
            println!("{} and {} are the same function.", source_name, target_name);
        }
        return Ok(());
    }

    // Load call graph and BFS
    let graph = store
        .get_call_graph()
        .context("Failed to load call graph")?;
    let path = bfs_shortest_path(&graph.forward, &source_name, &target_name, max_depth);

    match path {
        Some(names) => {
            if matches!(format, OutputFormat::Json) {
                // CQ-5: Batch lookup instead of N individual search_by_name calls
                let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                let batch_results = store.search_by_names_batch(&name_refs, 1)?;

                let mut path_json = Vec::new();
                for name in &names {
                    let entry = match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                        Some(r) => {
                            let rel = cqs::rel_display(&r.chunk.file, root);
                            serde_json::json!({
                                "name": name,
                                "file": rel,
                                "line": r.chunk.line_start,
                                "signature": r.chunk.signature
                            })
                        }
                        None => serde_json::json!({"name": name}),
                    };
                    path_json.push(entry);
                }

                let result = serde_json::json!({
                    "source": source_name,
                    "target": target_name,
                    "path": path_json,
                    "depth": names.len() - 1
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if matches!(format, OutputFormat::Mermaid) {
                format_mermaid(store, root, &names)?;
            } else {
                println!(
                    "Call path from {} to {} ({} hop{}):",
                    source_name.cyan(),
                    target_name.cyan(),
                    names.len() - 1,
                    if names.len() - 1 == 1 { "" } else { "s" }
                );
                println!();

                // CQ-5: Batch lookup instead of N individual search_by_name calls
                let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                let batch_results = store.search_by_names_batch(&name_refs, 1)?;

                for (i, name) in names.iter().enumerate() {
                    let prefix = if i == 0 {
                        "  ".to_string()
                    } else {
                        "  \u{2192} ".to_string()
                    };
                    match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                        Some(r) => {
                            let rel = cqs::rel_display(&r.chunk.file, root);
                            println!("{}{} ({}:{})", prefix, name.cyan(), rel, r.chunk.line_start);
                        }
                        None => {
                            println!("{}{}", prefix, name.cyan());
                        }
                    }
                }
            }
        }
        None => {
            if matches!(format, OutputFormat::Json) {
                let result = serde_json::json!({
                    "source": source_name,
                    "target": target_name,
                    "path": null,
                    "message": format!("No call path found within depth {}", max_depth)
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if matches!(format, OutputFormat::Mermaid) {
                // Empty graph with comment
                println!("graph TD");
                println!(
                    "    %% No call path found from {} to {} within depth {}",
                    source_name, target_name, max_depth
                );
            } else {
                println!(
                    "No call path found from {} to {} within depth {}.",
                    source_name.cyan(),
                    target_name.cyan(),
                    max_depth
                );
            }
        }
    }

    Ok(())
}

/// Format trace path as Mermaid graph TD diagram
fn format_mermaid(store: &Store, root: &std::path::Path, names: &[String]) -> Result<()> {
    println!("graph TD");

    // CQ-5: Batch lookup instead of N individual search_by_name calls
    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let batch_results = store.search_by_names_batch(&name_refs, 1)?;

    // Generate node definitions with labels
    for (i, name) in names.iter().enumerate() {
        let label = match batch_results.get(name.as_str()).and_then(|v| v.first()) {
            Some(r) => {
                let rel = cqs::rel_display(&r.chunk.file, root);
                format!(
                    "{} ({}:{})",
                    mermaid_escape(name),
                    mermaid_escape(&rel),
                    r.chunk.line_start
                )
            }
            None => mermaid_escape(name),
        };
        let node_id = node_letter(i);
        println!("    {}[\"{}\"]", node_id, label);
    }

    // Generate edges
    for i in 0..names.len().saturating_sub(1) {
        println!("    {} --> {}", node_letter(i), node_letter(i + 1));
    }

    Ok(())
}

/// Generate mermaid node ID from index (A, B, C, ..., Z, A1, B1, ...)
fn node_letter(i: usize) -> String {
    let letter = (b'A' + (i % 26) as u8) as char;
    if i < 26 {
        letter.to_string()
    } else {
        format!("{}{}", letter, i / 26)
    }
}

/// Escape characters that are special in Mermaid labels
fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// BFS shortest path through forward adjacency list.
/// Capped at 10,000 visited nodes to prevent OOM on dense graphs.
pub(crate) fn bfs_shortest_path(
    forward: &HashMap<std::sync::Arc<str>, Vec<std::sync::Arc<str>>>,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Option<Vec<String>> {
    const MAX_NODES: usize = 10_000;
    let mut visited: HashMap<String, String> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(source.to_string(), String::new());
    queue.push_back((source.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if visited.len() >= MAX_NODES {
            tracing::warn!(max_nodes = MAX_NODES, "BFS trace capped — graph too dense");
            break;
        }
        if current == target {
            let mut path = vec![current.clone()];
            let mut node = &current;
            while let Some(pred) = visited.get(node) {
                if pred.is_empty() {
                    break;
                }
                path.push(pred.clone());
                node = pred;
            }
            path.reverse();
            return Some(path);
        }
        if depth >= max_depth {
            continue;
        }

        if let Some(callees) = forward.get(current.as_str()) {
            for callee in callees {
                if !visited.contains_key(callee.as_ref()) {
                    visited.insert(callee.to_string(), current.clone());
                    queue.push_back((callee.to_string(), depth + 1));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Convert a `HashMap<String, Vec<String>>` to `HashMap<Arc<str>, Vec<Arc<str>>>` for tests.
    fn arc_map(m: HashMap<String, Vec<String>>) -> HashMap<Arc<str>, Vec<Arc<str>>> {
        m.into_iter()
            .map(|(k, vs)| {
                let k: Arc<str> = Arc::from(k.as_str());
                let vs: Vec<Arc<str>> = vs.into_iter().map(|v| Arc::from(v.as_str())).collect();
                (k, vs)
            })
            .collect()
    }

    // ===== node_letter tests (P3-17) =====

    #[test]
    fn test_node_letter_a_to_z() {
        assert_eq!(node_letter(0), "A");
        assert_eq!(node_letter(1), "B");
        assert_eq!(node_letter(25), "Z");
    }

    #[test]
    fn test_node_letter_beyond_z() {
        // After Z: A1, B1, ...
        assert_eq!(node_letter(26), "A1");
        assert_eq!(node_letter(27), "B1");
        assert_eq!(node_letter(51), "Z1");
        assert_eq!(node_letter(52), "A2");
    }

    // ===== mermaid_escape tests (P3-17) =====

    #[test]
    fn test_mermaid_escape_quotes() {
        assert_eq!(mermaid_escape("hello \"world\""), "hello &quot;world&quot;");
    }

    #[test]
    fn test_mermaid_escape_angle_brackets() {
        assert_eq!(mermaid_escape("Vec<T>"), "Vec&lt;T&gt;");
    }

    #[test]
    fn test_mermaid_escape_plain() {
        assert_eq!(mermaid_escape("simple_name"), "simple_name");
    }

    // ===== bfs_shortest_path tests =====

    #[test]
    fn test_bfs_direct_path() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "B", 10);
        assert!(result.is_some());
        let path = result.unwrap();
        assert_eq!(path, vec!["A", "B"]);
    }

    #[test]
    fn test_bfs_no_path() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "C", 10);
        assert!(result.is_none(), "No path from A to C");
    }

    #[test]
    fn test_bfs_respects_max_depth() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        forward.insert("B".to_string(), vec!["C".to_string()]);
        forward.insert("C".to_string(), vec!["D".to_string()]);
        let forward = arc_map(forward);
        // Path A->B->C->D exists but depth=2 should not reach D
        let result = bfs_shortest_path(&forward, "A", "D", 2);
        assert!(result.is_none(), "Should not find path beyond max_depth=2");
    }

    #[test]
    fn test_bfs_multi_hop() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        forward.insert("B".to_string(), vec!["C".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "C", 10);
        assert!(result.is_some());
        let path = result.unwrap();
        assert_eq!(path, vec!["A", "B", "C"]);
    }
}
