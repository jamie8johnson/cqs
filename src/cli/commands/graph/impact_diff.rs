//! Impact-diff command — what breaks based on a git diff

use anyhow::Result;

use cqs::parse_unified_diff;
use cqs::{
    analyze_diff_impact, diff_impact_empty_json, diff_impact_to_json, map_hunks_to_functions,
};

pub(crate) fn cmd_impact_diff(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    from_stdin: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_impact_diff").entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // 1. Get diff text
    let diff_text = if from_stdin {
        crate::cli::commands::read_stdin()?
    } else {
        crate::cli::commands::run_git_diff(base)?
    };

    // 2. Parse hunks
    let hunks = parse_unified_diff(&diff_text);
    if hunks.is_empty() {
        if json {
            crate::cli::json_envelope::emit_json(&diff_impact_empty_json())?;
        } else {
            println!("No changes detected.");
        }
        return Ok(());
    }

    // 3. Map hunks to functions
    let changed = map_hunks_to_functions(store, &hunks);

    if changed.is_empty() {
        if json {
            crate::cli::json_envelope::emit_json(&diff_impact_empty_json())?;
        } else {
            println!("No indexed functions affected by this diff.");
        }
        return Ok(());
    }

    // 4. Analyze impact
    let result = analyze_diff_impact(store, changed, root)?;

    // 5. Display
    if json {
        let json_val = diff_impact_to_json(&result);
        crate::cli::json_envelope::emit_json(&json_val)?;
    } else {
        display_diff_impact_text(&result, root);
    }

    Ok(())
}

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
