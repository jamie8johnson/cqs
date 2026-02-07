//! Test map tool — find tests that exercise a function

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

use super::super::server::McpServer;
use super::resolve::resolve_target;

pub fn tool_test_map(server: &McpServer, arguments: Value) -> Result<Value> {
    let name = arguments
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: name"))?;
    let max_depth = arguments
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(5)
        .clamp(1, 20);

    let (chunk, _) = resolve_target(&server.store, name)?;
    let target_name = chunk.name.clone();

    // Load call graph and test chunks
    let graph = server.store.get_call_graph()?;
    let test_chunks = server.store.find_test_chunks()?;
    let _test_names: HashSet<String> = test_chunks.iter().map(|t| t.name.clone()).collect();

    // Reverse BFS from target — find all ancestors up to max_depth
    // Track predecessors for call chain reconstruction
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new(); // name -> (depth, predecessor)
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target_name.clone(), (0, String::new()));
    queue.push_back((target_name.clone(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), (depth + 1, current.clone()));
                    queue.push_back((caller.clone(), depth + 1));
                }
            }
        }
    }

    // Find tests that are ancestors of target
    let mut tests_json = Vec::new();
    for test in &test_chunks {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                // exclude target itself
                // Reconstruct call chain from test to target
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                while !current.is_empty() {
                    chain.push(current.clone());
                    if current == target_name {
                        break;
                    }
                    current = ancestors
                        .get(&current)
                        .map(|(_, pred)| pred.clone())
                        .unwrap_or_default();
                }

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
                    "call_depth": depth,
                    "call_chain": chain,
                }));
            }
        }
    }

    // Sort by depth then name
    tests_json.sort_by(|a, b| {
        let da = a.get("call_depth").and_then(|v| v.as_u64()).unwrap_or(0);
        let db = b.get("call_depth").and_then(|v| v.as_u64()).unwrap_or(0);
        da.cmp(&db).then_with(|| {
            let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            na.cmp(nb)
        })
    });

    let test_count = tests_json.len();
    let result = serde_json::json!({
        "function": chunk.name,
        "tests": tests_json,
        "test_count": test_count,
    });

    Ok(
        serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
    )
}
