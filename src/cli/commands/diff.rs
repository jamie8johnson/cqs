//! Diff command — semantic diff between indexed snapshots

use anyhow::{bail, Context, Result};
use colored::Colorize;

use cqs::Store;
use cqs::{semantic_diff, DiffResult};

use crate::cli::find_project_root;

/// Compares semantic differences between two code stores and displays the results.
///
/// Loads a source code store from a reference and compares it against a target store (either the current project index or another reference). Computes semantic similarity differences using the specified threshold and optional language filter, then outputs the results in either JSON or human-readable format.
///
/// # Arguments
///
/// * `source` - Reference identifier for the source store to compare from
/// * `target` - Optional reference identifier for the target store; defaults to "project" (the current project index)
/// * `threshold` - Similarity threshold (0.0-1.0) for filtering semantic differences
/// * `lang` - Optional programming language filter to restrict the comparison scope
/// * `json` - If true, output results as JSON; otherwise use human-readable format
///
/// # Returns
///
/// Returns `Ok(())` on successful comparison and display, or an error if store resolution, semantic diff computation, or output rendering fails.
///
/// # Errors
///
/// Returns an error if the source or target store cannot be resolved, if the project index does not exist when comparing against "project", if the project store cannot be opened, or if semantic diff computation fails.
pub(crate) fn cmd_diff(
    source: &str,
    target: Option<&str>,
    threshold: f32,
    lang: Option<&str>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_diff", source).entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);

    // Resolve source store (must be a reference)
    let source_store = super::resolve::resolve_reference_store(&root, source)?;

    // Resolve target store
    let target_label = target.unwrap_or("project");
    let target_store = if target_label == "project" {
        let index_path = cqs_dir.join("index.db");
        if !index_path.exists() {
            bail!("Project index not found. Run 'cqs init && cqs index' first.");
        }
        Store::open(&index_path)
            .with_context(|| format!("Failed to open project store at {}", index_path.display()))?
    } else {
        super::resolve::resolve_reference_store(&root, target_label)?
    };

    let result = semantic_diff(
        &source_store,
        &target_store,
        source,
        target_label,
        threshold,
        lang,
    )?;

    if json {
        display_diff_json(&result)?;
    } else {
        display_diff(&result)?;
    }

    Ok(())
}

/// Displays a formatted diff report showing changes between two versions.
///
/// Prints a structured summary of all additions, removals, and modifications with color-coded output, including entry types, names, file paths, and similarity scores for modified items. Concludes with a summary line of totals.
///
/// # Arguments
///
/// * `result` - A reference to the DiffResult containing the source and target versions and their differences
///
/// # Returns
///
/// Returns `Ok(())` on successful output, or an error if printing fails.
fn display_diff(result: &DiffResult) -> Result<()> {
    println!("Diff: {} → {}", result.source.bold(), result.target.bold());
    println!();

    if !result.added.is_empty() {
        println!("{} ({}):", "Added".green().bold(), result.added.len());
        for entry in &result.added {
            println!(
                "  + {} {} ({})",
                entry.chunk_type,
                entry.name,
                entry.file.display()
            );
        }
        println!();
    }

    if !result.removed.is_empty() {
        println!("{} ({}):", "Removed".red().bold(), result.removed.len());
        for entry in &result.removed {
            println!(
                "  - {} {} ({})",
                entry.chunk_type,
                entry.name,
                entry.file.display()
            );
        }
        println!();
    }

    if !result.modified.is_empty() {
        println!(
            "{} ({}):",
            "Modified".yellow().bold(),
            result.modified.len()
        );
        for entry in &result.modified {
            let sim = entry
                .similarity
                .map(|s| format!("[{:.2}]", s))
                .unwrap_or_else(|| "[?]".to_string());
            println!(
                "  ~ {} {} ({}) {}",
                entry.chunk_type,
                entry.name,
                entry.file.display(),
                sim
            );
        }
        println!();
    }

    println!(
        "Summary: {} added, {} removed, {} modified, {} unchanged",
        result.added.len(),
        result.removed.len(),
        result.modified.len(),
        result.unchanged_count,
    );

    Ok(())
}

/// Formats and outputs a diff result as a formatted JSON document to stdout.
///
/// Converts the added, removed, and modified entries from a DiffResult into JSON objects containing their name, file path, and type. Includes a summary section with counts of all changes. Outputs the complete result as pretty-printed JSON.
///
/// # Arguments
///
/// * `result` - A reference to the DiffResult containing the differences to display.
///
/// # Returns
///
/// Returns `Ok(())` on successful output, or an error if JSON serialization or printing fails.
fn display_diff_json(result: &DiffResult) -> Result<()> {
    let added: Vec<_> = result
        .added
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "type": e.chunk_type.to_string(),
            })
        })
        .collect();

    let removed: Vec<_> = result
        .removed
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "type": e.chunk_type.to_string(),
            })
        })
        .collect();

    let modified: Vec<_> = result
        .modified
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "type": e.chunk_type.to_string(),
                "similarity": e.similarity,
            })
        })
        .collect();

    let output = serde_json::json!({
        "source": result.source,
        "target": result.target,
        "added": added,
        "removed": removed,
        "modified": modified,
        "summary": {
            "added": result.added.len(),
            "removed": result.removed.len(),
            "modified": result.modified.len(),
            "unchanged": result.unchanged_count,
        }
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
