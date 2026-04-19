//! Review command — comprehensive diff review context

use anyhow::Result;

use cqs::ReviewResult;
use cqs::RiskLevel;

pub(crate) fn cmd_review(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    from_stdin: bool,
    format: &crate::cli::OutputFormat,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_review", ?format, ?max_tokens).entered();

    if matches!(format, crate::cli::OutputFormat::Mermaid) {
        anyhow::bail!("Mermaid output is not supported for review — use text or json");
    }

    let json = matches!(format, crate::cli::OutputFormat::Json);
    let store = &ctx.store;
    let root = &ctx.root;

    // 1. Get diff text
    let diff_text = if from_stdin {
        crate::cli::commands::read_stdin()?
    } else {
        crate::cli::commands::run_git_diff(base)?
    };

    // 2. Run review
    let result = cqs::review_diff(store, &diff_text, root)?;

    match result {
        None => {
            if json {
                crate::cli::json_envelope::emit_json(&empty_review_json())?;
            } else {
                println!("No indexed functions affected by this diff.");
            }
        }
        Some(mut review) => {
            // Apply token budget: truncate callers and tests lists to fit
            let token_count_used =
                max_tokens.map(|budget| apply_token_budget(&mut review, budget, json));

            if json {
                let mut output: serde_json::Value = serde_json::to_value(&review)?;
                if let Some(tokens) = token_count_used {
                    output["token_count"] = serde_json::json!(tokens);
                    output["token_budget"] = serde_json::json!(max_tokens.unwrap_or(0));
                }
                crate::cli::json_envelope::emit_json(&output)?;
            } else {
                display_review_text(&review, root, token_count_used, max_tokens);
            }
        }
    }

    Ok(())
}

/// Apply token budget by truncating callers and tests lists.
/// Changed functions and risk summary are always included (small, essential).
/// Callers and tests are the variable-size sections that get truncated.
/// `json` adds per-item overhead for JSON field names and structure tokens.
/// Returns total token count used.
/// Public entry point for batch mode to apply token budgeting to review output.
pub(crate) fn apply_token_budget_public(
    review: &mut ReviewResult,
    budget: usize,
    json: bool,
) -> usize {
    apply_token_budget(review, budget, json)
}

fn apply_token_budget(review: &mut ReviewResult, budget: usize, json: bool) -> usize {
    let _span = tracing::info_span!("review_token_budget", budget, json).entered();

    // JSON wrapping adds ~35 tokens per item (field names, paths, metadata)
    let json_per_item = if json {
        crate::cli::commands::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };

    // Estimate tokens per item (~15 tokens per caller/test line in text output)
    let tokens_per_caller: usize = 15 + json_per_item;
    let tokens_per_test: usize = 18 + json_per_item;
    let tokens_per_function: usize = 12 + json_per_item;
    let tokens_per_note: usize = 20 + json_per_item;
    const BASE_OVERHEAD: usize = 30; // risk header, section headers, etc.

    let mut used = BASE_OVERHEAD;

    // Changed functions are always included (essential for review)
    used += review.changed_functions.len() * tokens_per_function;

    // Notes are always included (small, high value)
    used += review.relevant_notes.len() * tokens_per_note;

    // Fit callers within remaining budget (prioritize callers over tests).
    //
    // P3 #121: gate the `.max(1)` floor on a positive budget so a true-zero
    // budget produces zero callers/tests. Previously the floor always added
    // at least one item, overshooting tight budgets by ~50 tokens with no
    // way for the caller to shrink to nothing.
    let callers_budget = (budget.saturating_sub(used)) * 2 / 3; // 2/3 of remaining for callers
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
            "Token-budgeted review"
        );
        review.warnings.push(format!(
            "Output truncated to ~{} tokens (budget: {}). {} callers, {} tests omitted (min 1 caller + 1 test guaranteed).",
            used, budget, truncated_callers, truncated_tests
        ));
    }

    used
}

/// Creates and returns a JSON object representing an empty code review with no findings.
/// This function constructs a default review structure containing empty arrays for changed functions, affected callers, and affected tests, along with empty risk assessments and a null stale warning field.
/// # Returns
/// A `serde_json::Value` containing a JSON object with the following fields:
/// - `changed_functions`: empty array
/// - `affected_callers`: empty array
/// - `affected_tests`: empty array
/// - `relevant_notes`: empty array
/// - `risk_summary`: object with zero counts for high, medium, and low risk items, and overall risk set to "low"
/// - `stale_warning`: null value
fn empty_review_json() -> serde_json::Value {
    serde_json::json!({
        "changed_functions": [],
        "affected_callers": [],
        "affected_tests": [],
        "relevant_notes": [],
        "risk_summary": { "high": 0, "medium": 0, "low": 0, "overall": "low" },
        "stale_warning": null
    })
}

fn display_review_text(
    review: &ReviewResult,
    _root: &std::path::Path,
    token_count_used: Option<usize>,
    max_tokens: Option<usize>,
) {
    use colored::Colorize;

    // Risk summary header
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
    println!(
        "{} {} (high: {}, medium: {}, low: {}){}",
        "Risk:".bold(),
        colored_risk,
        review.risk_summary.high,
        review.risk_summary.medium,
        review.risk_summary.low,
        token_info,
    );

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

    // Changed functions with risk
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
        let blast_info = if f.risk.blast_radius != f.risk.risk_level {
            format!(", blast radius: {}", f.risk.blast_radius)
        } else {
            String::new()
        };
        println!(
            "  {} {} ({}:{}) — {} callers, {} tests{}",
            risk_indicator,
            f.name,
            f.file.display(),
            f.line_start,
            f.risk.caller_count,
            f.risk.test_count,
            blast_info,
        );
    }

    // Callers
    if review.affected_callers.is_empty() {
        println!();
        println!("{}", "No affected callers.".dimmed());
    } else {
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

    // Tests
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

    // Warnings
    if !review.warnings.is_empty() {
        println!();
        for w in &review.warnings {
            println!("{} {}", "Warning:".yellow().bold(), w);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::{CallerDetail, DiffTestInfo, ReviewedFunction, RiskLevel, RiskScore, RiskSummary};
    use std::path::PathBuf;

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
                    test_ratio: if num_callers > 0 {
                        (num_tests as f32 / num_callers as f32).min(1.0)
                    } else {
                        1.0
                    },
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

    #[test]
    fn test_apply_token_budget_preserves_when_fits() {
        let mut review = make_review(3, 3);
        let used = apply_token_budget(&mut review, 5000, false);

        assert_eq!(
            review.affected_callers.len(),
            3,
            "All callers should be preserved within budget"
        );
        assert_eq!(
            review.affected_tests.len(),
            3,
            "All tests should be preserved within budget"
        );
        assert!(review.warnings.is_empty(), "No truncation warning expected");
        assert!(used > 0, "Token count should be positive");
    }

    #[test]
    fn test_apply_token_budget_truncates_when_over() {
        let mut review = make_review(100, 100);
        // Tiny budget: base overhead (30) + 1 function (12) = 42 tokens, leaving very little
        let budget = 100;
        let used = apply_token_budget(&mut review, budget, false);

        assert!(
            review.affected_callers.len() < 100,
            "Callers should be truncated, got {}",
            review.affected_callers.len()
        );
        assert!(
            review.affected_tests.len() < 100,
            "Tests should be truncated, got {}",
            review.affected_tests.len()
        );
        // At least 1 caller and 1 test guaranteed by the max(1) logic when budget > 0
        assert!(
            !review.affected_callers.is_empty(),
            "At least 1 caller guaranteed when budget > 0"
        );
        assert!(
            !review.affected_tests.is_empty(),
            "At least 1 test guaranteed when budget > 0"
        );
        assert!(
            !review.warnings.is_empty(),
            "Should have a truncation warning"
        );
        assert!(
            used <= budget + 50,
            "Used tokens ({used}) should be near budget ({budget})"
        );
    }

    /// P3 #121: a true-zero budget must produce zero callers/tests, not one.
    /// Previously the `.max(1)` floor unconditionally added at least one
    /// caller and one test, overshooting the requested budget by ~50 tokens.
    /// The fix gates the floor on a positive budget.
    #[test]
    fn test_apply_token_budget_zero_produces_zero_items() {
        let mut review = make_review(10, 10);
        let used = apply_token_budget(&mut review, 0, false);

        assert!(
            review.affected_callers.is_empty(),
            "callers must be empty when budget = 0, got {}",
            review.affected_callers.len()
        );
        assert!(
            review.affected_tests.is_empty(),
            "tests must be empty when budget = 0, got {}",
            review.affected_tests.len()
        );
        // BASE_OVERHEAD + changed_functions still count toward `used`, but the
        // variable-size sections must contribute zero.
        assert!(
            used >= 30 && used < 100,
            "used = {used} should reflect base overhead only"
        );
    }
}
