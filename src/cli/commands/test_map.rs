//! Test map command — find tests that exercise a function

use anyhow::{Context as _, Result};

use super::resolve::resolve_target;

pub(crate) fn cmd_test_map(name: &str, max_depth: usize, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_test_map", name).entered();
    let (store, root, _) = crate::cli::open_project_store_readonly()?;
    let resolved = resolve_target(&store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = store
        .get_call_graph()
        .context("Failed to load call graph")?;
    let test_chunks = store
        .find_test_chunks()
        .context("Failed to find test chunks")?;

    let matches = cqs::find_test_matches(&graph, &test_chunks, &target_name, max_depth, |test| {
        cqs::rel_display(&test.file, &root)
    });

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
