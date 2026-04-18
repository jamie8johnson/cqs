//! `cqs plan` — task planning with template classification

use anyhow::{Context, Result};

use cqs::plan::plan;

pub(crate) fn cmd_plan(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    description: &str,
    limit: usize,
    json: bool,
    tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_plan", description).entered();

    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder().context("Failed to access embedder")?;

    let result =
        plan(store, embedder, description, root, limit).context("Plan generation failed")?;

    if json {
        let mut json_val = serde_json::to_value(&result)?;
        if let Some(budget) = tokens {
            json_val["token_budget"] = serde_json::json!(budget);
        }
        crate::cli::json_envelope::emit_json(&json_val)?;
    } else {
        display_plan_text(&result, root, tokens);
    }

    Ok(())
}

/// Displays a formatted text representation of a code query plan result to stdout.
fn display_plan_text(
    result: &cqs::plan::PlanResult,
    root: &std::path::Path,
    tokens: Option<usize>,
) {
    // v1.22.0 audit API-11: --tokens was accepted and silently ignored in
    // text mode. Warn so the user knows their budget isn't being applied.
    if tokens.is_some() {
        tracing::warn!(
            tokens,
            "--tokens is not yet applied in text mode (only JSON). Output may exceed budget."
        );
    }
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
