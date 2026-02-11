//! Related command — co-occurrence analysis

use anyhow::{bail, Result};

use cqs::Store;

use crate::cli::find_project_root;

pub(crate) fn cmd_related(
    _cli: &crate::cli::Cli,
    name: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

    let result = cqs::find_related(&store, name, limit)?;

    if json {
        let shared_callers: Vec<_> = result
            .shared_callers
            .iter()
            .map(|r| {
                let rel = r
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&r.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                serde_json::json!({
                    "name": r.name,
                    "file": rel,
                    "line": r.line,
                    "overlap_count": r.overlap_count,
                })
            })
            .collect();
        let shared_callees: Vec<_> = result
            .shared_callees
            .iter()
            .map(|r| {
                let rel = r
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&r.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                serde_json::json!({
                    "name": r.name,
                    "file": rel,
                    "line": r.line,
                    "overlap_count": r.overlap_count,
                })
            })
            .collect();
        let shared_types: Vec<_> = result
            .shared_types
            .iter()
            .map(|r| {
                let rel = r
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&r.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                serde_json::json!({
                    "name": r.name,
                    "file": rel,
                    "line": r.line,
                    "overlap_count": r.overlap_count,
                })
            })
            .collect();

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
                let rel = r
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&r.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
                let rel = r
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&r.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
                let rel = r
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&r.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
