//! Related command — co-occurrence analysis
//!
//! Core JSON builders are shared between CLI and batch handlers.

use std::path::Path;

use anyhow::Result;

// ─── Shared JSON builders ───────────────────────────────────────────────────

/// Build JSON array from a slice of `RelatedFunction` — shared between CLI and batch.
pub(crate) fn related_items_to_json(
    items: &[cqs::RelatedFunction],
    root: &Path,
) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|r| {
            let rel = cqs::rel_display(&r.file, root);
            serde_json::json!({
                "name": r.name,
                "file": rel,
                "line": r.line,
                "overlap_count": r.overlap_count,
            })
        })
        .collect()
}

/// Build full JSON output from a `RelatedResult` — shared between CLI and batch.
pub(crate) fn related_result_to_json(
    result: &cqs::RelatedResult,
    root: &Path,
) -> serde_json::Value {
    serde_json::json!({
        "target": result.target,
        "shared_callers": related_items_to_json(&result.shared_callers, root),
        "shared_callees": related_items_to_json(&result.shared_callees, root),
        "shared_types": related_items_to_json(&result.shared_types, root),
    })
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_related(
    ctx: &crate::cli::CommandContext,
    name: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_related", name).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    let result = cqs::find_related(store, name, limit)?;

    if json {
        let output = related_result_to_json(&result, root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;
        println!("{} {}", "Related to:".cyan(), result.target.bold());

        if !result.shared_callers.is_empty() {
            println!();
            println!("{}", "Shared callers (called by same functions):".cyan());
            for r in &result.shared_callers {
                let rel = cqs::rel_display(&r.file, root);
                println!(
                    "  {} {} ({} shared)",
                    r.name.bold(),
                    format!("{}:{}", rel, r.line).dimmed(),
                    r.overlap_count,
                );
            }
        }

        if !result.shared_callees.is_empty() {
            println!();
            println!("{}", "Shared callees (call same functions):".cyan());
            for r in &result.shared_callees {
                let rel = cqs::rel_display(&r.file, root);
                println!(
                    "  {} {} ({} shared)",
                    r.name.bold(),
                    format!("{}:{}", rel, r.line).dimmed(),
                    r.overlap_count,
                );
            }
        }

        if !result.shared_types.is_empty() {
            println!();
            println!("{}", "Shared types (use same custom types):".cyan());
            for r in &result.shared_types {
                let rel = cqs::rel_display(&r.file, root);
                println!(
                    "  {} {} ({} shared)",
                    r.name.bold(),
                    format!("{}:{}", rel, r.line).dimmed(),
                    r.overlap_count,
                );
            }
        }

        if result.shared_callers.is_empty()
            && result.shared_callees.is_empty()
            && result.shared_types.is_empty()
        {
            println!();
            println!("{}", "No related functions found.".dimmed());
        }
    }

    Ok(())
}
