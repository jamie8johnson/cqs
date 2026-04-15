//! Related command — co-occurrence analysis
//!
//! Core JSON builders are shared between CLI and batch handlers.

use std::path::Path;

use anyhow::Result;

// ─── Output types ──────────────────────────────────────────────────────────

/// A single related-function entry in the JSON output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct RelatedEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32, // was "line"
    pub overlap_count: u32,
}

/// Typed JSON output for the related command.
#[derive(Debug, serde::Serialize)]
pub(crate) struct RelatedOutput {
    pub target: String,
    pub shared_callers: Vec<RelatedEntry>,
    pub shared_callees: Vec<RelatedEntry>,
    pub shared_types: Vec<RelatedEntry>,
}

// ─── Shared JSON builders ───────────────────────────────────────────────────

/// Build typed entries from a slice of `RelatedFunction` — shared between CLI and batch.
fn build_related_entries(items: &[cqs::RelatedFunction], root: &Path) -> Vec<RelatedEntry> {
    items
        .iter()
        .map(|r| {
            let rel = cqs::rel_display(&r.file, root);
            RelatedEntry {
                name: r.name.clone(),
                file: rel,
                line_start: r.line,
                overlap_count: r.overlap_count,
            }
        })
        .collect()
}

/// Build full typed output from a `RelatedResult` — shared between CLI and batch.
pub(crate) fn build_related_output(result: &cqs::RelatedResult, root: &Path) -> RelatedOutput {
    let _span = tracing::info_span!("build_related_output", target = %result.target).entered();

    RelatedOutput {
        target: result.target.clone(),
        shared_callers: build_related_entries(&result.shared_callers, root),
        shared_callees: build_related_entries(&result.shared_callees, root),
        shared_types: build_related_entries(&result.shared_types, root),
    }
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_related(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_related", name).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    // CQ-V1.25-2: clamp via shared constant. Previously unbounded on CLI
    // vs 100 on batch; `find_related` runs a triple overlap query that
    // doesn't scale well to thousands of entries per category.
    let limit = limit.clamp(1, crate::cli::RELATED_LIMIT_MAX);

    let result = cqs::find_related(store, name, limit)?;

    if json {
        let output = build_related_output(&result, root);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_related_fn(name: &str, line: u32, overlap: u32) -> cqs::RelatedFunction {
        cqs::RelatedFunction {
            name: name.to_string(),
            file: PathBuf::from("src/lib.rs"),
            line,
            overlap_count: overlap,
        }
    }

    #[test]
    fn related_output_empty() {
        let result = cqs::RelatedResult {
            target: "my_func".to_string(),
            shared_callers: vec![],
            shared_callees: vec![],
            shared_types: vec![],
        };
        let root = PathBuf::from("/project");
        let output = build_related_output(&result, &root);
        assert_eq!(output.target, "my_func");
        assert!(output.shared_callers.is_empty());
        assert!(output.shared_callees.is_empty());
        assert!(output.shared_types.is_empty());
    }

    #[test]
    fn related_output_uses_line_start() {
        let result = cqs::RelatedResult {
            target: "foo".to_string(),
            shared_callers: vec![make_related_fn("bar", 42, 3)],
            shared_callees: vec![],
            shared_types: vec![],
        };
        let root = PathBuf::from("/project");
        let output = build_related_output(&result, &root);
        assert_eq!(output.shared_callers[0].line_start, 42);
        assert_eq!(output.shared_callers[0].overlap_count, 3);
    }

    #[test]
    fn related_output_serializes_line_start() {
        let result = cqs::RelatedResult {
            target: "foo".to_string(),
            shared_callers: vec![make_related_fn("bar", 10, 2)],
            shared_callees: vec![],
            shared_types: vec![],
        };
        let root = PathBuf::from("/project");
        let output = build_related_output(&result, &root);
        let json = serde_json::to_value(&output).unwrap();
        // Verify "line_start" is used, not "line"
        assert!(json["shared_callers"][0].get("line_start").is_some());
        assert!(json["shared_callers"][0].get("line").is_none());
        assert_eq!(json["shared_callers"][0]["line_start"], 10);
    }

    #[test]
    fn related_output_serializes_to_json_value() {
        let result = cqs::RelatedResult {
            target: "baz".to_string(),
            shared_callers: vec![],
            shared_callees: vec![make_related_fn("qux", 5, 1)],
            shared_types: vec![],
        };
        let root = PathBuf::from("/project");
        let output = build_related_output(&result, &root);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["target"], "baz");
        assert_eq!(json["shared_callees"][0]["name"], "qux");
        assert_eq!(json["shared_callees"][0]["line_start"], 5);
    }
}
