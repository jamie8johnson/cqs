//! Task command — one-shot implementation context for a task description.

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::{task, Embedder};

// ─── Output types ──────────────────────────────────────────────────────────

/// Brief task output: files to touch, placements, at-risk functions, tests.
#[derive(Debug, serde::Serialize)]
pub(crate) struct TaskBriefOutput {
    pub files: Vec<String>,
    pub placements: Vec<String>,
    pub risk: Vec<TaskBriefRisk>,
    pub tests: Vec<String>,
    pub summary: TaskBriefSummary,
}

/// Risk entry in the brief task output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct TaskBriefRisk {
    pub name: String,
    pub risk_level: String,
}

/// Summary counts in the brief task output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct TaskBriefSummary {
    pub files: usize,
    pub functions: usize,
    pub high_risk: usize,
    pub tests: usize,
}

/// A single scout chunk in the budgeted output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedScoutChunk {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub role: String,
    pub caller_count: usize,
    pub test_count: usize,
    pub search_score: f32,
}

/// A single file group in the budgeted scout output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedScoutGroup {
    pub file: String,
    pub relevance_score: f32,
    pub is_stale: bool,
    pub chunks: Vec<BudgetedScoutChunk>,
}

/// Summary for the budgeted scout section.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedScoutSummary {
    pub total_files: usize,
    pub total_functions: usize,
    pub untested_count: usize,
    pub stale_count: usize,
}

/// The scout section of the budgeted output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedScoutOutput {
    pub file_groups: Vec<BudgetedScoutGroup>,
    pub summary: BudgetedScoutSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevant_notes: Option<Vec<BudgetedNoteEntry>>,
}

/// A note entry in the budgeted output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedNoteEntry {
    pub text: String,
    pub sentiment: f32,
    pub mentions: Vec<String>,
}

/// Summary for the budgeted task output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedTaskSummary {
    pub total_files: usize,
    pub total_functions: usize,
    pub modify_targets: usize,
    pub high_risk_count: usize,
    pub test_count: usize,
    pub stale_count: usize,
}

/// Full budgeted task output combining all packed sections.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BudgetedTaskOutput {
    pub description: String,
    pub scout: BudgetedScoutOutput,
    pub code: Vec<serde_json::Value>,
    pub risk: Vec<serde_json::Value>,
    pub tests: Vec<serde_json::Value>,
    pub placement: Vec<serde_json::Value>,
    pub summary: BudgetedTaskSummary,
    pub token_count: usize,
    pub token_budget: usize,
}

// ─── Typed builders ────────────────────────────────────────────────────────

/// Build brief task output from a TaskResult.
pub(crate) fn build_task_brief(
    result: &cqs::TaskResult,
    root: &std::path::Path,
) -> TaskBriefOutput {
    let _span = tracing::info_span!("build_task_brief").entered();

    let files: Vec<String> = {
        use std::collections::BTreeSet;
        result
            .scout
            .file_groups
            .iter()
            .map(|g| {
                g.file
                    .strip_prefix(root)
                    .unwrap_or(&g.file)
                    .display()
                    .to_string()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };

    let placements: Vec<String> = result
        .placement
        .iter()
        .take(3)
        .map(|p| {
            let path = p.file.strip_prefix(root).unwrap_or(&p.file);
            format!(
                "{}:{} (near {})",
                path.display(),
                p.insertion_line,
                p.near_function
            )
        })
        .collect();

    let risk: Vec<TaskBriefRisk> = result
        .risk
        .iter()
        .map(|r| TaskBriefRisk {
            name: r.name.clone(),
            risk_level: r.risk.risk_level.to_string(),
        })
        .collect();

    let tests: Vec<String> = result.tests.iter().map(|t| t.name.clone()).collect();

    TaskBriefOutput {
        files,
        placements,
        risk,
        tests,
        summary: TaskBriefSummary {
            files: result.summary.total_files,
            functions: result.summary.total_functions,
            high_risk: result.summary.high_risk_count,
            tests: result.summary.test_count,
        },
    }
}

/// Build budgeted scout section from packed indices.
fn build_budgeted_scout(result: &cqs::TaskResult, indices: &[usize]) -> BudgetedScoutOutput {
    let _span = tracing::info_span!("build_budgeted_scout", count = indices.len()).entered();

    let file_groups: Vec<BudgetedScoutGroup> = indices
        .iter()
        .map(|&i| {
            let g = &result.scout.file_groups[i];
            let chunks: Vec<BudgetedScoutChunk> = g
                .chunks
                .iter()
                .map(|c| BudgetedScoutChunk {
                    name: c.name.clone(),
                    chunk_type: c.chunk_type.to_string(),
                    signature: c.signature.clone(),
                    line_start: c.line_start,
                    role: c.role.as_str().to_string(),
                    caller_count: c.caller_count,
                    test_count: c.test_count,
                    search_score: c.search_score,
                })
                .collect();
            BudgetedScoutGroup {
                file: cqs::normalize_path(&g.file).to_string(),
                relevance_score: g.relevance_score,
                is_stale: g.is_stale,
                chunks,
            }
        })
        .collect();

    BudgetedScoutOutput {
        file_groups,
        summary: BudgetedScoutSummary {
            total_files: result.scout.summary.total_files,
            total_functions: result.scout.summary.total_functions,
            untested_count: result.scout.summary.untested_count,
            stale_count: result.scout.summary.stale_count,
        },
        relevant_notes: None,
    }
}

/// Build budgeted notes from packed indices.
fn build_budgeted_notes(result: &cqs::TaskResult, indices: &[usize]) -> Vec<BudgetedNoteEntry> {
    let _span = tracing::info_span!("build_budgeted_notes", count = indices.len()).entered();
    indices
        .iter()
        .map(|&i| {
            let n = &result.scout.relevant_notes[i];
            BudgetedNoteEntry {
                text: n.text.clone(),
                sentiment: n.sentiment,
                mentions: n.mentions.clone(),
            }
        })
        .collect()
}

/// Build the full budgeted task output combining all packed sections.
pub(crate) fn build_budgeted_task(
    result: &cqs::TaskResult,
    packed: &PackedSections,
) -> BudgetedTaskOutput {
    let _span = tracing::info_span!("build_budgeted_task").entered();

    let mut scout = build_budgeted_scout(result, &packed.scout);
    let notes = build_budgeted_notes(result, &packed.notes);
    if !notes.is_empty() {
        scout.relevant_notes = Some(notes);
    }

    BudgetedTaskOutput {
        description: result.description.clone(),
        scout,
        code: build_code_json(result, &packed.code),
        risk: build_risk_json(result, &packed.risk),
        tests: build_tests_json(result, &packed.tests),
        placement: build_placement_json(result, &packed.placement),
        summary: BudgetedTaskSummary {
            total_files: result.summary.total_files,
            total_functions: result.summary.total_functions,
            modify_targets: result.summary.modify_targets,
            high_risk_count: result.summary.high_risk_count,
            test_count: result.summary.test_count,
            stale_count: result.summary.stale_count,
        },
        token_count: packed.total_used,
        token_budget: packed.budget,
    }
}

/// Waterfall budget weight for the scout section (file groups, chunk roles).
const WATERFALL_SCOUT: f64 = 0.15;
/// Waterfall budget weight for the code section (gathered chunks with content).
const WATERFALL_CODE: f64 = 0.50;
/// Waterfall budget weight for the impact section (risk scores + tests).
const WATERFALL_IMPACT: f64 = 0.15;
/// Waterfall budget weight for the placement section (where to add).
const WATERFALL_PLACEMENT: f64 = 0.10;
// Notes section takes whatever budget remains (no explicit constant needed).

pub(crate) fn cmd_task(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    description: &str,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
    brief: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_task", ?max_tokens, brief).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);

    let result = task(store, embedder, description, root, limit)?;

    if brief {
        output_brief(&result, root, json)?;
    } else if let Some(budget) = max_tokens {
        output_with_budget(&result, root, embedder, budget, json)?;
    } else if json {
        let output = serde_json::to_value(&result)?;
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        output_text(&result, root);
    }

    Ok(())
}

/// Compact output (~200 tokens): files to touch, at-risk functions, test coverage.
fn output_brief(result: &cqs::TaskResult, root: &std::path::Path, json: bool) -> Result<()> {
    let brief = build_task_brief(result, root);

    if json {
        crate::cli::json_envelope::emit_json(&brief)?;
    } else {
        println!("{}", "Files:".bold());
        for f in &brief.files {
            println!("  {f}");
        }
        if !brief.placements.is_empty() {
            println!("\n{}", "Add code at:".bold());
            for p in &brief.placements {
                println!("  {p}");
            }
        }
        if !brief.risk.is_empty() {
            println!("\n{}", "At risk:".bold());
            for r in &brief.risk {
                let styled = match r.risk_level.as_str() {
                    "High" => format!("{} [{}]", r.name, r.risk_level).red().to_string(),
                    "Medium" => format!("{} [{}]", r.name, r.risk_level)
                        .yellow()
                        .to_string(),
                    _ => format!("{} [{}]", r.name, r.risk_level),
                };
                println!("  {styled}");
            }
        }
        println!(
            "\n{} files, {} functions, {} high-risk, {} tests",
            brief.summary.files,
            brief.summary.functions,
            brief.summary.high_risk,
            brief.summary.tests,
        );
    }

    Ok(())
}

/// Waterfall token budgeting: allocate budget across sections, surplus flows forward.
fn output_with_budget(
    result: &cqs::TaskResult,
    root: &std::path::Path,
    embedder: &Embedder,
    budget: usize,
    json: bool,
) -> Result<()> {
    let overhead = if json {
        crate::cli::commands::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let packed = waterfall_pack(result, embedder, budget, overhead);

    if json {
        output_json_budgeted(result, &packed)?;
    } else {
        output_text_budgeted(result, root, &packed);
    }

    Ok(())
}

/// Packed section indices from waterfall budgeting.
pub(crate) struct PackedSections {
    scout: Vec<usize>,
    code: Vec<usize>,
    risk: Vec<usize>,
    tests: Vec<usize>,
    placement: Vec<usize>,
    notes: Vec<usize>,
    pub(crate) total_used: usize,
    pub(crate) budget: usize,
}

/// Pack a section: count tokens for texts, run index_pack, return (indices, used).
/// Extracts the repeated pattern of count_tokens_batch + index_pack used by
/// each waterfall section. CQ-26.
fn pack_section(
    embedder: &Embedder,
    texts: &[&str],
    section_budget: usize,
    overhead: usize,
    score_fn: impl Fn(usize) -> f32,
) -> (Vec<usize>, usize) {
    let counts = crate::cli::commands::count_tokens_batch(embedder, texts);
    crate::cli::commands::index_pack(&counts, section_budget, overhead, score_fn)
}

/// Compute waterfall token budgeting across all task sections.
/// Shared between CLI `cqs task --tokens` and batch `task --tokens`.
/// `overhead_per_item` should be `JSON_OVERHEAD_PER_RESULT` for JSON, 0 for text.
pub(crate) fn waterfall_pack(
    result: &cqs::TaskResult,
    embedder: &Embedder,
    budget: usize,
    overhead_per_item: usize,
) -> PackedSections {
    let _span = tracing::info_span!("waterfall_budget", budget).entered();
    let mut remaining = budget;

    // 1. Scout section — pack file groups by relevance
    let scout_budget = ((budget as f64 * WATERFALL_SCOUT) as usize).min(remaining);
    let group_texts: Vec<String> = result
        .scout
        .file_groups
        .iter()
        .map(|g| {
            g.chunks
                .iter()
                .map(|c| c.signature.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    let group_refs: Vec<&str> = group_texts.iter().map(|s| s.as_str()).collect();
    let (scout_indices, scout_used) = pack_section(
        embedder,
        &group_refs,
        scout_budget,
        overhead_per_item,
        |i| result.scout.file_groups[i].relevance_score,
    );
    // Charge actual usage to remaining — overshoot from first-item guarantee
    // must reduce downstream budgets to prevent total overshoot
    remaining = remaining.saturating_sub(scout_used);

    // 2. Code section (+ surplus) — pack gathered chunks by score
    // NOTE: `pack_section` guarantees at least one item even if it exceeds the
    // budget. That overshoot is absorbed here via saturating_sub on `remaining`,
    // which reduces downstream section budgets proportionally. The total output
    // may slightly exceed `budget` by at most one item's token count.
    let code_budget = ((budget as f64 * WATERFALL_CODE) as usize
        + scout_budget.saturating_sub(scout_used))
    .min(remaining);
    let code_refs: Vec<&str> = result.code.iter().map(|c| c.content.as_str()).collect();
    let (code_indices, code_used) =
        pack_section(embedder, &code_refs, code_budget, overhead_per_item, |i| {
            result.code[i].score
        });
    remaining = remaining.saturating_sub(code_used);

    // 3. Impact section (+ surplus) — risk by score, tests by depth
    let impact_budget = ((budget as f64 * WATERFALL_IMPACT) as usize
        + code_budget.saturating_sub(code_used))
    .min(remaining);
    let risk_texts: Vec<String> = result
        .risk
        .iter()
        .map(|fr| {
            format!(
                "{}: {:?} score:{:.1} callers:{} cov:{:.0}%",
                fr.name,
                fr.risk.risk_level,
                fr.risk.score,
                fr.risk.caller_count,
                fr.risk.test_ratio * 100.0
            )
        })
        .collect();
    let risk_refs: Vec<&str> = risk_texts.iter().map(|s| s.as_str()).collect();
    let (risk_indices, risk_used) = pack_section(
        embedder,
        &risk_refs,
        impact_budget,
        overhead_per_item,
        |i| result.risk[i].risk.score,
    );

    let tests_budget = impact_budget.saturating_sub(risk_used);
    let test_texts: Vec<String> = result
        .tests
        .iter()
        .map(|t| {
            format!(
                "{} {}:{} depth:{}",
                t.name,
                t.file.display(),
                t.line,
                t.call_depth
            )
        })
        .collect();
    let test_refs: Vec<&str> = test_texts.iter().map(|s| s.as_str()).collect();
    let (test_indices, tests_used) =
        pack_section(embedder, &test_refs, tests_budget, overhead_per_item, |i| {
            1.0 / (result.tests[i].call_depth as f32 + 1.0)
        });
    remaining = remaining.saturating_sub(risk_used + tests_used);

    // 4. Placement section (+ surplus)
    let placement_budget = ((budget as f64 * WATERFALL_PLACEMENT) as usize
        + impact_budget.saturating_sub(risk_used + tests_used))
    .min(remaining);
    let placement_texts: Vec<String> = result
        .placement
        .iter()
        .map(|s| {
            format!(
                "{}: {} line:{} near:{}",
                s.file.display(),
                s.reason,
                s.insertion_line,
                s.near_function
            )
        })
        .collect();
    let placement_refs: Vec<&str> = placement_texts.iter().map(|s| s.as_str()).collect();
    let (placement_indices, placement_used) = pack_section(
        embedder,
        &placement_refs,
        placement_budget,
        overhead_per_item,
        |i| result.placement[i].score,
    );
    remaining = remaining.saturating_sub(placement_used);

    // 5. Notes section — takes whatever budget remains
    let note_refs: Vec<&str> = result
        .scout
        .relevant_notes
        .iter()
        .map(|n| n.text.as_str())
        .collect();
    let (note_indices, notes_used) =
        pack_section(embedder, &note_refs, remaining, overhead_per_item, |i| {
            result.scout.relevant_notes[i].sentiment.abs()
        });

    let total_used = scout_used + code_used + risk_used + tests_used + placement_used + notes_used;

    tracing::info!(
        total = total_used,
        budget,
        scout = scout_used,
        code = code_used,
        risk = risk_used,
        tests = tests_used,
        placement = placement_used,
        notes = notes_used,
        "Waterfall budget complete"
    );

    PackedSections {
        scout: scout_indices,
        code: code_indices,
        risk: risk_indices,
        tests: test_indices,
        placement: placement_indices,
        notes: note_indices,
        total_used,
        budget,
    }
}

/// Build budgeted JSON for a task result using full waterfall token budgeting.
/// Shared between CLI `cqs task --tokens --json` and batch `task --tokens`.
/// Returns `Err` on serialization failure so the batch dispatcher can emit a
/// proper envelope error instead of nesting `{"error": ...}` inside `data`.
pub(crate) fn task_to_budgeted_json(
    result: &cqs::TaskResult,
    embedder: &Embedder,
    budget: usize,
) -> Result<serde_json::Value> {
    let packed = waterfall_pack(
        result,
        embedder,
        budget,
        crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
    );
    let output = build_budgeted_task(result, &packed);
    serde_json::to_value(&output).context("Failed to serialize budgeted task output")
}

fn output_json_budgeted(result: &cqs::TaskResult, packed: &PackedSections) -> Result<()> {
    let output = build_budgeted_task(result, packed);
    crate::cli::json_envelope::emit_json(&output)?;
    Ok(())
}

fn build_code_json(result: &cqs::TaskResult, indices: &[usize]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .filter_map(|&i| match serde_json::to_value(&result.code[i]) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, chunk = %result.code[i].name, "Failed to serialize chunk");
                None
            }
        })
        .collect()
}

fn build_risk_json(result: &cqs::TaskResult, indices: &[usize]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .filter_map(|&i| match serde_json::to_value(&result.risk[i]) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(index = i, error = %e, "Failed to serialize risk entry");
                None
            }
        })
        .collect()
}

/// Converts a subset of test results to JSON format.
/// Paths in tests are already relative to the project root.
fn build_tests_json(result: &cqs::TaskResult, indices: &[usize]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .filter_map(|&i| match serde_json::to_value(&result.tests[i]) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(index = i, error = %e, "Failed to serialize test entry");
                None
            }
        })
        .collect()
}

/// Builds a JSON representation of task result placements for specified indices.
/// Paths in placement are already relative to the project root.
fn build_placement_json(result: &cqs::TaskResult, indices: &[usize]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .filter_map(|&i| match serde_json::to_value(&result.placement[i]) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(index = i, error = %e, "Failed to serialize placement entry");
                None
            }
        })
        .collect()
}

// build_notes_json replaced by build_budgeted_notes (typed BudgetedNoteEntry)

fn output_text_budgeted(result: &cqs::TaskResult, root: &std::path::Path, packed: &PackedSections) {
    print_header(
        &result.description,
        &result.summary,
        packed.total_used,
        packed.budget,
    );
    print_scout_section(result, root, &packed.scout);
    print_code_section_idx(&result.code, root, &packed.code, result.code.len());
    print_impact_section_idx(&result.risk, &result.tests, &packed.risk, &packed.tests);
    print_placement_section_idx(
        &result.placement,
        root,
        &packed.placement,
        result.placement.len(),
    );
    print_notes_section_idx(
        &result.scout.relevant_notes,
        &packed.notes,
        result.scout.relevant_notes.len(),
    );
}

fn output_text(result: &cqs::TaskResult, root: &std::path::Path) {
    let all_scout: Vec<usize> = (0..result.scout.file_groups.len()).collect();
    print_header(&result.description, &result.summary, 0, 0);
    print_scout_section(result, root, &all_scout);

    let all_code: Vec<usize> = (0..result.code.len()).collect();
    print_code_section_idx(&result.code, root, &all_code, result.code.len());

    let all_risk: Vec<usize> = (0..result.risk.len()).collect();
    let all_tests: Vec<usize> = (0..result.tests.len()).collect();
    print_impact_section_idx(&result.risk, &result.tests, &all_risk, &all_tests);

    let all_placement: Vec<usize> = (0..result.placement.len()).collect();
    print_placement_section_idx(
        &result.placement,
        root,
        &all_placement,
        result.placement.len(),
    );

    let all_notes: Vec<usize> = (0..result.scout.relevant_notes.len()).collect();
    print_notes_section_idx(
        &result.scout.relevant_notes,
        &all_notes,
        result.scout.relevant_notes.len(),
    );
}

fn print_header(description: &str, summary: &cqs::TaskSummary, used: usize, budget: usize) {
    let token_label = if budget > 0 {
        format!(" ({} of {} tokens)", used, budget)
    } else {
        String::new()
    };
    println!(
        "{} {}{}",
        "═══ Task:".cyan().bold(),
        description.bold(),
        token_label.dimmed()
    );
    println!(
        "  {} targets | {} files | {} tests | {} high-risk",
        summary.modify_targets.to_string().bold(),
        summary.total_files,
        summary.test_count,
        summary.high_risk_count
    );
}

fn print_scout_section(result: &cqs::TaskResult, root: &std::path::Path, indices: &[usize]) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Scout ──────────────────────────────".cyan());
    let total = result.scout.file_groups.len();
    for &i in indices {
        let g = &result.scout.file_groups[i];
        let rel = cqs::rel_display(&g.file, root);
        print!(
            "  {} {}",
            rel.bold(),
            format!("({:.2})", g.relevance_score).dimmed()
        );
        if g.is_stale {
            print!(" {}", "[STALE]".yellow().bold());
        }
        println!();
        for c in &g.chunks {
            let role = match c.role {
                cqs::ChunkRole::ModifyTarget => "modify",
                cqs::ChunkRole::TestToUpdate => "test",
                cqs::ChunkRole::Dependency => "dep",
            };
            println!(
                "    {} {} {} {}",
                "▸".dimmed(),
                c.name,
                format!("({})", role).dimmed(),
                format!("callers:{} tests:{}", c.caller_count, c.test_count).dimmed()
            );
        }
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more files truncated)", total - indices.len()).dimmed()
        );
    }
}

fn print_code_section_idx(
    code: &[cqs::GatheredChunk],
    root: &std::path::Path,
    indices: &[usize],
    total: usize,
) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Code ───────────────────────────────".cyan());
    for &i in indices {
        let c = &code[i];
        let rel = cqs::rel_display(&c.file, root);
        println!("  {} {}:{}", c.name.bold(), rel, c.line_start);
        if !c.signature.is_empty() {
            println!("    {}", c.signature.dimmed());
        }
        let mut line_count = 0;
        for line in c.content.lines().take(5) {
            println!("    {}", line);
            line_count += 1;
        }
        if line_count == 5 && c.content.lines().nth(5).is_some() {
            println!("    {}", "...".dimmed());
        }
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more items truncated)", total - indices.len()).dimmed()
        );
    }
}

fn print_impact_section_idx(
    risk: &[cqs::FunctionRisk],
    tests: &[cqs::TestInfo],
    risk_idx: &[usize],
    test_idx: &[usize],
) {
    if risk_idx.is_empty() && test_idx.is_empty() {
        return;
    }
    if !risk_idx.is_empty() {
        println!();
        println!("{}", "── Impact ─────────────────────────────".cyan());
        for &i in risk_idx {
            let fr = &risk[i];
            let level = match fr.risk.risk_level {
                cqs::RiskLevel::High => {
                    format!("{:?}", fr.risk.risk_level).red().bold().to_string()
                }
                cqs::RiskLevel::Medium => format!("{:?}", fr.risk.risk_level).yellow().to_string(),
                cqs::RiskLevel::Low => format!("{:?}", fr.risk.risk_level).green().to_string(),
            };
            println!(
                "  {}: {} {}",
                fr.name,
                level,
                format!(
                    "(score: {:.1}, callers: {}, test_ratio: {:.0}%)",
                    fr.risk.score,
                    fr.risk.caller_count,
                    fr.risk.test_ratio * 100.0
                )
                .dimmed()
            );
        }
        if risk_idx.len() < risk.len() {
            println!(
                "  {}",
                format!(
                    "({} more risk entries truncated)",
                    risk.len() - risk_idx.len()
                )
                .dimmed()
            );
        }
    }

    if !test_idx.is_empty() {
        println!();
        println!("{}", "── Tests ──────────────────────────────".cyan());
        for &i in test_idx {
            let t = &tests[i];
            let rel = cqs::rel_display(&t.file, std::path::Path::new(""));
            println!(
                "  {} {}:{} {}",
                t.name,
                rel,
                t.line,
                format!("depth:{}", t.call_depth).dimmed()
            );
        }
        if test_idx.len() < tests.len() {
            println!(
                "  {}",
                format!("({} more tests truncated)", tests.len() - test_idx.len()).dimmed()
            );
        }
    }
}

fn print_placement_section_idx(
    placement: &[cqs::FileSuggestion],
    root: &std::path::Path,
    indices: &[usize],
    total: usize,
) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Placement ──────────────────────────".cyan());
    for &i in indices {
        let s = &placement[i];
        let rel = cqs::rel_display(&s.file, root);
        println!("  {} — {}", rel.bold(), s.reason.dimmed());
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more suggestions truncated)", total - indices.len()).dimmed()
        );
    }
}

fn print_notes_section_idx(notes: &[cqs::store::NoteSummary], indices: &[usize], total: usize) {
    if indices.is_empty() {
        return;
    }
    println!();
    println!("{}", "── Notes ──────────────────────────────".cyan());
    for &i in indices {
        let n = &notes[i];
        let sentiment = if n.sentiment < 0.0 {
            format!("[{:.1}]", n.sentiment).red().to_string()
        } else if n.sentiment > 0.0 {
            format!("[+{:.1}]", n.sentiment).green().to_string()
        } else {
            "[0.0]".dimmed().to_string()
        };
        let text = if n.text.len() > 80 {
            format!("{}...", &n.text[..n.text.floor_char_boundary(77)])
        } else {
            n.text.clone()
        };
        println!("  {} {}", sentiment, text.dimmed());
    }
    if indices.len() < total {
        println!(
            "  {}",
            format!("({} more notes truncated)", total - indices.len()).dimmed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands::index_pack;

    #[test]
    fn test_waterfall_allocation_percentages() {
        // Notes takes the remainder, so the explicit weights must sum to ≤1.0
        let total = WATERFALL_SCOUT + WATERFALL_CODE + WATERFALL_IMPACT + WATERFALL_PLACEMENT;
        assert!(
            (0.9..=1.0).contains(&total),
            "Explicit budget weights must leave a small remainder for notes, got {total}"
        );
    }

    #[test]
    fn test_waterfall_section_budgets() {
        let budget: usize = 1000;
        let scout = (budget as f64 * WATERFALL_SCOUT) as usize;
        let code = (budget as f64 * WATERFALL_CODE) as usize;
        let impact = (budget as f64 * WATERFALL_IMPACT) as usize;
        let placement = (budget as f64 * WATERFALL_PLACEMENT) as usize;
        let notes = budget - scout - code - impact - placement;
        assert_eq!(scout + code + impact + placement + notes, budget);
    }

    #[test]
    fn test_index_pack_empty() {
        let (indices, used) = index_pack(&[], 100, 0, |_| 1.0);
        assert!(indices.is_empty());
        assert_eq!(used, 0);
    }

    #[test]
    fn test_index_pack_all_fit() {
        let counts = vec![10, 20, 30];
        let (indices, used) = index_pack(&counts, 100, 0, |_| 1.0);
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(used, 60);
    }

    #[test]
    fn test_index_pack_budget_forces_selection() {
        let counts = vec![10, 10, 10, 10, 10];
        // Scores: 0=1.0, 1=5.0, 2=3.0, 3=4.0, 4=2.0
        // Budget 30 fits 3 items → picks indices 1, 3, 2 (by score), sorted → [1, 2, 3]
        let (indices, used) = index_pack(&counts, 30, 0, |i| match i {
            0 => 1.0,
            1 => 5.0,
            2 => 3.0,
            3 => 4.0,
            4 => 2.0,
            _ => 0.0,
        });
        assert_eq!(indices.len(), 3);
        assert_eq!(used, 30);
        assert!(indices.contains(&1));
        assert!(indices.contains(&2));
        assert!(indices.contains(&3));
    }

    #[test]
    fn test_index_pack_preserves_order() {
        let counts = vec![10, 10, 10];
        // Budget fits 2 → picks highest score items, returned in original order
        let (indices, _) = index_pack(&counts, 20, 0, |i| match i {
            0 => 1.0,
            1 => 3.0,
            2 => 2.0,
            _ => 0.0,
        });
        assert_eq!(indices, vec![1, 2]); // original order, not score order
    }

    #[test]
    fn test_index_pack_always_includes_one() {
        let counts = vec![100]; // over budget
        let (indices, used) = index_pack(&counts, 10, 0, |_| 1.0);
        assert_eq!(indices, vec![0]);
        assert_eq!(used, 100);
    }

    #[test]
    fn test_index_pack_with_overhead() {
        let counts = vec![10, 10, 10];
        // With overhead 35, each item costs 45. Budget 100 fits 2 (90), not 3 (135)
        let (indices, used) = index_pack(&counts, 100, 35, |_| 1.0);
        assert_eq!(indices.len(), 2);
        assert_eq!(used, 90);
    }

    // TC-8: index_pack with zero budget returns nothing
    #[test]
    fn test_index_pack_zero_budget() {
        let counts = vec![10, 20, 30];
        let (indices, used) = index_pack(&counts, 0, 0, |_| 1.0);
        assert!(indices.is_empty());
        assert_eq!(used, 0);
    }

    // TC-11: Waterfall surplus forwarding — verify unused budget flows to next section
    #[test]
    fn test_waterfall_surplus_forwarding() {
        let budget: usize = 1000;
        let weights = [
            WATERFALL_SCOUT,
            WATERFALL_CODE,
            WATERFALL_IMPACT,
            WATERFALL_PLACEMENT,
        ];
        let base_budgets: Vec<usize> = weights
            .iter()
            .map(|w| (budget as f64 * w) as usize)
            .collect();

        // Scenario: scout uses only 50 of its 150 budget → 100 surplus flows to code
        let scout_budget = base_budgets[0]; // 150
        let scout_used = 50;
        let code_budget_with_surplus =
            (base_budgets[1] + scout_budget.saturating_sub(scout_used)).min(budget - scout_used);
        // Code gets 500 base + 100 surplus = 600 (capped by remaining = 950)
        assert_eq!(code_budget_with_surplus, 600);

        // Scenario: code uses all 600 → 0 surplus to impact
        let code_used = 600;
        let impact_budget_with_surplus = (base_budgets[2]
            + code_budget_with_surplus.saturating_sub(code_used))
        .min(budget - scout_used - code_used);
        // Impact gets 150 base + 0 surplus = 150 (remaining = 350)
        assert_eq!(impact_budget_with_surplus, 150);

        // Scenario: impact uses only 30 → 120 surplus flows to placement
        let impact_used = 30;
        let placement_budget_with_surplus = (base_budgets[3]
            + impact_budget_with_surplus.saturating_sub(impact_used))
        .min(budget - scout_used - code_used - impact_used);
        // Placement gets 100 base + 120 surplus = 220 (remaining = 320)
        assert_eq!(placement_budget_with_surplus, 220);

        // Notes gets remaining
        let placement_used = 80;
        let notes_budget = budget - scout_used - code_used - impact_used - placement_used;
        assert_eq!(notes_budget, 240);
    }

    // ── Typed output struct serialization tests ─────────────────────────────

    #[test]
    fn test_task_brief_output_field_names() {
        let output = TaskBriefOutput {
            files: vec!["src/lib.rs".into()],
            placements: vec!["src/lib.rs:10 (near foo)".into()],
            risk: vec![TaskBriefRisk {
                name: "bar".into(),
                risk_level: "High".into(),
            }],
            tests: vec!["test_bar".into()],
            summary: TaskBriefSummary {
                files: 1,
                functions: 2,
                high_risk: 1,
                tests: 1,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("files").is_some());
        assert!(json.get("placements").is_some());
        assert!(json.get("risk").is_some());
        assert!(json.get("tests").is_some());
        assert!(json.get("summary").is_some());
        assert_eq!(json["risk"][0]["name"], "bar");
        assert_eq!(json["risk"][0]["risk_level"], "High");
        assert_eq!(json["summary"]["files"], 1);
        assert_eq!(json["summary"]["functions"], 2);
        assert_eq!(json["summary"]["high_risk"], 1);
    }

    #[test]
    fn test_task_brief_output_empty() {
        let output = TaskBriefOutput {
            files: vec![],
            placements: vec![],
            risk: vec![],
            tests: vec![],
            summary: TaskBriefSummary {
                files: 0,
                functions: 0,
                high_risk: 0,
                tests: 0,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["files"].as_array().unwrap().len(), 0);
        assert_eq!(json["risk"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["files"], 0);
    }

    #[test]
    fn test_budgeted_scout_chunk_field_names() {
        let chunk = BudgetedScoutChunk {
            name: "foo".into(),
            chunk_type: "Function".into(),
            signature: "fn foo()".into(),
            line_start: 42,
            role: "modify".into(),
            caller_count: 3,
            test_count: 2,
            search_score: 0.95,
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["name"], "foo");
        assert_eq!(json["line_start"], 42);
        assert!(json.get("line").is_none()); // normalized away
        assert_eq!(json["chunk_type"], "Function");
        assert_eq!(json["role"], "modify");
    }

    #[test]
    fn test_budgeted_scout_group_serialization() {
        let group = BudgetedScoutGroup {
            file: "src/lib.rs".into(),
            relevance_score: 0.85,
            is_stale: false,
            chunks: vec![],
        };
        let json = serde_json::to_value(&group).unwrap();
        assert_eq!(json["file"], "src/lib.rs");
        assert_eq!(json["is_stale"], false);
        assert!(json["chunks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_budgeted_scout_output_notes_omitted_when_none() {
        let output = BudgetedScoutOutput {
            file_groups: vec![],
            summary: BudgetedScoutSummary {
                total_files: 0,
                total_functions: 0,
                untested_count: 0,
                stale_count: 0,
            },
            relevant_notes: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("relevant_notes").is_none());
    }

    #[test]
    fn test_budgeted_scout_output_notes_present_when_some() {
        let output = BudgetedScoutOutput {
            file_groups: vec![],
            summary: BudgetedScoutSummary {
                total_files: 0,
                total_functions: 0,
                untested_count: 0,
                stale_count: 0,
            },
            relevant_notes: Some(vec![BudgetedNoteEntry {
                text: "important note".into(),
                sentiment: -0.5,
                mentions: vec!["foo.rs".into()],
            }]),
        };
        let json = serde_json::to_value(&output).unwrap();
        let notes = json["relevant_notes"].as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["text"], "important note");
        assert_eq!(notes[0]["sentiment"], -0.5);
    }

    #[test]
    fn test_budgeted_note_entry_field_names() {
        let entry = BudgetedNoteEntry {
            text: "some note".into(),
            sentiment: 0.5,
            mentions: vec!["a.rs".into(), "b.rs".into()],
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["text"], "some note");
        assert_eq!(json["sentiment"], 0.5);
        assert_eq!(json["mentions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_budgeted_task_output_field_names() {
        let output = BudgetedTaskOutput {
            description: "test task".into(),
            scout: BudgetedScoutOutput {
                file_groups: vec![],
                summary: BudgetedScoutSummary {
                    total_files: 1,
                    total_functions: 2,
                    untested_count: 0,
                    stale_count: 0,
                },
                relevant_notes: None,
            },
            code: vec![],
            risk: vec![],
            tests: vec![],
            placement: vec![],
            summary: BudgetedTaskSummary {
                total_files: 1,
                total_functions: 2,
                modify_targets: 1,
                high_risk_count: 0,
                test_count: 3,
                stale_count: 0,
            },
            token_count: 150,
            token_budget: 300,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["description"], "test task");
        assert_eq!(json["token_count"], 150);
        assert_eq!(json["token_budget"], 300);
        assert_eq!(json["summary"]["modify_targets"], 1);
        assert_eq!(json["summary"]["test_count"], 3);
        assert!(json["code"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_budgeted_task_output_empty() {
        let output = BudgetedTaskOutput {
            description: String::new(),
            scout: BudgetedScoutOutput {
                file_groups: vec![],
                summary: BudgetedScoutSummary {
                    total_files: 0,
                    total_functions: 0,
                    untested_count: 0,
                    stale_count: 0,
                },
                relevant_notes: None,
            },
            code: vec![],
            risk: vec![],
            tests: vec![],
            placement: vec![],
            summary: BudgetedTaskSummary {
                total_files: 0,
                total_functions: 0,
                modify_targets: 0,
                high_risk_count: 0,
                test_count: 0,
                stale_count: 0,
            },
            token_count: 0,
            token_budget: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["token_count"], 0);
        assert!(json["scout"]["file_groups"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_build_task_brief_empty_result() {
        use std::path::PathBuf;
        let result = cqs::TaskResult {
            description: "empty".into(),
            scout: cqs::ScoutResult {
                file_groups: vec![],
                relevant_notes: vec![],
                summary: cqs::ScoutSummary {
                    total_files: 0,
                    total_functions: 0,
                    untested_count: 0,
                    stale_count: 0,
                },
            },
            code: vec![],
            risk: vec![],
            tests: vec![],
            placement: vec![],
            summary: cqs::TaskSummary {
                total_files: 0,
                total_functions: 0,
                modify_targets: 0,
                high_risk_count: 0,
                test_count: 0,
                stale_count: 0,
            },
        };
        let root = PathBuf::from("/tmp");
        let brief = build_task_brief(&result, &root);
        assert!(brief.files.is_empty());
        assert!(brief.risk.is_empty());
        assert!(brief.tests.is_empty());
        assert_eq!(brief.summary.files, 0);
    }
}
