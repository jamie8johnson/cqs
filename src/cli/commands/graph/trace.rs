//! Trace command — find shortest call path between two functions

use std::collections::{HashMap, VecDeque};

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::Store;

use crate::cli::commands::resolve::resolve_target;
use crate::cli::OutputFormat;

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceHop {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file: String,
    pub line_start: u32, // was "line"
    #[serde(skip_serializing_if = "String::is_empty")]
    pub signature: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceOutput {
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<TraceHop>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub found: bool,
}

// ─── Shared JSON builder ───────────────────────────────────────────────────

/// Build typed trace output from BFS result.
///
/// Shared between CLI (`cmd_trace --json`) and batch (`dispatch_trace`).
/// Takes the BFS path (or None) and resolves chunk metadata via batch lookup.
pub(crate) fn build_trace_output(
    store: &Store,
    source_name: &str,
    target_name: &str,
    path: Option<&[String]>,
    root: &std::path::Path,
) -> Result<TraceOutput> {
    let _span = tracing::info_span!("build_trace_output", source_name, target_name).entered();

    match path {
        Some(names) => {
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let batch_results = store.search_by_names_batch(&name_refs, 1)?;

            let hops: Vec<TraceHop> = names
                .iter()
                .map(
                    |name| match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                        Some(r) => TraceHop {
                            name: name.clone(),
                            file: cqs::rel_display(&r.chunk.file, root).to_string(),
                            line_start: r.chunk.line_start,
                            signature: r.chunk.signature.clone(),
                        },
                        None => {
                            tracing::warn!(name, "Trace hop not found in index");
                            TraceHop {
                                name: name.clone(),
                                file: String::new(),
                                line_start: 0,
                                signature: String::new(),
                            }
                        }
                    },
                )
                .collect();

            Ok(TraceOutput {
                source: source_name.to_string(),
                target: target_name.to_string(),
                depth: Some(hops.len().saturating_sub(1)),
                path: Some(hops),
                found: true,
            })
        }
        None => Ok(TraceOutput {
            source: source_name.to_string(),
            target: target_name.to_string(),
            path: None,
            depth: None,
            found: false,
        }),
    }
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_trace(
    ctx: &crate::cli::CommandContext,
    source: &str,
    target: &str,
    max_depth: usize,
    format: &OutputFormat,
    cross_project: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_trace", source, target, cross_project).entered();

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let result = cqs::cross_project::trace_cross(&mut cross_ctx, source, target, max_depth)?;

        let trace_result = cqs::cross_project::CrossProjectTraceResult {
            source: source.to_string(),
            target: target.to_string(),
            depth: result.as_ref().map(|p| p.len().saturating_sub(1)),
            found: result.is_some(),
            path: result,
        };

        if matches!(format, OutputFormat::Json) {
            println!("{}", serde_json::to_string_pretty(&trace_result)?);
        } else if matches!(format, OutputFormat::Mermaid) {
            if let Some(ref path) = trace_result.path {
                println!("graph TD");
                for (i, hop) in path.iter().enumerate() {
                    let id = node_letter(i);
                    let label = if hop.project.is_empty() {
                        mermaid_escape(&hop.name)
                    } else {
                        format!(
                            "{} [{}]",
                            mermaid_escape(&hop.name),
                            mermaid_escape(&hop.project)
                        )
                    };
                    println!("    {}[\"{}\"]", id, label);
                }
                for i in 0..path.len().saturating_sub(1) {
                    println!("    {} --> {}", node_letter(i), node_letter(i + 1));
                }
            } else {
                println!("graph TD");
                println!(
                    "    %% No call path found from {} to {} within depth {}",
                    source, target, max_depth
                );
            }
        } else if let Some(ref path) = trace_result.path {
            println!(
                "Call path from {} to {} ({} hop{}, cross-project):",
                source.cyan(),
                target.cyan(),
                path.len().saturating_sub(1),
                if path.len().saturating_sub(1) == 1 {
                    ""
                } else {
                    "s"
                }
            );
            println!();
            for (i, hop) in path.iter().enumerate() {
                let prefix = if i == 0 {
                    "  ".to_string()
                } else {
                    "  \u{2192} ".to_string()
                };
                if hop.project.is_empty() {
                    println!("{}{}", prefix, hop.name.cyan());
                } else {
                    println!("{}{} [{}]", prefix, hop.name.cyan(), hop.project.dimmed());
                }
            }
        } else {
            println!(
                "No call path found from {} to {} within depth {} (cross-project).",
                source.cyan(),
                target.cyan(),
                max_depth
            );
        }
        return Ok(());
    }

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
            let trivial_path = vec![source_name.clone()];
            let result =
                build_trace_output(store, &source_name, &target_name, Some(&trivial_path), root)?;
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
                let result =
                    build_trace_output(store, &source_name, &target_name, Some(&names), root)?;
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
                let result = build_trace_output(store, &source_name, &target_name, None, root)?;
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

/// Default maximum nodes in trace BFS traversal.
const DEFAULT_TRACE_MAX_NODES: usize = 10_000;

/// Returns the trace BFS node cap, reading `CQS_TRACE_MAX_NODES` once on first call.
fn trace_max_nodes() -> usize {
    use std::sync::OnceLock;
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| match std::env::var("CQS_TRACE_MAX_NODES") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(
                    cap = n,
                    "Trace BFS node cap overridden via CQS_TRACE_MAX_NODES"
                );
                n
            }
            _ => {
                tracing::warn!(
                    val,
                    "CQS_TRACE_MAX_NODES invalid, using default {DEFAULT_TRACE_MAX_NODES}"
                );
                DEFAULT_TRACE_MAX_NODES
            }
        },
        Err(_) => DEFAULT_TRACE_MAX_NODES,
    })
}

/// BFS shortest path through forward adjacency list.
/// Capped at `CQS_TRACE_MAX_NODES` (default 10,000) visited nodes to prevent
/// OOM on dense graphs.
pub(crate) fn bfs_shortest_path(
    forward: &HashMap<std::sync::Arc<str>, Vec<std::sync::Arc<str>>>,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Option<Vec<String>> {
    let max_nodes = trace_max_nodes();
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
        if visited.len() >= max_nodes {
            tracing::warn!(max_nodes, "BFS trace capped — graph too dense");
            break;
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

    // ===== TraceOutput serialization tests =====

    #[test]
    fn test_trace_output_not_found() {
        let output = TraceOutput {
            source: "a".into(),
            target: "b".into(),
            path: None,
            depth: None,
            found: false,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["found"], false);
        assert!(json.get("path").is_none());
        assert!(json.get("depth").is_none());
    }

    #[test]
    fn test_trace_output_found() {
        let output = TraceOutput {
            source: "a".into(),
            target: "c".into(),
            path: Some(vec![
                TraceHop {
                    name: "a".into(),
                    file: "src/a.rs".into(),
                    line_start: 1,
                    signature: "fn a()".into(),
                },
                TraceHop {
                    name: "b".into(),
                    file: "src/b.rs".into(),
                    line_start: 10,
                    signature: "fn b()".into(),
                },
                TraceHop {
                    name: "c".into(),
                    file: "src/c.rs".into(),
                    line_start: 20,
                    signature: "fn c()".into(),
                },
            ]),
            depth: Some(2),
            found: true,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["found"], true);
        assert_eq!(json["depth"], 2);
        assert_eq!(json["path"][0]["line_start"], 1); // was "line"
        assert!(json["path"][0].get("line").is_none());
    }
}
