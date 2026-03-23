//! Impact-diff command — what breaks based on a git diff

use anyhow::Result;

use cqs::parse_unified_diff;
use cqs::{analyze_diff_impact, diff_impact_to_json, map_hunks_to_functions};

/// Creates an empty impact analysis JSON structure.
///
/// Constructs and returns a JSON value representing an impact analysis report with no changed functions, callers, or tests. This serves as a default or template response when there are no code changes to analyze.
///
/// # Returns
///
/// A `serde_json::Value` containing an empty impact analysis object with zero counts for changed functions, callers, and tests.
fn empty_impact_json() -> serde_json::Value {
    serde_json::json!({
        "changed_functions": [],
        "callers": [],
        "tests": [],
        "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
    })
}

/// Analyzes the impact of code changes by comparing a diff against indexed functions in the project.
///
/// # Arguments
///
/// * `_cli` - CLI context (currently unused)
/// * `base` - Optional git base ref for computing the diff; if not provided, uses unstaged changes
/// * `from_stdin` - If true, reads diff from stdin; otherwise runs `git diff` against base
/// * `json` - If true, outputs results in JSON format; otherwise outputs human-readable text
///
/// # Returns
///
/// Returns `Ok(())` on successful completion, or an error if project loading, diff parsing, or impact analysis fails.
///
/// # Panics
///
/// Does not explicitly panic; all error conditions are propagated as `Result` values.
pub(crate) fn cmd_impact_diff(
    _cli: &crate::cli::Cli,
    base: Option<&str>,
    from_stdin: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_impact_diff").entered();
    let (store, root, _) = crate::cli::open_project_store_readonly()?;

    // 1. Get diff text
    let diff_text = if from_stdin {
        super::read_stdin()?
    } else {
        super::run_git_diff(base)?
    };

    // 2. Parse hunks
    let hunks = parse_unified_diff(&diff_text);
    if hunks.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&empty_impact_json())?);
        } else {
            println!("No changes detected.");
        }
        return Ok(());
    }

    // 3. Map hunks to functions
    let changed = map_hunks_to_functions(&store, &hunks);

    if changed.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&empty_impact_json())?);
        } else {
            println!("No indexed functions affected by this diff.");
        }
        return Ok(());
    }

    // 4. Analyze impact
    let result = analyze_diff_impact(&store, changed)?;

    // 5. Display
    if json {
        let json_val = diff_impact_to_json(&result, &root);
        println!("{}", serde_json::to_string_pretty(&json_val)?);
    } else {
        display_diff_impact_text(&result, &root);
    }

    Ok(())
}

/// Displays a formatted text summary of code change impact analysis results to stdout.
///
/// Prints colored output organized into three sections: changed functions, affected callers, and tests that need re-running. Each section includes counts and detailed information (names, file paths, line numbers) for relevant items. The root path is used to compute relative file paths for display.
///
/// # Arguments
///
/// * `result` - The diff impact analysis result containing changed functions, affected callers, and affected tests.
/// * `root` - The root path used to compute relative file paths for display purposes.
///
/// # Returns
///
/// Returns nothing; output is written directly to stdout.
fn display_diff_impact_text(result: &cqs::DiffImpactResult, root: &std::path::Path) {
    use colored::Colorize;

    // Changed functions
    println!(
        "{} ({}):",
        "Changed functions".bold(),
        result.changed_functions.len()
    );
    for f in &result.changed_functions {
        println!("  {} ({}:{})", f.name, f.file.display(), f.line_start);
    }

    // Callers
    if result.all_callers.is_empty() {
        println!();
        println!("{}", "No affected callers.".dimmed());
    } else {
        println!();
        println!(
            "{} ({}):",
            "Affected callers".cyan(),
            result.all_callers.len()
        );
        for c in &result.all_callers {
            let rel = cqs::rel_display(&c.file, root);
            println!(
                "  {} ({}:{}, call at line {})",
                c.name, rel, c.line, c.call_line
            );
        }
    }

    // Tests
    if result.all_tests.is_empty() {
        println!();
        println!("{}", "No affected tests.".dimmed());
    } else {
        println!();
        println!(
            "{} ({}):",
            "Tests to re-run".yellow(),
            result.all_tests.len()
        );
        for t in &result.all_tests {
            let rel = cqs::rel_display(&t.file, root);
            println!(
                "  {} ({}:{}) [via {}, depth {}]",
                t.name, rel, t.line, t.via, t.call_depth
            );
        }
    }
}
