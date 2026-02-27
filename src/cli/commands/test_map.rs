//! Test map command — find tests that exercise a function

use anyhow::{Context as _, Result};
use std::collections::{HashMap, HashSet, VecDeque};

use super::resolve::resolve_target;

pub(crate) fn cmd_test_map(
    _cli: &crate::cli::Cli,
    name: &str,
    max_depth: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_test_map", name).entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let resolved = resolve_target(&store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = store
        .get_call_graph()
        .context("Failed to load call graph")?;
    let test_chunks = store
        .find_test_chunks()
        .context("Failed to find test chunks")?;
    let _test_names: HashSet<String> = test_chunks.iter().map(|t| t.name.clone()).collect();

    // Reverse BFS from target
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
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

    // Collect matching tests
    struct TestMatch {
        name: String,
        file: String,
        line: u32,
        depth: usize,
        chain: Vec<String>,
    }

    let mut matches: Vec<TestMatch> = Vec::new();
    for test in &test_chunks {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                while !current.is_empty() {
                    chain.push(current.clone());
                    if current == target_name {
                        break;
                    }
                    current = ancestors
                        .get(&current)
                        .map(|(_, p)| p.clone())
                        .unwrap_or_default();
                }
                let rel_file = cqs::rel_display(&test.file, &root);
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

    if json {
        let tests_json: Vec<_> = matches
            .iter()
            .map(|m| {
                serde_json::json!({"name": m.name, "file": m.file, "line": m.line, "call_depth": m.depth, "call_chain": m.chain})
            })
            .collect();
        let output = serde_json::json!({"function": target_name, "tests": tests_json, "test_count": matches.len()});
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
