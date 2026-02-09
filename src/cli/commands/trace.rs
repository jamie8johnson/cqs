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
    format: &str,
) -> Result<()> {
    let root = find_project_root();
    let index_path = cqs::resolve_index_dir(&root).join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    if !matches!(format, "text" | "json" | "mermaid") {
        bail!("Invalid format '{}'. Valid: text, json, mermaid", format);
    }

    let store = Store::open(&index_path)?;

    // Resolve source and target to chunk names
    let (source_chunk, _) = resolve_target(&store, source)?;
    let (target_chunk, _) = resolve_target(&store, target)?;

    let source_name = source_chunk.name.clone();
    let target_name = target_chunk.name.clone();

    // Trivial case: source == target
    if source_name == target_name {
        if format == "json" {
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
        } else if format == "mermaid" {
            let rel_file = source_chunk
                .file
                .strip_prefix(&root)
                .unwrap_or(&source_chunk.file)
                .to_string_lossy()
                .replace('\\', "/");
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
    let graph = store.get_call_graph()?;
    let path = bfs_shortest_path(&graph.forward, &source_name, &target_name, max_depth);

    match path {
        Some(names) => {
            if format == "json" {
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
            } else if format == "mermaid" {
                format_mermaid(&store, &root, &names)?;
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
            if format == "json" {
                let result = serde_json::json!({
                    "source": source_name,
                    "target": target_name,
                    "path": null,
                    "message": format!("No call path found within depth {}", max_depth)
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if format == "mermaid" {
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

    // Generate node definitions with labels
    for (i, name) in names.iter().enumerate() {
        let label = match store.search_by_name(name, 1)?.into_iter().next() {
            Some(r) => {
                let rel = r
                    .chunk
                    .file
                    .strip_prefix(root)
                    .unwrap_or(&r.chunk.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
    if i < 26 {
        ((b'A' + i as u8) as char).to_string()
    } else {
        format!("{}{}", ((b'A' + (i % 26) as u8) as char), i / 26)
    }
}

/// Escape characters that are special in Mermaid labels
fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
