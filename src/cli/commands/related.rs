//! Related command — co-occurrence analysis

use anyhow::Result;

/// Converts a slice of related functions into JSON values for serialization.
///
/// # Arguments
///
/// * `items` - A slice of `RelatedFunction` objects to convert
/// * `root` - The root path used to compute relative file paths
///
/// # Returns
///
/// A vector of JSON objects, each containing the function name, relative file path, line number, and overlap count.
fn related_to_json(
    items: &[cqs::RelatedFunction],
    root: &std::path::Path,
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

/// Finds and displays functions or types related to a given symbol by shared callers, callees, and type usage.
///
/// # Arguments
///
/// * `_cli` - CLI context (unused)
/// * `name` - The name of the symbol to find related items for
/// * `limit` - Maximum number of results to return for each category
/// * `json` - If true, output results as formatted JSON; otherwise use colored text format
///
/// # Returns
///
/// Returns `Ok(())` on success, or an error if the project store cannot be opened or the query fails.
///
/// # Errors
///
/// Returns an error if the project store cannot be opened or if the relation query fails.
pub(crate) fn cmd_related(
    _cli: &crate::cli::Cli,
    name: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_related", name).entered();
    let (store, root, _) = crate::cli::open_project_store()?;

    let result = cqs::find_related(&store, name, limit)?;

    if json {
        let shared_callers = related_to_json(&result.shared_callers, &root);
        let shared_callees = related_to_json(&result.shared_callees, &root);
        let shared_types = related_to_json(&result.shared_types, &root);

        let output = serde_json::json!({
            "target": result.target,
            "shared_callers": shared_callers,
            "shared_callees": shared_callees,
            "shared_types": shared_types,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;
        println!("{} {}", "Related to:".cyan(), result.target.bold());

        if !result.shared_callers.is_empty() {
            println!();
            println!("{}", "Shared callers (called by same functions):".cyan());
            for r in &result.shared_callers {
                let rel = cqs::rel_display(&r.file, &root);
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
                let rel = cqs::rel_display(&r.file, &root);
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
                let rel = cqs::rel_display(&r.file, &root);
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
