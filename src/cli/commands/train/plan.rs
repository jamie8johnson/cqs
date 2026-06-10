//! `cqs plan` — task planning with template classification

use anyhow::{Context, Result};

use cqs::plan::{plan, PlanResult};
use cqs::Embedder;

// ---------------------------------------------------------------------------
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`plan_core`]. The embedder is a resource the adapter resolves
/// (`ctx.embedder()`); the request-scoped fields live here.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct PlanArgs {
    /// Natural-language task description to classify and plan.
    pub description: String,
    /// Max candidate results to surface per plan section.
    #[serde(default)]
    pub limit: usize,
    /// Token budget echoed onto the output as `token_budget` (JSON only).
    #[serde(default)]
    pub tokens: Option<usize>,
}

/// Typed output for `cqs plan`. Flattens the lib [`PlanResult`] and adds the
/// optional `token_budget` echo the adapters previously spliced inline.
/// THE schema — both surfaces serialize this.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PlanOutput {
    #[serde(flatten)]
    pub plan: PlanResult,
    /// Echoed token budget (present only when `--tokens` set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}

/// Surface-agnostic core for `cqs plan`. Runs the planning pipeline (a pure
/// read query over the store + embedder) and returns the typed [`PlanOutput`].
/// Both the CLI (`cmd_plan`) and daemon (`dispatch_plan`) drive this.
pub(crate) fn plan_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    embedder: &Embedder,
    root: &std::path::Path,
    args: &PlanArgs,
) -> Result<PlanOutput> {
    let _span = tracing::info_span!("plan_core", description = %args.description).entered();
    let result = plan(store, embedder, &args.description, root, args.limit)
        .context("Plan generation failed")?;
    Ok(PlanOutput {
        plan: result,
        token_budget: args.tokens,
    })
}

pub(crate) fn cmd_plan(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    description: &str,
    limit: usize,
    json: bool,
    tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_plan", description).entered();

    let root = &ctx.root;
    let embedder = ctx.embedder().context("Failed to access embedder")?;

    let args = PlanArgs {
        description: description.to_string(),
        limit,
        tokens,
    };

    if json {
        let output = plan_core(&ctx.store, embedder, root, &args)?;
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        let output = plan_core(&ctx.store, embedder, root, &args)?;
        display_plan_text(&output.plan, root, tokens);
    }

    Ok(())
}

/// Displays a formatted text representation of a code query plan result to stdout.
fn display_plan_text(
    result: &cqs::plan::PlanResult,
    root: &std::path::Path,
    tokens: Option<usize>,
) {
    // --tokens is accepted but only applied in JSON mode. Warn so the user
    // knows their budget isn't being applied in text mode.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `PlanArgs` deserializes from a wire/MCP-shaped object with defaults for
    /// the optional fields (limit, tokens).
    #[test]
    fn plan_args_minimal_deserialize() {
        let args: PlanArgs =
            serde_json::from_value(serde_json::json!({"description": "add a flag"})).unwrap();
        assert_eq!(args.description, "add a flag");
        assert_eq!(args.limit, 0, "limit defaults to 0 (clamped by the lib)");
        assert!(args.tokens.is_none(), "tokens defaults to None");
    }
}
