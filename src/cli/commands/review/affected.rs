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

// ---------------------------------------------------------------------------
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`affected_core`]. The diff text is acquired by the adapter
/// (CLI: git or `--stdin`) and passed in, so the core stays I/O-free.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct AffectedArgs {}

/// Typed output for `cqs affected`. The per-result JSON projection is owned by
/// the lib (`diff_impact_to_json` handles rel-path display + field selection),
/// so this is a thin newtype over the projected value plus the
/// command-specific `overall_risk` field. Implementing `Serialize` by emitting
/// the inner value keeps the schema lib-owned while giving the core a typed
/// return (one definition site for the `affected` JSON).
#[derive(Debug)]
pub(crate) struct AffectedOutput(serde_json::Value);

impl serde::Serialize for AffectedOutput {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

/// Surface-agnostic core for `cqs affected`. Parses the diff, maps hunks to
/// functions, runs the impact analysis, and returns the typed
/// [`AffectedOutput`]. Empty diffs / no-indexed-functions both yield the
/// shared empty shape (with `overall_risk: "none"`). `cqs affected` is
/// CLI-only today, but the core is daemon-ready by construction.
pub(crate) fn affected_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &std::path::Path,
    diff_text: &str,
    _args: &AffectedArgs,
) -> Result<AffectedOutput> {
    let _span = tracing::info_span!("affected_core").entered();

    let hunks = parse_unified_diff(diff_text);
    if hunks.is_empty() {
        return Ok(AffectedOutput(empty_affected_json()));
    }

    let changed = map_hunks_to_functions(store, &hunks);
    if changed.is_empty() {
        return Ok(AffectedOutput(empty_affected_json()));
    }

    let result = analyze_diff_impact(store, changed, root)?;
    let mut json_val = diff_impact_to_json(&result)?;
    json_val["overall_risk"] = serde_json::json!(overall_risk(&result).to_string());
    Ok(AffectedOutput(json_val))
}

pub(crate) fn cmd_affected(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    from_stdin: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_affected", from_stdin).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // Get diff text — `--stdin` lets agents pipe a captured diff
    // (`git diff main | cqs affected --stdin --json`) without re-shelling
    // git. Mirrors the path in `cmd_review`/`cmd_ci`/`cmd_impact_diff`.
    // Adapter owns I/O.
    let diff_text = if from_stdin {
        crate::cli::commands::read_stdin()?
    } else {
        crate::cli::commands::run_git_diff(base, root)?
    };

    if json {
        let output = affected_core(store, root, &diff_text, &AffectedArgs::default())?;
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        // Text path re-derives the analysis to drive the dashboard rendering
        // (the typed output is a JSON projection; text needs the rich result).
        let hunks = parse_unified_diff(&diff_text);
        if hunks.is_empty() {
            println!("No changes detected.");
            return Ok(());
        }
        let changed = map_hunks_to_functions(store, &hunks);
        if changed.is_empty() {
            println!("No indexed functions affected by this diff.");
            return Ok(());
        }
        let result = analyze_diff_impact(store, changed, root)?;
        display_affected_text(&result, root);
    }

    Ok(())
}

fn empty_affected_json() -> serde_json::Value {
    // Share the empty-diff JSON shape with impact_diff / graph handlers. Add
    // `overall_risk: "none"` on top of the shared base — a sentinel that
    // cannot collide with overall_risk() (which only emits Low/Medium/High)
    // so agents can detect "no changes" without counting.
    let mut base = cqs::diff_impact_empty_json();
    base["overall_risk"] = serde_json::json!("none");
    base
}

/// Single source of truth for the affected-command risk thresholds. Both the
/// JSON path (`overall_risk` field) and the text path (`Risk: ...` footer) go
/// through this function so the two renderings can't drift on future threshold
/// tweaks.
fn overall_risk(result: &DiffImpactResult) -> RiskLevel {
    if result.all_callers.len() > 10 || result.changed_functions.len() > 5 {
        RiskLevel::High
    } else if result.all_callers.len() > 3 || result.changed_functions.len() > 2 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
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

    // Risk summary — route through the same `overall_risk` helper used by
    // the JSON path so the two outputs can't drift.
    println!();
    let risk = risk_label(&overall_risk(result));
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
                degraded: false,
            },
        };
        assert_eq!(overall_risk(&empty_result), RiskLevel::Low);
        assert_eq!(overall_risk(&empty_result).to_string(), "low");
    }
}
