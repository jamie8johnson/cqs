//! `cqs plan` — task planning with template classification

use anyhow::{Context, Result};

use cqs::plan::{plan, plan_to_json};
use cqs::Embedder;

pub(crate) fn cmd_plan(
    _cli: &crate::cli::Cli,
    description: &str,
    limit: usize,
    json: bool,
    tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_plan", description).entered();

    let (store, root, _) = crate::cli::open_project_store_readonly()?;
    let embedder = Embedder::new().context("Failed to create embedder")?;

    let result =
        plan(&store, &embedder, description, &root, limit).context("Plan generation failed")?;

    if json {
        let mut json_val = plan_to_json(&result);
        if let Some(budget) = tokens {
            json_val["token_budget"] = serde_json::json!(budget);
        }
        println!("{}", serde_json::to_string_pretty(&json_val)?);
    } else {
        display_plan_text(&result, &root, tokens);
    }

    Ok(())
}

/// Displays a formatted text representation of a code query plan result to stdout.
///
fn display_plan_text(
    result: &cqs::plan::PlanResult,
    root: &std::path::Path,
    _tokens: Option<usize>,
) {
    use colored::Colorize;

    println!("{}", format!("Plan: {}", result.template).bold());
    println!("{}", result.template_description.dimmed());
    println!();

    // Scout results
    if !result.scout.file_groups.is_empty() {
        println!("{}", "Scout Results:".bold());
        for group in &result.scout.file_groups {
            let rel = cqs::rel_display(&group.file, root);
            let chunks = group.chunks.len();
            let score = group.relevance_score;
            println!("  {} ({} chunks, score {:.2})", rel.cyan(), chunks, score);
        }
        println!();
    }

    // Checklist
    println!("{}", "Checklist:".bold());
    for (i, item) in result.checklist.iter().enumerate() {
        println!("  {}. {}", i + 1, item);
    }
    println!();

    // Patterns
    if !result.patterns.is_empty() {
        println!("{}", "Patterns:".bold());
        for pattern in &result.patterns {
            println!("  - {}", pattern);
        }
    }
}
