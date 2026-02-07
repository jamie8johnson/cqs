//! Trace command — find shortest call path between two functions

use std::collections::{HashMap, VecDeque};

use anyhow::{bail, Result};
use colored::Colorize;

use cqs::Store;

use crate::cli::find_project_root;

use super::resolve::resolve_target;

pub(crate) fn cmd_trace(
    _cli: &crate::cli::Cli,
    source: &str,
    target: &str,
    max_depth: usize,
    json: bool,
) -> Result<()> {
    let root = find_project_root();
    let index_path = root.join(".cq/index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

    // Resolve source and target to chunk names
    let (source_chunk, _) = resolve_target(&store, source)?;
    let (target_chunk, _) = resolve_target(&store, target)?;

    let source_name = source_chunk.name.clone();
    let target_name = target_chunk.name.clone();

    // Trivial case: source == target
    if source_name == target_name {
        if json {
            let rel_file = source_chunk
                .file
                .strip_prefix(&root)
                .unwrap_or(&source_chunk.file)
                .to_string_lossy()
                .replace('\\', "/");
            let result = serde_json::json!({
                "source": source_name,
                "target": target_name,
                "path": [{"name": source_name, "file": rel_file, "line": source_chunk.line_start, "signature": source_chunk.signature}],
                "depth": 0
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("{} and {} are the same function.", source_name, target_name);
        }
        return Ok(());
    }

    // Load call graph and BFS
    let graph = store.get_call_graph()?;
    let path = bfs_shortest_path(&graph.forward, &source_name, &target_name, max_depth);

    match path {
        Some(names) => {
            if json {
                let mut path_json = Vec::new();
                for name in &names {
                    let entry = match store.search_by_name(name, 1)?.into_iter().next() {
                        Some(r) => {
                            let rel = r
                                .chunk
                                .file
                                .strip_prefix(&root)
                                .unwrap_or(&r.chunk.file)
                                .to_string_lossy()
                                .replace('\\', "/");
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
            } else {
                println!(
                    "Call path from {} to {} ({} hop{}):",
                    source_name.cyan(),
                    target_name.cyan(),
                    names.len() - 1,
                    if names.len() - 1 == 1 { "" } else { "s" }
                );
                println!();
                for (i, name) in names.iter().enumerate() {
                    let prefix = if i == 0 {
                        "  ".to_string()
                    } else {
                        "  \u{2192} ".to_string()
                    };
                    match store.search_by_name(name, 1)?.into_iter().next() {
                        Some(r) => {
                            let rel = r.chunk.file.strip_prefix(&root).unwrap_or(&r.chunk.file);
                            println!(
                                "{}{} ({}:{})",
                                prefix,
                                name.cyan(),
                                rel.display(),
                                r.chunk.line_start
                            );
                        }
                        None => {
                            println!("{}{}", prefix, name.cyan());
                        }
                    }
                }
            }
        }
        None => {
            if json {
                let result = serde_json::json!({
                    "source": source_name,
                    "target": target_name,
                    "path": null,
                    "message": format!("No call path found within depth {}", max_depth)
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
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

/// BFS shortest path through forward adjacency list
fn bfs_shortest_path(
    forward: &HashMap<String, Vec<String>>,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Option<Vec<String>> {
    let mut visited: HashMap<String, String> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(source.to_string(), String::new());
    queue.push_back((source.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
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

        if let Some(callees) = forward.get(&current) {
            for callee in callees {
                if !visited.contains_key(callee) {
                    visited.insert(callee.clone(), current.clone());
                    queue.push_back((callee.clone(), depth + 1));
                }
            }
        }
    }
    None
}
