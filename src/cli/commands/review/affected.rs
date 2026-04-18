//! Affected command — what functions, callers, and tests are affected by a diff
//!
//! Combines `parse_unified_diff`, `map_hunks_to_functions`, `impact()`, and
//! `test_map()` into a single risk-scored report.

use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use cqs::{
    analyze_diff_impact, diff_impact_to_json, map_hunks_to_functions, parse_unified_diff,
    rel_display, DiffImpactResult, RiskLevel,
};

/// Risk label for text display
fn risk_label(level: &RiskLevel) -> colored::ColoredString {
    match level {
        RiskLevel::High => "HIGH".red().bold(),
        RiskLevel::Medium => "MEDIUM".yellow(),
        RiskLevel::Low => "LOW".green(),
    }
}

pub(crate) fn cmd_affected(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_affected").entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // 1. Get diff text
    let diff_text = crate::cli::commands::run_git_diff(base)?;

    // 2. Parse hunks
    let hunks = parse_unified_diff(&diff_text);
    if hunks.is_empty() {
        if json {
            crate::cli::json_envelope::emit_json(&empty_affected_json())?;
        } else {
            println!("No changes detected.");
        }
        return Ok(());
    }

    // 3. Map hunks to functions
    let changed = map_hunks_to_functions(store, &hunks);
    if changed.is_empty() {
        if json {
            crate::cli::json_envelope::emit_json(&empty_affected_json())?;
        } else {
            println!("No indexed functions affected by this diff.");
        }
        return Ok(());
    }

    // 4. Analyze impact (callers + tests + risk)
    let result = analyze_diff_impact(store, changed, root)?;

    // 5. Display
    if json {
        let mut json_val = diff_impact_to_json(&result);
        // Add overall risk
        json_val["overall_risk"] = serde_json::json!(overall_risk_label(&result));
        crate::cli::json_envelope::emit_json(&json_val)?;
    } else {
        display_affected_text(&result, root);
    }

    Ok(())
}

fn empty_affected_json() -> serde_json::Value {
    serde_json::json!({
        "changed_functions": [],
        "callers": [],
        "tests": [],
        "overall_risk": "none",
        "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
    })
}

fn overall_risk_label(result: &DiffImpactResult) -> &'static str {
    if result.all_callers.len() > 10 || result.changed_functions.len() > 5 {
        "high"
    } else if result.all_callers.len() > 3 || result.changed_functions.len() > 2 {
        "medium"
    } else {
        "low"
    }
}

fn display_affected_text(result: &DiffImpactResult, root: &Path) {
    // Changed functions table
    println!(
        "{} ({}):",
        "Changed functions".bold(),
        result.changed_functions.len()
    );
    for f in &result.changed_functions {
        let rel = rel_display(&f.file, root);
        println!("  {} ({}:{})", f.name.cyan(), rel.dimmed(), f.line_start);
    }

    // Callers
    if !result.all_callers.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Affected callers".bold(),
            result.all_callers.len()
        );
        for c in &result.all_callers {
            let rel = rel_display(&c.file, root);
            println!("  {} ({}:{})", c.name, rel.dimmed(), c.line);
        }
    }

    // Tests
    if !result.all_tests.is_empty() {
        println!();
        println!("{} ({}):", "Tests to re-run".bold(), result.all_tests.len());
        for t in &result.all_tests {
            let rel = rel_display(&t.file, root);
            println!(
                "  {} ({}:{}) [via {}, depth {}]",
                t.name, rel, t.line, t.via, t.call_depth
            );
        }
    }

    // Risk summary
    println!();
    let risk = if result.all_callers.len() > 10 || result.changed_functions.len() > 5 {
        risk_label(&RiskLevel::High)
    } else if result.all_callers.len() > 3 || result.changed_functions.len() > 2 {
        risk_label(&RiskLevel::Medium)
    } else {
        risk_label(&RiskLevel::Low)
    };
    println!(
        "Risk: {} ({} changed, {} callers, {} tests)",
        risk,
        result.changed_functions.len(),
        result.all_callers.len(),
        result.all_tests.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_affected_json_shape() {
        let j = empty_affected_json();
        assert_eq!(j["summary"]["changed_count"], 0);
        assert_eq!(j["summary"]["caller_count"], 0);
        assert_eq!(j["summary"]["test_count"], 0);
        assert_eq!(j["overall_risk"], "none");
    }

    #[test]
    fn empty_diff_produces_no_changes() {
        let hunks = parse_unified_diff("");
        assert!(hunks.is_empty());
    }

    #[test]
    fn overall_risk_thresholds() {
        // Build minimal DiffImpactResult to test risk thresholds
        let empty_result = DiffImpactResult {
            changed_functions: vec![],
            all_callers: vec![],
            all_tests: vec![],
            summary: cqs::DiffImpactSummary {
                changed_count: 0,
                caller_count: 0,
                test_count: 0,
                truncated: false,
                truncated_functions: 0,
            },
        };
        assert_eq!(overall_risk_label(&empty_result), "low");
    }
}
