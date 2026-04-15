//! Where command — suggest placement for new code
//!
//! Core JSON construction is in [`build_where_output`] so batch mode can reuse it.

use std::path::Path;

use anyhow::Result;

use cqs::suggest_placement;

// ─── Output types ──────────────────────────────────────────────────────────

/// Patterns detected in a suggested file.
#[derive(Debug, serde::Serialize)]
pub(crate) struct WherePatternsEntry {
    pub imports: Vec<String>,
    pub error_handling: String,
    pub naming_convention: String,
    pub visibility: String,
    pub has_inline_tests: bool,
}

/// A single placement suggestion entry.
#[derive(Debug, serde::Serialize)]
pub(crate) struct WhereSuggestionEntry {
    pub file: String,
    pub score: f32,
    pub insertion_line: u32,
    pub near_function: String,
    pub reason: String,
    pub patterns: WherePatternsEntry,
}

/// Typed JSON output for the where command.
#[derive(Debug, serde::Serialize)]
pub(crate) struct WhereOutput {
    pub description: String,
    pub suggestions: Vec<WhereSuggestionEntry>,
}

// ─── Shared JSON builder ───────────────────────────────────────────────────

/// Build typed where output from placement results — shared between CLI and batch.
pub(crate) fn build_where_output(
    result: &cqs::PlacementResult,
    description: &str,
    root: &Path,
) -> WhereOutput {
    let _span = tracing::info_span!("build_where_output").entered();

    let suggestions: Vec<WhereSuggestionEntry> = result
        .suggestions
        .iter()
        .map(|s| {
            let rel = cqs::rel_display(&s.file, root);
            WhereSuggestionEntry {
                file: rel,
                score: s.score,
                insertion_line: s.insertion_line,
                near_function: s.near_function.clone(),
                reason: s.reason.clone(),
                patterns: WherePatternsEntry {
                    imports: s.patterns.imports.clone(),
                    error_handling: s.patterns.error_handling.clone(),
                    naming_convention: s.patterns.naming_convention.clone(),
                    visibility: s.patterns.visibility.clone(),
                    has_inline_tests: s.patterns.has_inline_tests,
                },
            }
        })
        .collect();

    WhereOutput {
        description: description.to_string(),
        suggestions,
    }
}

// ─── CLI command ───────────────────────────────────────────────────────────

pub(crate) fn cmd_where(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
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
        let output = build_where_output(&result, description, root);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_suggestion(file: &str, score: f32) -> cqs::FileSuggestion {
        cqs::FileSuggestion {
            file: PathBuf::from(file),
            score,
            insertion_line: 42,
            near_function: "nearby_fn".to_string(),
            reason: "Good fit".to_string(),
            patterns: cqs::LocalPatterns {
                imports: vec!["use std::io;".to_string()],
                error_handling: "anyhow".to_string(),
                naming_convention: "snake_case".to_string(),
                visibility: "pub(crate)".to_string(),
                has_inline_tests: true,
            },
        }
    }

    #[test]
    fn where_output_empty() {
        let result = cqs::PlacementResult {
            suggestions: vec![],
        };
        let root = PathBuf::from("/project");
        let output = build_where_output(&result, "add feature X", &root);
        assert_eq!(output.description, "add feature X");
        assert!(output.suggestions.is_empty());
    }

    #[test]
    fn where_output_with_suggestion() {
        let result = cqs::PlacementResult {
            suggestions: vec![make_suggestion("src/lib.rs", 0.85)],
        };
        let root = PathBuf::from("/project");
        let output = build_where_output(&result, "add Y", &root);
        assert_eq!(output.suggestions.len(), 1);
        assert_eq!(output.suggestions[0].insertion_line, 42);
        assert_eq!(output.suggestions[0].near_function, "nearby_fn");
        assert!(output.suggestions[0].patterns.has_inline_tests);
    }

    #[test]
    fn where_output_serializes() {
        let result = cqs::PlacementResult {
            suggestions: vec![make_suggestion("src/lib.rs", 0.9)],
        };
        let root = PathBuf::from("/project");
        let output = build_where_output(&result, "desc", &root);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["description"], "desc");
        assert!(json["suggestions"].is_array());
        assert_eq!(json["suggestions"][0]["insertion_line"], 42);
        assert_eq!(
            json["suggestions"][0]["patterns"]["error_handling"],
            "anyhow"
        );
    }

    #[test]
    fn where_output_serializes_to_json_value() {
        let result = cqs::PlacementResult {
            suggestions: vec![make_suggestion("src/foo.rs", 0.7)],
        };
        let root = PathBuf::from("/project");
        let output = build_where_output(&result, "test desc", &root);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["description"], "test desc");
        assert_eq!(json["suggestions"][0]["near_function"], "nearby_fn");
    }
}
