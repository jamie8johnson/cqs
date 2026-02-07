//! Trace tool — find shortest call path between two functions

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

use super::super::server::McpServer;
use super::resolve::resolve_target;

pub fn tool_trace(server: &McpServer, arguments: Value) -> Result<Value> {
    let source = arguments
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: source"))?;
    let target = arguments
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: target"))?;
    let max_depth = arguments
        .get("max_depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(10)
        .clamp(1, 50);
    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    // Resolve source and target to chunk names
    let (source_chunk, _) = resolve_target(&server.store, source)?;
    let (target_chunk, _) = resolve_target(&server.store, target)?;

    let source_name = source_chunk.name.clone();
    let target_name = target_chunk.name.clone();

    // Trivial case: source == target
    if source_name == target_name {
        let rel_file = source_chunk
            .file
            .strip_prefix(&server.project_root)
            .unwrap_or(&source_chunk.file)
            .to_string_lossy()
            .replace('\\', "/");

        if format == "mermaid" {
            let text = format!(
                "graph TD\n    A[\"{} ({}:{})\"]",
                mermaid_escape(&source_name),
                mermaid_escape(&rel_file),
                source_chunk.line_start
            );
            return Ok(serde_json::json!({"content": [{"type": "text", "text": text}]}));
        }

        let result = serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": [{"name": source_name, "file": rel_file, "line": source_chunk.line_start, "signature": source_chunk.signature}],
            "depth": 0
        });
        return Ok(
            serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
        );
    }

    // Load call graph and BFS
    let graph = server.store.get_call_graph()?;
    let path = bfs_shortest_path(&graph.forward, &source_name, &target_name, max_depth);

    match path {
        Some(names) => {
            if format == "mermaid" {
                let text = format_mermaid(&server.store, &server.project_root, &names)?;
                return Ok(serde_json::json!({"content": [{"type": "text", "text": text}]}));
            }

            // Enrich each node with file/line/signature
            let mut path_json = Vec::new();
            for name in &names {
                let entry = match server.store.search_by_name(name, 1)?.into_iter().next() {
                    Some(r) => {
                        let rel = r
                            .chunk
                            .file
                            .strip_prefix(&server.project_root)
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
            Ok(
                serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
            )
        }
        None => {
            if format == "mermaid" {
                let text = format!(
                    "graph TD\n    %% No call path found from {} to {} within depth {}",
                    source_name, target_name, max_depth
                );
                return Ok(serde_json::json!({"content": [{"type": "text", "text": text}]}));
            }

            let result = serde_json::json!({
                "source": source_name,
                "target": target_name,
                "path": null,
                "message": format!("No call path found within depth {}", max_depth)
            });
            Ok(
                serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
            )
        }
    }
}

/// BFS shortest path through forward adjacency list
fn bfs_shortest_path(
    forward: &HashMap<String, Vec<String>>,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Option<Vec<String>> {
    let mut visited: HashMap<String, String> = HashMap::new(); // node -> predecessor
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(source.to_string(), String::new()); // empty = start
    queue.push_back((source.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if current == target {
            // Reconstruct path
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

/// Format trace path as Mermaid graph TD string
fn format_mermaid(
    store: &crate::Store,
    project_root: &std::path::Path,
    names: &[String],
) -> anyhow::Result<String> {
    let mut lines = vec!["graph TD".to_string()];

    for (i, name) in names.iter().enumerate() {
        let label = match store.search_by_name(name, 1)?.into_iter().next() {
            Some(r) => {
                let rel = r
                    .chunk
                    .file
                    .strip_prefix(project_root)
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
        lines.push(format!("    {}[\"{}\"]", node_letter(i), label));
    }

    for i in 0..names.len().saturating_sub(1) {
        lines.push(format!("    {} --> {}", node_letter(i), node_letter(i + 1)));
    }

    Ok(lines.join("\n"))
}

fn node_letter(i: usize) -> String {
    if i < 26 {
        ((b'A' + i as u8) as char).to_string()
    } else {
        format!("{}{}", ((b'A' + (i % 26) as u8) as char), i / 26)
    }
}

fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
