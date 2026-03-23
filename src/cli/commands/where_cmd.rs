//! Where command — suggest placement for new code

use anyhow::Result;

use cqs::{suggest_placement, Embedder};

/// Suggests optimal file locations for placing new code based on a description.
///
/// # Arguments
///
/// * `description` - A description of the code to be placed
/// * `limit` - Maximum number of placement suggestions to return (clamped between 1 and 10)
/// * `json` - If true, output results as formatted JSON; otherwise, output human-readable text
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if the project store cannot be opened, the embedder fails to initialize, or JSON serialization fails.
///
/// # Errors
///
/// Returns an error if opening the project store fails, creating the embedder fails, generating placement suggestions fails, or JSON serialization fails.
pub(crate) fn cmd_where(description: &str, limit: usize, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_where", description).entered();
    let (store, root, _) = crate::cli::open_project_store_readonly()?;
    let embedder = Embedder::new()?;
    let limit = limit.clamp(1, 10);

    let result = suggest_placement(&store, &embedder, description, limit)?;

    if json {
        let suggestions_json: Vec<_> = result
            .suggestions
            .iter()
            .map(|s| {
                let rel = cqs::rel_display(&s.file, &root);
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
        let output = serde_json::json!({
            "description": description,
            "suggestions": suggestions_json,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;

        println!("{} {}", "Where to add:".cyan(), description.bold());

        if result.suggestions.is_empty() {
            println!();
            println!("{}", "No placement suggestions found.".dimmed());
        } else {
            for (i, s) in result.suggestions.iter().enumerate() {
                let rel = cqs::rel_display(&s.file, &root);
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
