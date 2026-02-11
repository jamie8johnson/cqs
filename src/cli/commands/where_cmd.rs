//! Where command — suggest placement for new code

use anyhow::Result;

use cqs::{suggest_placement, Embedder, Store};

use crate::cli::find_project_root;

pub(crate) fn cmd_where(
    _cli: &crate::cli::Cli,
    description: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    let embedder = Embedder::new()?;
    let limit = limit.clamp(1, 10);

    let result = suggest_placement(&store, &embedder, description, limit)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if json {
        let suggestions_json: Vec<_> = result
            .suggestions
            .iter()
            .map(|s| {
                let rel = s
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&s.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
                let rel = s
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&s.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
