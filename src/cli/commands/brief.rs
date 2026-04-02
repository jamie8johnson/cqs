//! Brief command — one-line-per-function summary for a file

use std::collections::HashMap;

use anyhow::{bail, Context as _, Result};

use cqs::rel_display;
use cqs::store::{ChunkSummary, Store};

/// One-line summary entry for a function in the file.
#[derive(serde::Serialize)]
struct BriefEntry {
    name: String,
    chunk_type: String,
    line_start: u32,
    callers: u64,
    tests: u64,
}

/// Data returned by `build_brief_data`: chunks with caller and test counts.
#[derive(Debug)]
struct BriefData {
    chunks: Vec<ChunkSummary>,
    caller_counts: HashMap<String, u64>,
    test_counts: HashMap<String, u64>,
}

/// Build brief data for a file: load chunks, count callers and test coverage.
fn build_brief_data(store: &Store, path: &str) -> Result<BriefData> {
    let _span = tracing::info_span!("build_brief_data", path).entered();

    let chunks = store
        .get_chunks_by_origin(path)
        .context("Failed to load chunks for file")?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    // Dedup by name — multiple window_idx values produce duplicate entries
    let mut seen = std::collections::HashSet::new();
    let chunks: Vec<ChunkSummary> = chunks
        .into_iter()
        .filter(|c| seen.insert(c.name.clone()))
        .collect();

    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();

    let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to fetch caller counts");
        HashMap::new()
    });

    // Get test coverage via call graph BFS (same approach as test_map)
    let graph = store.get_call_graph().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to load call graph for test counts");
        std::sync::Arc::new(cqs::store::CallGraph::from_string_maps(
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        ))
    });
    let test_chunks = store.find_test_chunks().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to find test chunks");
        std::sync::Arc::new(Vec::new())
    });

    let mut test_counts: HashMap<String, u64> = HashMap::new();
    for chunk in &chunks {
        // Reverse BFS from chunk to find test ancestors (depth 5)
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        visited.insert(chunk.name.clone());
        queue.push_back((chunk.name.clone(), 0usize));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= 5 {
                continue;
            }
            if let Some(callers) = graph.reverse.get(current.as_str()) {
                for caller in callers {
                    if visited.insert(caller.to_string()) {
                        queue.push_back((caller.to_string(), depth + 1));
                    }
                }
            }
        }

        let count = test_chunks
            .iter()
            .filter(|t| visited.contains(&t.name) && t.name != chunk.name)
            .count() as u64;
        test_counts.insert(chunk.name.clone(), count);
    }

    Ok(BriefData {
        chunks,
        caller_counts,
        test_counts,
    })
}

pub(crate) fn cmd_brief(ctx: &crate::cli::CommandContext, path: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_brief", path).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    let data = build_brief_data(store, path)?;
    let rel = rel_display(&std::path::PathBuf::from(path), root);

    let entries: Vec<BriefEntry> = data
        .chunks
        .iter()
        .map(|c| BriefEntry {
            name: c.name.clone(),
            chunk_type: c.chunk_type.to_string(),
            line_start: c.line_start,
            callers: *data.caller_counts.get(&c.name).unwrap_or(&0),
            tests: *data.test_counts.get(&c.name).unwrap_or(&0),
        })
        .collect();

    if json {
        let result = serde_json::json!({
            "file": rel,
            "functions": entries,
            "total": entries.len(),
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        use colored::Colorize;
        println!("{} ({})", rel.bold(), entries.len());
        println!(
            "  {:<30} {:<12} {:>7} {:>7}",
            "Name", "Type", "Callers", "Tests"
        );
        println!("  {}", "-".repeat(60));
        for e in &entries {
            println!(
                "  {:<30} {:<12} {:>7} {:>7}",
                e.name, e.chunk_type, e.callers, e.tests
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_entry_serializes_correctly() {
        let entry = BriefEntry {
            name: "my_func".to_string(),
            chunk_type: "Function".to_string(),
            line_start: 10,
            callers: 3,
            tests: 1,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["name"], "my_func");
        assert_eq!(json["chunk_type"], "Function");
        assert_eq!(json["line_start"], 10);
        assert_eq!(json["callers"], 3);
        assert_eq!(json["tests"], 1);
    }

    #[test]
    fn brief_json_output_shape() {
        let entries = vec![
            BriefEntry {
                name: "foo".to_string(),
                chunk_type: "Function".to_string(),
                line_start: 1,
                callers: 2,
                tests: 0,
            },
            BriefEntry {
                name: "bar".to_string(),
                chunk_type: "Method".to_string(),
                line_start: 10,
                callers: 0,
                tests: 1,
            },
        ];
        let result = serde_json::json!({
            "file": "src/lib.rs",
            "functions": entries,
            "total": entries.len(),
        });
        assert_eq!(result["total"], 2);
        assert_eq!(result["functions"][0]["name"], "foo");
        assert_eq!(result["functions"][1]["callers"], 0);
    }
}
