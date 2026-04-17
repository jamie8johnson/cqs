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

// ─── Output types ───────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct TestMapEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32, // was "line"
    pub call_depth: usize,
    pub call_chain: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TestMapOutput {
    pub name: String, // was "function"
    pub tests: Vec<TestMapEntry>,
    pub count: usize,
}

// ─── Shared core ────────────────────────────────────────────────────────────

/// Default maximum nodes in test-map reverse BFS traversal.
const DEFAULT_TEST_MAP_MAX_NODES: usize = 10_000;

/// Returns the test-map BFS node cap, reading `CQS_TEST_MAP_MAX_NODES` once on first call.
fn test_map_max_nodes() -> usize {
    use std::sync::OnceLock;
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| match std::env::var("CQS_TEST_MAP_MAX_NODES") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(
                    cap = n,
                    "Test-map BFS node cap overridden via CQS_TEST_MAP_MAX_NODES"
                );
                n
            }
            _ => {
                tracing::warn!(
                    val,
                    "CQS_TEST_MAP_MAX_NODES invalid, using default {DEFAULT_TEST_MAP_MAX_NODES}"
                );
                DEFAULT_TEST_MAP_MAX_NODES
            }
        },
        Err(_) => DEFAULT_TEST_MAP_MAX_NODES,
    })
}

/// Reverse BFS through the call graph to find all test chunks that call the
/// target, up to `max_depth`. Returns sorted matches.
///
/// Capped at `CQS_TEST_MAP_MAX_NODES` (default 10,000) visited nodes to prevent
/// OOM on dense graphs.
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
    let max_nodes = test_map_max_nodes();

    // Reverse BFS from target
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target_name.to_string(), (0, String::new()));
    queue.push_back((target_name.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if ancestors.len() >= max_nodes {
            tracing::warn!(
                target_name,
                max_nodes,
                "test_map reverse BFS hit node cap, returning partial results"
            );
            break;
        }
        if let Some(callers) = graph.reverse.get(current.as_str()) {
            for caller in callers {
                if ancestors.len() >= max_nodes {
                    break;
                }
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

/// Build typed test map output from matches -- shared between CLI and batch.
pub(crate) fn build_test_map_output(target_name: &str, matches: &[TestMatch]) -> TestMapOutput {
    let _span =
        tracing::info_span!("build_test_map_output", target_name, count = matches.len()).entered();
    TestMapOutput {
        name: target_name.to_string(),
        tests: matches
            .iter()
            .map(|m| TestMapEntry {
                name: m.name.clone(),
                file: m.file.clone(),
                line_start: m.line,
                call_depth: m.depth,
                call_chain: m.chain.clone(),
            })
            .collect(),
        count: matches.len(),
    }
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_test_map(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    max_depth: usize,
    limit: usize,
    cross_project: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_test_map", name, limit, cross_project).entered();
    // Task A3: cap on rendered matches. Default is 5 (LimitArg). Truncates the
    // BFS-derived matches AFTER sorting so the "closest" tests rank first.
    let limit = limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let test_chunks = cross_ctx.find_test_chunks_cross()?;

        // Build a merged call graph from all projects
        let graph = cross_ctx.merged_call_graph()?;
        let summaries: Vec<cqs::store::ChunkSummary> =
            test_chunks.iter().map(|tc| tc.chunk.clone()).collect();

        let mut matches = build_test_map(name, &graph, &summaries, &ctx.root, max_depth);
        matches.truncate(limit);

        if json {
            let output = build_test_map_output(name, &matches);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            use colored::Colorize;
            println!("{} {} (cross-project)", "Tests for:".cyan(), name.bold());
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
        return Ok(());
    }

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

    let mut matches = build_test_map(&target_name, &graph, &test_chunks, root, max_depth);
    matches.truncate(limit);

    if json {
        let output = build_test_map_output(&target_name, &matches);
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

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_test_map_output_field_names() {
        let output = TestMapOutput {
            name: "my_func".into(),
            tests: vec![TestMapEntry {
                name: "test_it".into(),
                file: "tests/foo.rs".into(),
                line_start: 10,
                call_depth: 1,
                call_chain: vec!["my_func".into()],
            }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "my_func"); // was "function"
        assert!(json.get("function").is_none());
        assert_eq!(json["tests"][0]["line_start"], 10); // was "line"
    }

    #[test]
    fn test_test_map_output_empty() {
        let output = build_test_map_output("no_tests", &[]);
        assert_eq!(output.count, 0);
        assert!(output.tests.is_empty());
    }
}
