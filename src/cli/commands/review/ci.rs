//! CI command — pipeline analysis with gate logic

use anyhow::Result;

use cqs::ci::{run_ci_analysis, run_ci_analysis_overlay};
use cqs::ReviewResult;
use cqs::RiskLevel;

// ---------------------------------------------------------------------------
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`ci_core`]. The diff text + gate threshold are supplied by the
/// adapter (which owns I/O and the `GateThreshold` parse); `tokens` folds the
/// request-scoped budget in.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct CiArgs {
    /// Token budget for the embedded review (truncates callers/tests lists).
    #[serde(default)]
    pub tokens: Option<usize>,
}

/// Typed output for `cqs ci`. Flattens the lib [`cqs::ci::CiReport`] and adds
/// the optional token-budget telemetry the adapters previously spliced inline.
/// THE schema — both surfaces serialize this.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CiOutput {
    #[serde(flatten)]
    pub report: cqs::ci::CiReport,
    /// Estimated tokens used after budgeting (present only when `--tokens` set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    /// The requested token budget (present only when `--tokens` set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}

/// Surface-agnostic core for `cqs ci`. Runs the CI analysis (review + dead
/// code + gate) on `diff_text`, applies the optional token budget, and returns
/// the typed [`CiOutput`]. The gate `passed` flag rides along in the report;
/// the *exit-code* reaction to a failed gate is adapter-owned (the CLI exits
/// non-zero, the daemon just reports). Both surfaces drive this so the CI
/// schema + budgeting live in one place.
///
/// Plain entry point: no worktree overlay. The full logic lives in
/// [`ci_overlay`]; this delegates with `None` (participation discarded), so the
/// CLI / tests are byte-unchanged.
pub(crate) fn ci_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &std::path::Path,
    diff_text: &str,
    gate: cqs::ci::GateThreshold,
    args: &CiArgs,
) -> Result<CiOutput> {
    Ok(ci_overlay(store, root, diff_text, gate, args, None)?.0)
}

/// Overlay-aware core for `cqs ci`. Identical to [`ci_core`] when
/// `overlay` is `None`. When `Some`, BOTH bundled sections reflect the worktree
/// delta via [`run_ci_analysis_overlay`]: the embedded review's
/// `affected_callers` (the same mask+union `cqs review` applies) and the
/// `dead_in_diff` set (recomputed over the merged caller graph, the same merge
/// `cqs dead` applies).
///
/// Returns `(CiOutput, overlay_participated)`. Participation is `true` iff the
/// review-overlay merge OR the dead-overlay merge consulted the delta for THIS
/// diff. The empty-diff early-return (no indexed function) and every no-overlay
/// call report `false`.
///
/// Composite-marker honesty: `cqs ci` bundles a `"callers-only"` review and a
/// `"full"` dead component. The weakest component bounds the claim, so the
/// daemon adapter emits the honest combined `_meta.overlay_graph = "callers-only"`
/// marker — NOT `"full"` — when this returns `participated == true`.
pub(crate) fn ci_overlay(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &std::path::Path,
    diff_text: &str,
    gate: cqs::ci::GateThreshold,
    args: &CiArgs,
    overlay: Option<&cqs::worktree_overlay::WorktreeOverlay>,
) -> Result<(CiOutput, bool)> {
    let _span = tracing::info_span!(
        "ci_core",
        ?gate,
        tokens = ?args.tokens,
        overlay = overlay.is_some()
    )
    .entered();

    let (mut report, participated) =
        run_ci_analysis_overlay(store, diff_text, root, gate, overlay)?;
    let token_count = args
        .tokens
        .map(|budget| apply_token_budget(&mut report.review, budget, true));

    Ok((
        CiOutput {
            report,
            token_count,
            token_budget: args.tokens,
        },
        participated,
    ))
}

pub(crate) fn cmd_ci(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    from_stdin: bool,
    format: &crate::cli::OutputFormat,
    gate: &crate::cli::GateThreshold,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_ci", ?format, ?gate, ?max_tokens).entered();

    // Exhaustive match — Mermaid bails, Json/Text drive a boolean used
    // downstream by `apply_token_budget`. A future `OutputFormat` variant
    // fails to compile here until it adds an arm.
    let json = match format {
        crate::cli::OutputFormat::Mermaid => {
            anyhow::bail!("Mermaid output is not supported for ci — use text or json");
        }
        crate::cli::OutputFormat::Json => true,
        crate::cli::OutputFormat::Text => false,
    };
    let store = &ctx.store;
    let root = &ctx.root;

    // Get diff text — adapter owns I/O (CLI supports stdin; daemon git only).
    let diff_text = if from_stdin {
        crate::cli::commands::read_stdin()?
    } else {
        crate::cli::commands::run_git_diff(base)?
    };

    // The gate `passed` flag drives the process exit code below, so both
    // branches surface it. JSON goes through the shared core (same as the
    // daemon); text budgets with json=false accounting and renders the
    // dashboard.
    let gate_passed = if json {
        let output = ci_core(
            store,
            root,
            &diff_text,
            *gate,
            &CiArgs { tokens: max_tokens },
        )?;
        let gate_passed = output.report.gate.passed;
        crate::cli::json_envelope::emit_json(&output)?;
        gate_passed
    } else {
        let mut report = run_ci_analysis(store, &diff_text, root, *gate)?;
        let token_count_used =
            max_tokens.map(|budget| apply_token_budget(&mut report.review, budget, false));
        display_ci_text(&report, root, token_count_used, max_tokens);
        report.gate.passed
    };

    // Exit with gate code if failed (adapter-owned reaction; the core only
    // reports the gate result).
    if !gate_passed {
        std::process::exit(crate::cli::signal::ExitCode::GateFailed as i32);
    }

    Ok(())
}

fn apply_token_budget(review: &mut ReviewResult, budget: usize, json: bool) -> usize {
    let _span = tracing::info_span!("ci_token_budget", budget, json).entered();

    let json_per_item = if json {
        crate::cli::commands::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };

    let tokens_per_caller: usize = 15 + json_per_item;
    let tokens_per_test: usize = 18 + json_per_item;
    let tokens_per_function: usize = 12 + json_per_item;
    let tokens_per_note: usize = 20 + json_per_item;
    const BASE_OVERHEAD: usize = 50; // gate + risk header + section headers + dead code

    let mut used = BASE_OVERHEAD;

    // Changed functions are always included
    used += review.changed_functions.len() * tokens_per_function;

    // Notes are always included
    used += review.relevant_notes.len() * tokens_per_note;

    // Fit callers within remaining budget (2/3 for callers).
    //
    // Gate the `.max(1)` floor on positive budget — a true-zero budget must
    // produce zero items, not one. Same rule as in `diff_review.rs`.
    let callers_budget = (budget.saturating_sub(used)) * 2 / 3;
    let max_callers = callers_budget / tokens_per_caller;
    let original_callers = review.affected_callers.len();
    if review.affected_callers.len() > max_callers {
        let floor = if budget > 0 && callers_budget > 0 {
            1
        } else {
            0
        };
        review.affected_callers.truncate(max_callers.max(floor));
    }
    used += review.affected_callers.len() * tokens_per_caller;

    // Fit tests within remaining budget
    let tests_budget = budget.saturating_sub(used);
    let max_tests = tests_budget / tokens_per_test;
    let original_tests = review.affected_tests.len();
    if review.affected_tests.len() > max_tests {
        let floor = if budget > 0 && tests_budget > 0 { 1 } else { 0 };
        review.affected_tests.truncate(max_tests.max(floor));
    }
    used += review.affected_tests.len() * tokens_per_test;

    if review.affected_callers.len() < original_callers
        || review.affected_tests.len() < original_tests
    {
        let truncated_callers = original_callers - review.affected_callers.len();
        let truncated_tests = original_tests - review.affected_tests.len();
        tracing::info!(
            budget,
            used,
            truncated_callers,
            truncated_tests,
            "Token-budgeted CI review"
        );
        review.warnings.push(format!(
            "Output truncated to ~{} tokens (budget: {}). {} callers, {} tests omitted (min 1 caller + 1 test guaranteed).",
            used, budget, truncated_callers, truncated_tests
        ));
    }

    used
}

fn display_ci_text(
    report: &cqs::ci::CiReport,
    _root: &std::path::Path,
    token_count_used: Option<usize>,
    max_tokens: Option<usize>,
) {
    use colored::Colorize;

    let review = &report.review;

    // Gate result header
    if report.gate.passed {
        println!(
            "{} {} [threshold: {}]",
            "Gate:".bold(),
            "PASS".green().bold(),
            format!("{:?}", report.gate.threshold).to_lowercase(),
        );
    } else {
        println!(
            "{} {} [threshold: {}]",
            "Gate:".bold(),
            "FAIL".red().bold(),
            format!("{:?}", report.gate.threshold).to_lowercase(),
        );
        for reason in &report.gate.reasons {
            println!("  {}", reason);
        }
    }

    // Risk summary
    let risk_color = match review.risk_summary.overall {
        RiskLevel::High => "red",
        RiskLevel::Medium => "yellow",
        RiskLevel::Low => "green",
    };
    let overall_str = format!("{}", review.risk_summary.overall);
    let colored_risk = match risk_color {
        "red" => overall_str.red().bold().to_string(),
        "yellow" => overall_str.yellow().bold().to_string(),
        _ => overall_str.green().bold().to_string(),
    };
    let token_info = match (token_count_used, max_tokens) {
        (Some(used), Some(budget)) => format!(" [{}/{}T]", used, budget),
        _ => String::new(),
    };
    println!();
    println!(
        "{} {} (high: {}, medium: {}, low: {}){}",
        "Risk:".bold(),
        colored_risk,
        review.risk_summary.high,
        review.risk_summary.medium,
        review.risk_summary.low,
        token_info,
    );

    // Changed functions
    if !review.changed_functions.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Changed functions".bold(),
            review.changed_functions.len()
        );
        for f in &review.changed_functions {
            let risk_indicator = match f.risk.risk_level {
                RiskLevel::High => format!("[{}]", "HIGH".red()),
                RiskLevel::Medium => format!("[{}]", "MED".yellow()),
                RiskLevel::Low => format!("[{}]", "LOW".green()),
            };
            println!(
                "  {} {} ({}:{}) — {} callers, {} tests",
                risk_indicator,
                f.name,
                f.file.display(),
                f.line_start,
                f.risk.caller_count,
                f.risk.test_count,
            );
        }
    }

    // Dead code in diff — surface scan failures so operators can distinguish
    // "no dead code found" from "scan never ran". The gate fails on scan
    // failure; this message explains why.
    if !report.dead_scan_ok {
        println!();
        println!(
            "{} Dead-code scan failed — results not available. See logs for error.",
            "Warning:".red().bold(),
        );
    } else if !report.dead_in_diff.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Dead code in diff".yellow().bold(),
            report.dead_in_diff.len()
        );
        for d in &report.dead_in_diff {
            println!(
                "  {} {}:{} [{}]",
                d.name,
                d.file.display(),
                d.line_start,
                d.confidence.as_str()
            );
        }
    }

    // Tests to re-run
    if review.affected_tests.is_empty() {
        println!();
        println!("{}", "No affected tests.".dimmed());
    } else {
        println!();
        println!(
            "{} ({}):",
            "Tests to re-run".yellow(),
            review.affected_tests.len()
        );
        for t in &review.affected_tests {
            println!(
                "  {} ({}:{}) [via {}, depth {}]",
                t.name,
                t.file.display(),
                t.line,
                t.via,
                t.call_depth
            );
        }
    }

    // Callers
    if !review.affected_callers.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Affected callers".cyan(),
            review.affected_callers.len()
        );
        for c in &review.affected_callers {
            println!(
                "  {} ({}:{}, call at line {})",
                c.name,
                c.file.display(),
                c.line,
                c.call_line
            );
        }
    }

    // Stale warning
    if let Some(ref stale) = review.stale_warning {
        eprintln!();
        eprintln!(
            "{} Index is stale for {} file(s):",
            "Warning:".yellow().bold(),
            stale.len()
        );
        for f in stale {
            eprintln!("  {}", f);
        }
    }

    // Notes
    if !review.relevant_notes.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Relevant notes".magenta(),
            review.relevant_notes.len()
        );
        for n in &review.relevant_notes {
            let sentiment_str = match n.sentiment {
                s if s <= -0.5 => "⚠".to_string(),
                s if s >= 0.5 => "✓".to_string(),
                _ => "·".to_string(),
            };
            println!(
                "  {} {} ({})",
                sentiment_str,
                n.text,
                n.matching_files.join(", ")
            );
        }
    }

    // Warnings
    if !review.warnings.is_empty() {
        println!();
        for w in &review.warnings {
            println!("{} {}", "Warning:".yellow().bold(), w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::{CallerDetail, DiffTestInfo, ReviewedFunction, RiskLevel, RiskScore, RiskSummary};
    use std::path::PathBuf;

    /// Build a synthetic ReviewResult mirroring the diff_review.rs helper.
    /// Pinned here so the ci.rs test doesn't import a private helper.
    fn make_review(num_callers: usize, num_tests: usize) -> ReviewResult {
        let callers: Vec<CallerDetail> = (0..num_callers)
            .map(|i| CallerDetail {
                name: format!("caller_{}", i),
                file: PathBuf::from(format!("src/c{}.rs", i)),
                line: (i as u32) + 1,
                call_line: (i as u32) + 10,
                snippet: None,
                edge_kind: cqs::parser::CallEdgeKind::Call,
            })
            .collect();
        let tests: Vec<DiffTestInfo> = (0..num_tests)
            .map(|i| DiffTestInfo {
                name: format!("test_{}", i),
                file: PathBuf::from(format!("tests/t{}.rs", i)),
                line: (i as u32) + 1,
                via: "direct".into(),
                call_depth: 1,
            })
            .collect();
        ReviewResult {
            changed_functions: vec![ReviewedFunction {
                name: "target_fn".into(),
                file: PathBuf::from("src/lib.rs"),
                line_start: 42,
                risk: RiskScore {
                    caller_count: num_callers,
                    test_count: num_tests,
                    test_ratio: 1.0,
                    risk_level: RiskLevel::Low,
                    blast_radius: RiskLevel::Low,
                    score: 0.0,
                },
            }],
            affected_callers: callers,
            affected_tests: tests,
            relevant_notes: vec![],
            risk_summary: RiskSummary {
                high: 0,
                medium: 0,
                low: 1,
                overall: RiskLevel::Low,
            },
            stale_warning: None,
            warnings: vec![],
        }
    }

    /// `CiArgs` deserializes from a wire/MCP object; `tokens` defaults to None.
    #[test]
    fn ci_args_minimal_deserialize() {
        let def: CiArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(def.tokens.is_none());
        let with: CiArgs = serde_json::from_value(serde_json::json!({"tokens": 800})).unwrap();
        assert_eq!(with.tokens, Some(800));
    }

    /// Pin the CI token-budget shape with json=true accounting (sibling
    /// test_apply_token_budget_truncates_when_over only covers json=false).
    /// json=true adds JSON_OVERHEAD_PER_RESULT to per-item cost, so the same
    /// budget fits fewer items. This is the budgeting `ci_core` applies.
    #[test]
    fn test_apply_ci_token_budget_truncates_callers_and_tests() {
        let mut review = make_review(50, 50);
        let used = apply_token_budget(&mut review, 200, true);
        assert!(
            review.affected_callers.len() < 50 || review.affected_tests.len() < 50,
            "small budget must truncate at least one of callers/tests with json=true accounting"
        );
        assert!(used > 0);
    }

    #[test]
    fn test_apply_ci_token_budget_zero_produces_zero_items() {
        let mut review = make_review(5, 5);
        apply_token_budget(&mut review, 0, true);
        assert_eq!(
            review.affected_callers.len(),
            0,
            "budget=0 must drop all callers"
        );
        assert_eq!(
            review.affected_tests.len(),
            0,
            "budget=0 must drop all tests"
        );
    }
}
