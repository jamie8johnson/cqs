//! Review command — comprehensive diff review context

use anyhow::Result;

use cqs::review::ReviewResult;
use cqs::RiskLevel;

pub(crate) fn cmd_review(
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
    let (store, root, _) = crate::cli::open_project_store()?;

    // 1. Get diff text
    let diff_text = if from_stdin {
        super::read_stdin()?
    } else {
        super::run_git_diff(base)?
    };

    // 2. Run review
    let result = cqs::review::review_diff(&store, &diff_text, &root)?;

    match result {
        None => {
            if json {
                println!("{}", serde_json::to_string_pretty(&empty_review_json())?);
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
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                display_review_text(&review, &root, token_count_used, max_tokens);
            }
        }
    }

    Ok(())
}

/// Apply token budget by truncating callers and tests lists.
///
/// Changed functions and risk summary are always included (small, essential).
/// Callers and tests are the variable-size sections that get truncated.
/// `json` adds per-item overhead for JSON field names and structure tokens.
/// Returns total token count used.
fn apply_token_budget(review: &mut ReviewResult, budget: usize, json: bool) -> usize {
    let _span = tracing::info_span!("review_token_budget", budget, json).entered();

    // JSON wrapping adds ~35 tokens per item (field names, paths, metadata)
    let json_per_item = if json {
        super::JSON_OVERHEAD_PER_RESULT
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

    // Fit callers within remaining budget (prioritize callers over tests)
    let callers_budget = (budget.saturating_sub(used)) * 2 / 3; // 2/3 of remaining for callers
    let max_callers = callers_budget / tokens_per_caller;
    let original_callers = review.affected_callers.len();
    if review.affected_callers.len() > max_callers {
        review.affected_callers.truncate(max_callers.max(1));
    }
    used += review.affected_callers.len() * tokens_per_caller;

    // Fit tests within remaining budget
    let tests_budget = budget.saturating_sub(used);
    let max_tests = tests_budget / tokens_per_test;
    let original_tests = review.affected_tests.len();
    if review.affected_tests.len() > max_tests {
        review.affected_tests.truncate(max_tests.max(1));
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
            "Output truncated to ~{} tokens (budget: {}). {} callers, {} tests omitted.",
            used, budget, truncated_callers, truncated_tests
        ));
    }

    used
}

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
            f.file,
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
