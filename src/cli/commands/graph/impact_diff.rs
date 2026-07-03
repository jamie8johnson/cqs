//! Impact-diff command — what breaks based on a git diff

use anyhow::Result;

use cqs::parse_unified_diff;
use cqs::{
    analyze_diff_impact, diff_impact_empty_json, diff_impact_to_json, map_hunks_to_functions,
};

/// Surface-agnostic core input for `cqs impact-diff` — the daemon JSON-args
/// path (`cqs_impact_diff`) deserializes this directly. Exposes ONLY `base`
/// (the git ref to diff against). The CLI's `--stdin` (read the diff from
/// stdin) is WITHHELD by absence: the daemon has no agent stdin, so the
/// JSON-args adapter forces `stdin: false`, mirroring how `cqs_index` withholds
/// `--force`. Input-only: derives `Deserialize` + `JsonSchema`, never
/// `Serialize` — the command's JSON OUTPUT is the separate `diff_impact_to_json`
/// shape, so this core cannot change the output wire contract. `#[serde(default)]`
/// lets a wire caller omit `base` and inherit the working-tree default (`None`).
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct ImpactDiffArgs {
    /// Git ref to diff against (default: unstaged working-tree changes).
    pub base: Option<String>,
}

pub(crate) fn cmd_impact_diff(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    base: Option<&str>,
    from_stdin: bool,
    json: bool,
    overlay: &crate::cli::args::OverlayArgs,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_impact_diff").entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // 1. Get diff text. The non-stdin branch resolves the diff subject through the
    // overlay tri-state: an active worktree overlay diffs the worktree, `--no-overlay`
    // reverts to parent-truth (see `cmd_review`). The daemon `dispatch_impact_diff`
    // has its own overlay plumbing and diffs the same subject on the JSON fast path.
    // `--stdin` stays byte-identical. Hoisted out of the diff-text branch so its
    // `Some`-ness also gates the CLI-direct partial-skip markers below; `--stdin`
    // carries no overlay subject.
    let overlay_root = if from_stdin {
        None
    } else {
        crate::cli::commands::cli_diff_overlay_root(overlay, root)
    };
    let diff_text = if from_stdin {
        crate::cli::commands::read_stdin()?
    } else {
        crate::cli::commands::run_git_diff_for_overlay(
            base,
            &crate::cli::commands::cli_parent_diff_root(root),
            overlay_root.as_deref(),
        )?
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
        // CLI-direct two-axis partial-skip vocabulary (see `cmd_review`). Reached
        // only past the empty-diff early-returns above, so the diff is non-empty —
        // gate purely on the diff subject resolving to the worktree.
        if overlay_root.is_some() {
            crate::cli::commands::mark_cli_direct_diff_overlay_partial();
        }
        let json_val = diff_impact_to_json(&result)?;
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
