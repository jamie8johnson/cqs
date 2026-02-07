//! Impact tool — what breaks if you change a function

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

use super::super::server::McpServer;
use super::resolve::resolve_target;

pub fn tool_impact(server: &McpServer, arguments: Value) -> Result<Value> {
    let name = arguments
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: name"))?;
    let depth = arguments
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1)
        .clamp(1, 10);
    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    let (chunk, _) = resolve_target(&server.store, name)?;
    let target_name = chunk.name.clone();

    // Get callers with call-site context
    let callers_ctx = server.store.get_callers_with_context(&target_name)?;

    // Build caller JSON with snippets
    let mut callers_json = Vec::new();
    for caller in &callers_ctx {
        let rel_file = caller
            .file
            .strip_prefix(&server.project_root)
            .unwrap_or(&caller.file)
            .to_string_lossy()
            .replace('\\', "/");

        // Try to extract snippet from caller's indexed content
        let snippet = server
            .store
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
            });

        callers_json.push(serde_json::json!({
            "name": caller.name,
            "file": rel_file,
            "line": caller.line,
            "call_line": caller.call_line,
            "snippet": snippet,
        }));
    }

    // Find tests via reverse BFS
    let graph = server.store.get_call_graph()?;
    let test_chunks = server.store.find_test_chunks()?;

    // Reverse BFS from target to find all ancestors
    let mut ancestors: HashMap<String, usize> = HashMap::new(); // name -> depth
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target_name.clone(), 0);
    queue.push_back((target_name.clone(), 0));

    while let Some((current, d)) = queue.pop_front() {
        const MAX_TEST_SEARCH_DEPTH: usize = 5;
        if d >= MAX_TEST_SEARCH_DEPTH {
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

    // Intersect ancestors with test set
    let mut tests_json = Vec::new();
    for test in &test_chunks {
        if let Some(&d) = ancestors.get(&test.name) {
            if d > 0 {
                // exclude the target itself if it happens to be a test
                let rel_file = test
                    .file
                    .strip_prefix(&server.project_root)
                    .unwrap_or(&test.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                tests_json.push(serde_json::json!({
                    "name": test.name,
                    "file": rel_file,
                    "line": test.line_start,
                    "call_depth": d,
                }));
            }
        }
    }

    // Sort tests by depth
    tests_json.sort_by_key(|t| t.get("call_depth").and_then(|v| v.as_u64()).unwrap_or(0));

    // Mermaid output (same for any depth)
    if format == "mermaid" {
        let text = format_impact_mermaid(&chunk.name, &callers_json, &tests_json);
        return Ok(serde_json::json!({"content": [{"type": "text", "text": text}]}));
    }

    // For depth > 1, also include transitive callers
    if depth > 1 {
        let mut transitive_callers: Vec<Value> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(target_name.clone());
        let mut q: VecDeque<(String, usize)> = VecDeque::new();
        q.push_back((target_name.clone(), 0));

        while let Some((current, d)) = q.pop_front() {
            if d >= depth {
                continue;
            }
            if let Some(callers) = graph.reverse.get(&current) {
                for caller_name in callers {
                    if visited.insert(caller_name.clone()) {
                        // Look up file info
                        if let Some(r) = server
                            .store
                            .search_by_name(caller_name, 1)
                            .ok()
                            .and_then(|r| r.into_iter().next())
                        {
                            let rel = r
                                .chunk
                                .file
                                .strip_prefix(&server.project_root)
                                .unwrap_or(&r.chunk.file)
                                .to_string_lossy()
                                .replace('\\', "/");
                            transitive_callers.push(serde_json::json!({
                                "name": caller_name,
                                "file": rel,
                                "line": r.chunk.line_start,
                                "depth": d + 1,
                            }));
                        }
                        q.push_back((caller_name.clone(), d + 1));
                    }
                }
            }
        }

        let result = serde_json::json!({
            "function": chunk.name,
            "callers": callers_json,
            "transitive_callers": transitive_callers,
            "tests": tests_json,
            "caller_count": callers_json.len(),
            "test_count": tests_json.len(),
        });
        return Ok(
            serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
        );
    }

    let result = serde_json::json!({
        "function": chunk.name,
        "callers": callers_json,
        "tests": tests_json,
        "caller_count": callers_json.len(),
        "test_count": tests_json.len(),
    });
    Ok(
        serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
    )
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

fn format_impact_mermaid(
    target_name: &str,
    callers_json: &[Value],
    tests_json: &[Value],
) -> String {
    let mut lines = vec!["graph TD".to_string()];

    // Target node (index 0)
    lines.push(format!(
        "    A[\"{}\"]\n    style A fill:#f96",
        mermaid_escape(target_name)
    ));

    // Caller nodes
    let mut idx = 1;
    for caller in callers_json {
        let name = caller.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let file = caller.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let line = caller.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}[\"{} ({}:{})\"]",
            letter,
            mermaid_escape(name),
            mermaid_escape(file),
            line
        ));
        lines.push(format!("    {} --> A", letter));
        idx += 1;
    }

    // Test nodes (different shape)
    for test in tests_json {
        let name = test.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let file = test.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let depth = test.get("call_depth").and_then(|v| v.as_u64()).unwrap_or(0);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}{{\"{}\\n{}\\ndepth: {}\"}}",
            letter,
            mermaid_escape(name),
            mermaid_escape(file),
            depth
        ));
        lines.push(format!("    {} -.-> A", letter));
        idx += 1;
    }

    lines.join("\n")
}
