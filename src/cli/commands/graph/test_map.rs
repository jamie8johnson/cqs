//! Test map command — find tests that exercise a function
//!
//! Core BFS logic is in `build_test_map()` so batch mode can reuse it.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use anyhow::{Context as _, Result};

use cqs::store::{CallGraph, ChunkSummary};

use crate::cli::commands::resolve::resolve_target;

// ─── Shared data structures ─────────────────────────────────────────────────

/// A test that exercises the target function, found via reverse BFS.
pub(crate) struct TestMatch {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub depth: usize,
    pub chain: Vec<String>,
}

// ─── Shared core ────────────────────────────────────────────────────────────

/// Reverse BFS through the call graph to find all test chunks that call the
/// target, up to `max_depth`. Returns sorted matches.
///
/// Shared between CLI `cmd_test_map` and batch `dispatch_test_map`.
pub(crate) fn build_test_map(
    target_name: &str,
    graph: &CallGraph,
    test_chunks: &[ChunkSummary],
    root: &Path,
    max_depth: usize,
) -> Vec<TestMatch> {
    let _span = tracing::info_span!("build_test_map", target_name, max_depth).entered();

    // Reverse BFS from target
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

    // Collect matching tests
    let mut matches: Vec<TestMatch> = Vec::new();
    for test in test_chunks.iter() {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                let chain_limit = max_depth + 1;
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
                let rel_file = cqs::rel_display(&test.file, root);
                matches.push(TestMatch {
                    name: test.name.clone(),
                    file: rel_file,
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

/// Build JSON output from test map matches — shared between CLI and batch.
pub(crate) fn test_map_to_json(target_name: &str, matches: &[TestMatch]) -> serde_json::Value {
    let tests_json: Vec<_> = matches
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "file": m.file,
                "line": m.line,
                "call_depth": m.depth,
                "call_chain": m.chain,
            })
        })
        .collect();

    serde_json::json!({
        "function": target_name,
        "tests": tests_json,
        "test_count": matches.len(),
    })
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_test_map(
    ctx: &crate::cli::CommandContext,
    name: &str,
    max_depth: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_test_map", name).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let resolved = resolve_target(store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = store
        .get_call_graph()
        .context("Failed to load call graph")?;
    let test_chunks = store
        .find_test_chunks()
        .context("Failed to find test chunks")?;

    let matches = build_test_map(&target_name, &graph, &test_chunks, root, max_depth);

    if json {
        let output = test_map_to_json(&target_name, &matches);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;
        println!("{} {}", "Tests for:".cyan(), target_name.bold());
        if matches.is_empty() {
            println!("  No tests found");
        } else {
            for m in &matches {
                println!("  {} ({}:{}) [depth {}]", m.name, m.file, m.line, m.depth);
                if m.chain.len() > 2 {
                    println!("    chain: {}", m.chain.join(" -> "));
                }
            }
            println!("\n{} tests found", matches.len());
        }
    }

    Ok(())
}
