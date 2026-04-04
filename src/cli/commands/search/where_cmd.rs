//! Where command — suggest placement for new code
//!
//! Core JSON construction is in [`where_to_json`] so batch mode can reuse it.

use std::path::Path;

use anyhow::Result;

use cqs::suggest_placement;

/// Build JSON for placement suggestions.
///
/// Shared between CLI (`cmd_where --json`) and batch (`dispatch_where`).
pub(crate) fn where_to_json(
    result: &cqs::PlacementResult,
    description: &str,
    root: &Path,
) -> serde_json::Value {
    let _span = tracing::info_span!("where_to_json").entered();

    let suggestions_json: Vec<_> = result
        .suggestions
        .iter()
        .map(|s| {
            let rel = cqs::rel_display(&s.file, root);
            serde_json::json!({
                "file": rel,
                "score": s.score,
                "insertion_line": s.insertion_line,
                "near_function": s.near_function,
                "reason": s.reason,
                "patterns": {
                    "imports": s.patterns.imports,
                    "error_handling": s.patterns.error_handling,
                    "naming_convention": s.patterns.naming_convention,
                    "visibility": s.patterns.visibility,
                    "has_inline_tests": s.patterns.has_inline_tests,
                }
            })
        })
        .collect();

    serde_json::json!({
        "description": description,
        "suggestions": suggestions_json,
    })
}

pub(crate) fn cmd_where(
    ctx: &crate::cli::CommandContext,
    description: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_where", description).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);

    let result = suggest_placement(store, embedder, description, limit)?;

    if json {
        let output = where_to_json(&result, description, root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;

        println!("{} {}", "Where to add:".cyan(), description.bold());

        if result.suggestions.is_empty() {
            println!();
            println!("{}", "No placement suggestions found.".dimmed());
        } else {
            for (i, s) in result.suggestions.iter().enumerate() {
                let rel = cqs::rel_display(&s.file, root);
                println!();
                println!(
                    "{}. {} {}",
                    i + 1,
                    rel.bold(),
                    format!("(score: {:.2})", s.score).dimmed()
                );
                println!(
                    "   Insert after line {} (near {})",
                    s.insertion_line, s.near_function
                );
                println!("   {}", s.reason.dimmed());

                // Show patterns
                if !s.patterns.visibility.is_empty() {
                    println!(
                        "   {} {} | {} | {} {}",
                        "Patterns:".cyan(),
                        s.patterns.visibility,
                        s.patterns.naming_convention,
                        s.patterns.error_handling,
                        if s.patterns.has_inline_tests {
                            "| inline tests"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
    }

    Ok(())
}
