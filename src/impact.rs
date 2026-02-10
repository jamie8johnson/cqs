//! Impact analysis core — shared between CLI and MCP
//!
//! Provides BFS caller traversal, test discovery, snippet extraction,
//! transitive caller analysis, and mermaid diagram generation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::store::{CallGraph, CallerWithContext};
use crate::Store;

/// Direct caller with display-ready fields
pub struct CallerInfo {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub call_line: u32,
    pub snippet: Option<String>,
}

/// Affected test with call depth
pub struct TestInfo {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub call_depth: usize,
}

/// Transitive caller at a given depth
pub struct TransitiveCaller {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub depth: usize,
}

/// Complete impact analysis result
pub struct ImpactResult {
    pub function_name: String,
    pub callers: Vec<CallerInfo>,
    pub tests: Vec<TestInfo>,
    pub transitive_callers: Vec<TransitiveCaller>,
}

/// Maximum depth for test search BFS
const MAX_TEST_SEARCH_DEPTH: usize = 5;

/// Run impact analysis: find callers, affected tests, and transitive callers.
pub fn analyze_impact(
    store: &Store,
    target_name: &str,
    depth: usize,
) -> anyhow::Result<ImpactResult> {
    let callers = build_caller_info(store, target_name)?;
    let graph = store.get_call_graph()?;
    let tests = find_affected_tests(store, &graph, target_name)?;
    let transitive_callers = if depth > 1 {
        find_transitive_callers(store, &graph, target_name, depth)?
    } else {
        Vec::new()
    };

    Ok(ImpactResult {
        function_name: target_name.to_string(),
        callers,
        tests,
        transitive_callers,
    })
}

/// Build caller info with call-site snippets
fn build_caller_info(store: &Store, target_name: &str) -> anyhow::Result<Vec<CallerInfo>> {
    let callers_ctx = store.get_callers_with_context(target_name)?;
    let mut callers = Vec::with_capacity(callers_ctx.len());

    for caller in &callers_ctx {
        let snippet = extract_call_snippet(store, caller);
        callers.push(CallerInfo {
            name: caller.name.clone(),
            file: caller.file.clone(),
            line: caller.line,
            call_line: caller.call_line,
            snippet,
        });
    }

    Ok(callers)
}

/// Extract a snippet around the call site from the caller's indexed content
fn extract_call_snippet(store: &Store, caller: &CallerWithContext) -> Option<String> {
    store
        .search_by_name(&caller.name, 1)
        .ok()
        .and_then(|r| r.into_iter().next())
        .and_then(|r| {
            let lines: Vec<&str> = r.chunk.content.lines().collect();
            let offset = caller.call_line.saturating_sub(r.chunk.line_start) as usize;
            if offset < lines.len() {
                let start = offset.saturating_sub(1);
                let end = (offset + 2).min(lines.len());
                Some(lines[start..end].join("\n"))
            } else {
                None
            }
        })
}

/// Find tests that transitively call the target via reverse BFS
fn find_affected_tests(
    store: &Store,
    graph: &CallGraph,
    target_name: &str,
) -> anyhow::Result<Vec<TestInfo>> {
    let test_chunks = store.find_test_chunks()?;
    let ancestors = reverse_bfs(graph, target_name, MAX_TEST_SEARCH_DEPTH);

    let mut tests: Vec<TestInfo> = test_chunks
        .iter()
        .filter_map(|test| {
            ancestors.get(&test.name).and_then(|&d| {
                if d > 0 {
                    Some(TestInfo {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        line: test.line_start,
                        call_depth: d,
                    })
                } else {
                    None
                }
            })
        })
        .collect();

    tests.sort_by_key(|t| t.call_depth);
    Ok(tests)
}

/// Find transitive callers up to the given depth
fn find_transitive_callers(
    store: &Store,
    graph: &CallGraph,
    target_name: &str,
    depth: usize,
) -> anyhow::Result<Vec<TransitiveCaller>> {
    let mut result = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(target_name.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((target_name.to_string(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller_name in callers {
                if visited.insert(caller_name.clone()) {
                    if let Some(r) = store
                        .search_by_name(caller_name, 1)
                        .ok()
                        .and_then(|r| r.into_iter().next())
                    {
                        result.push(TransitiveCaller {
                            name: caller_name.clone(),
                            file: r.chunk.file,
                            line: r.chunk.line_start,
                            depth: d + 1,
                        });
                    }
                    queue.push_back((caller_name.clone(), d + 1));
                }
            }
        }
    }

    Ok(result)
}

/// Reverse BFS from a target node, returning all ancestors with their depths
fn reverse_bfs(graph: &CallGraph, target: &str, max_depth: usize) -> HashMap<String, usize> {
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

// ============ JSON Serialization ============

/// Serialize impact result to JSON, relativizing paths against the project root
pub fn impact_to_json(result: &ImpactResult, root: &Path) -> serde_json::Value {
    let callers_json: Vec<_> = result
        .callers
        .iter()
        .map(|c| {
            let rel = rel_path(&c.file, root);
            serde_json::json!({
                "name": c.name,
                "file": rel,
                "line": c.line,
                "call_line": c.call_line,
                "snippet": c.snippet,
            })
        })
        .collect();

    let tests_json: Vec<_> = result
        .tests
        .iter()
        .map(|t| {
            let rel = rel_path(&t.file, root);
            serde_json::json!({
                "name": t.name,
                "file": rel,
                "line": t.line,
                "call_depth": t.call_depth,
            })
        })
        .collect();

    let mut output = serde_json::json!({
        "function": result.function_name,
        "callers": callers_json,
        "tests": tests_json,
        "caller_count": callers_json.len(),
        "test_count": tests_json.len(),
    });

    if !result.transitive_callers.is_empty() {
        let trans_json: Vec<_> = result
            .transitive_callers
            .iter()
            .map(|c| {
                let rel = rel_path(&c.file, root);
                serde_json::json!({
                    "name": c.name,
                    "file": rel,
                    "line": c.line,
                    "depth": c.depth,
                })
            })
            .collect();
        output
            .as_object_mut()
            .unwrap()
            .insert("transitive_callers".into(), serde_json::json!(trans_json));
    }

    output
}

// ============ Mermaid Diagram ============

/// Generate a mermaid diagram from impact result
pub fn impact_to_mermaid(result: &ImpactResult, root: &Path) -> String {
    let mut lines = vec!["graph TD".to_string()];
    lines.push(format!(
        "    A[\"{}\"]\n    style A fill:#f96",
        mermaid_escape(&result.function_name)
    ));

    let mut idx = 1;
    for c in &result.callers {
        let rel = rel_path(&c.file, root);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}[\"{} ({}:{})\"]",
            letter,
            mermaid_escape(&c.name),
            mermaid_escape(&rel),
            c.line
        ));
        lines.push(format!("    {} --> A", letter));
        idx += 1;
    }

    for t in &result.tests {
        let rel = rel_path(&t.file, root);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}{{\"{}\\n{}\\ndepth: {}\"}}",
            letter,
            mermaid_escape(&t.name),
            mermaid_escape(&rel),
            t.call_depth
        ));
        lines.push(format!("    {} -.-> A", letter));
        idx += 1;
    }

    lines.join("\n")
}

// ============ Helpers ============

fn rel_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
