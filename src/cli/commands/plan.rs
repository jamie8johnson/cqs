//! `cqs plan` — task planning with template classification

use anyhow::{Context, Result};

use cqs::plan::{plan, plan_to_json};
use cqs::Embedder;

/// Generates a plan based on a given description and outputs it in either JSON or text format.
///
/// # Arguments
///
/// * `_cli` - CLI context (unused)
/// * `description` - The plan description to process
/// * `limit` - Maximum number of items to include in the plan
/// * `json` - If true, output as JSON; otherwise output as formatted text
/// * `tokens` - Optional token budget to include in the output
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if project store initialization, embedder creation, plan generation, or output formatting fails.
///
/// # Errors
///
/// Returns an error if:
/// * Opening the project store fails
/// * Creating the embedder fails
/// * Plan generation fails
/// * JSON serialization fails (when `json` is true)
pub(crate) fn cmd_plan(
    _cli: &crate::cli::Cli,
    description: &str,
    limit: usize,
    json: bool,
    tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_plan", description).entered();

    let (store, root, _) = crate::cli::open_project_store()?;
    let embedder = Embedder::new().context("Failed to create embedder")?;

    let result =
        plan(&store, &embedder, description, &root, limit).context("Plan generation failed")?;

    if json {
        let mut json_val = plan_to_json(&result, &root);
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
/// Outputs the plan template name and description, followed by scout file results (grouped files with relevance scores), a numbered checklist of items, and any identified patterns. File paths are displayed relative to the provided root directory.
///
/// # Arguments
///
/// * `result` - The plan result containing template info, scout findings, checklist items, and patterns
/// * `root` - Root path used to compute relative file paths for display
/// * `_tokens` - Unused parameter
///
/// # Returns
///
/// Returns nothing; output is printed to stdout.
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
