//! CI command — pipeline analysis with gate logic

use anyhow::Result;

use cqs::ci::run_ci_analysis;
use cqs::ReviewResult;
use cqs::RiskLevel;

pub(crate) fn cmd_ci(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    from_stdin: bool,
    format: &crate::cli::OutputFormat,
    gate: &crate::cli::GateThreshold,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_ci", ?format, ?gate, ?max_tokens).entered();

    if matches!(format, crate::cli::OutputFormat::Mermaid) {
        anyhow::bail!("Mermaid output is not supported for ci — use text or json");
    }

    let json = matches!(format, crate::cli::OutputFormat::Json);
    let store = &ctx.store;
    let root = &ctx.root;

    // Get diff text
    let diff_text = if from_stdin {
        crate::cli::commands::read_stdin()?
    } else {
        crate::cli::commands::run_git_diff(base)?
    };

    // Run CI analysis
    let mut report = run_ci_analysis(store, &diff_text, root, *gate)?;

    // Apply token budget
    let token_count_used =
        max_tokens.map(|budget| apply_token_budget(&mut report.review, budget, json));

    if json {
        let mut output: serde_json::Value = serde_json::to_value(&report)?;
        if let Some(tokens) = token_count_used {
            output["token_count"] = serde_json::json!(tokens);
            output["token_budget"] = serde_json::json!(max_tokens.unwrap_or(0));
        }
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        display_ci_text(&report, root, token_count_used, max_tokens);
    }

    // Exit with gate code if failed
    if !report.gate.passed {
        std::process::exit(crate::cli::signal::ExitCode::GateFailed as i32);
    }

    Ok(())
}

/// Apply token budget by truncating callers and tests lists.
/// Reuses the same logic as review.rs — changed functions and risk summary
/// are always included, callers and tests are truncated.
/// Public entry point for batch mode to apply CI token budgeting.
pub(crate) fn apply_ci_token_budget(review: &mut ReviewResult, budget: usize) -> usize {
    apply_token_budget(review, budget, true)
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
    // P3 #121: gate the `.max(1)` floor on positive budget — true-zero must
    // produce zero, not one. Mirrors the same fix in `diff_review.rs`.
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

    // Dead code in diff — EH-V1.29-2: surface scan failures so operators
    // can distinguish "no dead code found" from "scan never ran". The gate
    // fails on scan failure; this message explains why.
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

    /// TC-HAP-V1.36-5 / P3: pin apply_ci_token_budget shape with json=true
    /// accounting (sibling test_apply_token_budget_truncates_when_over only
    /// covers json=false). Adds JSON_OVERHEAD_PER_RESULT to per-item cost,
    /// so the same budget fits fewer items.
    #[test]
    fn test_apply_ci_token_budget_truncates_callers_and_tests() {
        let mut review = make_review(50, 50);
        let used = apply_ci_token_budget(&mut review, 200);
        assert!(
            review.affected_callers.len() < 50 || review.affected_tests.len() < 50,
            "small budget must truncate at least one of callers/tests with json=true accounting"
        );
        assert!(used > 0);
    }

    #[test]
    fn test_apply_ci_token_budget_zero_produces_zero_items() {
        let mut review = make_review(5, 5);
        apply_ci_token_budget(&mut review, 0);
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
