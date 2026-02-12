//! Review command — comprehensive diff review context

use std::io::Read;

use anyhow::Result;

use cqs::review::ReviewResult;
use cqs::RiskLevel;

pub(crate) fn cmd_review(
    _cli: &crate::cli::Cli,
    base: Option<&str>,
    from_stdin: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_review").entered();
    let (store, root, _) = crate::cli::open_project_store()?;

    // 1. Get diff text
    let diff_text = if from_stdin {
        read_stdin()?
    } else {
        run_git_diff(base)?
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
        Some(review) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&review)?);
            } else {
                display_review_text(&review, &root);
            }
        }
    }

    Ok(())
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

fn read_stdin() -> Result<String> {
    const MAX_STDIN_SIZE: usize = 50 * 1024 * 1024; // 50 MB
    let mut buf = String::new();
    std::io::stdin()
        .take(MAX_STDIN_SIZE as u64 + 1)
        .read_to_string(&mut buf)?;
    if buf.len() > MAX_STDIN_SIZE {
        anyhow::bail!("stdin input exceeds 50 MB limit");
    }
    Ok(buf)
}

fn run_git_diff(base: Option<&str>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-pager", "diff", "--no-color"]);
    if let Some(b) = base {
        if b.starts_with('-') {
            anyhow::bail!("Invalid base ref '{}': must not start with '-'", b);
        }
        cmd.arg(b);
    }

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run 'git diff': {}. Is git installed?", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn display_review_text(review: &ReviewResult, _root: &std::path::Path) {
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
    println!(
        "{} {} (high: {}, medium: {}, low: {})",
        "Risk:".bold(),
        colored_risk,
        review.risk_summary.high,
        review.risk_summary.medium,
        review.risk_summary.low,
    );

    // Stale warning
    if let Some(ref stale) = review.stale_warning {
        println!();
        println!(
            "{} Index is stale for {} file(s):",
            "Warning:".yellow().bold(),
            stale.len()
        );
        for f in stale {
            println!("  {}", f);
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
        println!(
            "  {} {} ({}:{}) — {} callers, {} tests",
            risk_indicator, f.name, f.file, f.line_start, f.risk.caller_count, f.risk.test_count,
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
                c.name, c.file, c.line, c.call_line
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
                t.name, t.file, t.line, t.via, t.call_depth
            );
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
