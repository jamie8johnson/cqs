//! `cqs plan` — task planning with template classification

use anyhow::{Context, Result};

use cqs::plan::{plan, PlanResult};
use cqs::Embedder;

// ---------------------------------------------------------------------------
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`plan_core`]. The embedder is a resource the adapter resolves
/// (`ctx.embedder()`); the request-scoped fields live here.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct PlanArgs {
    /// Natural-language task description to classify and plan.
    pub description: String,
    /// Max candidate results to surface per plan section.
    #[serde(default)]
    pub limit: usize,
    /// Token budget. In JSON output it is echoed as `token_budget`; in text
    /// output it packs the rendered sections to fit within the budget.
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
        display_plan_text(&output.plan, root, embedder, tokens);
    }

    Ok(())
}

/// Packed plan section indices under a token budget.
///
/// Each field holds indices into the corresponding `PlanResult` collection,
/// in original order. The template content (`checklist`, `patterns`) is what
/// distinguishes `plan` from `scout`, so it is packed first (highest priority);
/// the supplementary `scout` file groups degrade first when the budget is tight.
struct PlanPacked {
    checklist: Vec<usize>,
    patterns: Vec<usize>,
    scout: Vec<usize>,
    total_used: usize,
    budget: usize,
}

/// Count section tokens via the embedder, then pack to fit `budget`.
///
/// Thin wrapper that mirrors `cqs task`'s `waterfall_pack`: it turns each
/// rendered item into a representative string, batch-counts tokens, and hands
/// the counts to the pure [`pack_plan_sections`] packer.
fn plan_pack(result: &cqs::plan::PlanResult, embedder: &Embedder, budget: usize) -> PlanPacked {
    let _span = tracing::info_span!("plan_pack", budget).entered();

    let checklist_refs: Vec<&str> = result.checklist.iter().map(|s| s.as_str()).collect();
    let pattern_refs: Vec<&str> = result.patterns.iter().map(|s| s.as_str()).collect();
    let scout_texts: Vec<String> = result
        .scout
        .file_groups
        .iter()
        .map(|g| format!("{} ({} chunks)", g.file.display(), g.chunks.len()))
        .collect();
    let scout_refs: Vec<&str> = scout_texts.iter().map(|s| s.as_str()).collect();
    let scout_scores: Vec<f32> = result
        .scout
        .file_groups
        .iter()
        .map(|g| g.relevance_score)
        .collect();

    let checklist_counts = crate::cli::commands::count_tokens_batch(embedder, &checklist_refs);
    let pattern_counts = crate::cli::commands::count_tokens_batch(embedder, &pattern_refs);
    let scout_counts = crate::cli::commands::count_tokens_batch(embedder, &scout_refs);

    pack_plan_sections(
        &checklist_counts,
        &pattern_counts,
        &scout_counts,
        &scout_scores,
        budget,
    )
}

/// Pure token-budget packer for `plan`'s three renderable sections.
///
/// Sections are packed in priority order against a running budget: the
/// checklist and patterns (the template deliverable that makes `plan` more than
/// a bare `scout`) fill first, and the scout file groups get whatever remains.
/// Checklist and pattern items are ordered lists, so they pack earliest-first
/// (`-index` score); scout groups pack by relevance. `total_used` is the summed
/// token cost of the kept items and stays within `budget` (bar the
/// always-include-one overshoot inherited from `index_pack`).
fn pack_plan_sections(
    checklist_counts: &[usize],
    pattern_counts: &[usize],
    scout_counts: &[usize],
    scout_scores: &[f32],
    budget: usize,
) -> PlanPacked {
    use crate::cli::commands::index_pack;

    let mut remaining = budget;

    let (checklist, checklist_used) = index_pack(checklist_counts, remaining, 0, |i| -(i as f32));
    remaining = remaining.saturating_sub(checklist_used);

    let (patterns, patterns_used) = index_pack(pattern_counts, remaining, 0, |i| -(i as f32));
    remaining = remaining.saturating_sub(patterns_used);

    let (scout, scout_used) = index_pack(scout_counts, remaining, 0, |i| scout_scores[i]);

    let total_used = checklist_used + patterns_used + scout_used;
    tracing::info!(
        total = total_used,
        budget,
        checklist = checklist_used,
        patterns = patterns_used,
        scout = scout_used,
        "Plan budget complete"
    );

    PlanPacked {
        checklist,
        patterns,
        scout,
        total_used,
        budget,
    }
}

/// Displays a formatted text representation of a code query plan result to
/// stdout. When `tokens` is set, the rendered sections are packed to fit the
/// budget (mirrors `cqs task`'s budgeted text render); otherwise every item is
/// shown.
fn display_plan_text(
    result: &cqs::plan::PlanResult,
    root: &std::path::Path,
    embedder: &Embedder,
    tokens: Option<usize>,
) {
    use colored::Colorize;

    let packed = tokens.map(|budget| plan_pack(result, embedder, budget));

    let (checklist_idx, pattern_idx, scout_idx): (Vec<usize>, Vec<usize>, Vec<usize>) =
        match &packed {
            Some(p) => (p.checklist.clone(), p.patterns.clone(), p.scout.clone()),
            None => (
                (0..result.checklist.len()).collect(),
                (0..result.patterns.len()).collect(),
                (0..result.scout.file_groups.len()).collect(),
            ),
        };

    match &packed {
        Some(p) => println!(
            "{}{}",
            format!("Plan: {}", result.template).bold(),
            format!(" ({} of {} tokens)", p.total_used, p.budget).dimmed()
        ),
        None => println!("{}", format!("Plan: {}", result.template).bold()),
    }
    println!("{}", result.template_description.dimmed());
    println!();

    print_scout_section(result, root, &scout_idx);
    print_checklist_section(result, &checklist_idx);
    print_patterns_section(result, &pattern_idx);
}

/// Prints the scout file-group section for the packed `indices`, with a
/// truncation marker when the budget dropped some groups.
fn print_scout_section(result: &cqs::plan::PlanResult, root: &std::path::Path, indices: &[usize]) {
    use colored::Colorize;

    let total = result.scout.file_groups.len();
    if total == 0 {
        return;
    }
    println!("{}", "Scout Results:".bold());
    for &i in indices {
        let group = &result.scout.file_groups[i];
        let rel = cqs::rel_display(&group.file, root);
        let chunks = group.chunks.len();
        let score = group.relevance_score;
        println!("  {} ({} chunks, score {:.2})", rel.cyan(), chunks, score);
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more files truncated)", total - indices.len()).dimmed()
        );
    }
    println!();
}

/// Prints the checklist section for the packed `indices`, preserving original
/// step numbers and appending a truncation marker when items were dropped.
fn print_checklist_section(result: &cqs::plan::PlanResult, indices: &[usize]) {
    use colored::Colorize;

    let total = result.checklist.len();
    if total == 0 {
        return;
    }
    println!("{}", "Checklist:".bold());
    for &i in indices {
        println!("  {}. {}", i + 1, result.checklist[i]);
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more steps truncated)", total - indices.len()).dimmed()
        );
    }
    println!();
}

/// Prints the patterns section for the packed `indices`, with a truncation
/// marker when the budget dropped some patterns.
fn print_patterns_section(result: &cqs::plan::PlanResult, indices: &[usize]) {
    use colored::Colorize;

    let total = result.patterns.len();
    if total == 0 {
        return;
    }
    println!("{}", "Patterns:".bold());
    for &i in indices {
        println!("  - {}", result.patterns[i]);
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more patterns truncated)", total - indices.len()).dimmed()
        );
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

    // ── Text-mode token budgeting (`pack_plan_sections`) ────────────────────

    /// A generous budget keeps every item in every section, and the reported
    /// token usage equals the summed cost — no truncation.
    #[test]
    fn plan_pack_generous_budget_keeps_all() {
        let checklist = vec![10, 12, 8];
        let patterns = vec![6, 6];
        let scout = vec![20, 15];
        let scores = vec![0.9_f32, 0.5];
        let budget = 1000;

        let packed = pack_plan_sections(&checklist, &patterns, &scout, &scores, budget);

        assert_eq!(packed.checklist, vec![0, 1, 2], "all checklist steps kept");
        assert_eq!(packed.patterns, vec![0, 1], "all patterns kept");
        assert_eq!(packed.scout, vec![0, 1], "all scout groups kept");
        assert_eq!(
            packed.total_used,
            checklist
                .iter()
                .chain(&patterns)
                .chain(&scout)
                .sum::<usize>(),
            "usage is the full summed cost when nothing is dropped"
        );
        assert!(packed.total_used <= budget);
    }

    /// A budget small enough to force truncation drops items and keeps the
    /// reported usage within the budget. The template deliverable (checklist,
    /// patterns) is packed first; the supplementary scout section degrades
    /// first — here it is dropped entirely.
    #[test]
    fn plan_pack_tight_budget_truncates_within_budget() {
        let checklist = vec![10, 10, 10, 10];
        let patterns = vec![10, 10];
        let scout = vec![10, 10];
        let scores = vec![0.9_f32, 0.5];
        // Fits the four checklist steps (40) but nothing else.
        let budget = 40;

        let packed = pack_plan_sections(&checklist, &patterns, &scout, &scores, budget);

        assert!(
            packed.total_used <= budget,
            "packed usage {} must stay within budget {budget}",
            packed.total_used
        );
        let kept = packed.checklist.len() + packed.patterns.len() + packed.scout.len();
        let total = checklist.len() + patterns.len() + scout.len();
        assert!(kept < total, "a tight budget must drop at least one item");
        assert!(
            !packed.checklist.is_empty(),
            "the checklist (deliverable) is packed first, so it survives"
        );
        assert!(
            packed.scout.is_empty(),
            "the supplementary scout section degrades first"
        );
    }

    /// Checklist steps pack earliest-first so a partial checklist keeps its
    /// leading (highest-priority) steps in original order.
    #[test]
    fn plan_pack_checklist_keeps_leading_steps() {
        let checklist = vec![10, 10, 10, 10, 10];
        let patterns: Vec<usize> = vec![];
        let scout: Vec<usize> = vec![];
        let scores: Vec<f32> = vec![];
        let budget = 25; // fits two steps

        let packed = pack_plan_sections(&checklist, &patterns, &scout, &scores, budget);

        assert_eq!(
            packed.checklist,
            vec![0, 1],
            "keeps the first two steps in order, not a later subset"
        );
        assert!(packed.total_used <= budget);
    }

    /// A zero budget keeps nothing (`index_pack` returns empty for budget 0).
    #[test]
    fn plan_pack_zero_budget_keeps_nothing() {
        let packed = pack_plan_sections(&[10, 10], &[10], &[10], &[0.5], 0);
        assert!(packed.checklist.is_empty());
        assert!(packed.patterns.is_empty());
        assert!(packed.scout.is_empty());
        assert_eq!(packed.total_used, 0);
    }
}
