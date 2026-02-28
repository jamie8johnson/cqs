//! Diff command — semantic diff between indexed snapshots

use anyhow::{bail, Result};
use colored::Colorize;

use cqs::Store;
use cqs::{semantic_diff, DiffResult};

use crate::cli::find_project_root;

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
        Store::open(&index_path)?
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

fn display_diff(result: &DiffResult) -> Result<()> {
    println!("Diff: {} → {}", result.source.bold(), result.target.bold());
    println!();

    if !result.added.is_empty() {
        println!("{} ({}):", "Added".green().bold(), result.added.len());
        for entry in &result.added {
            println!("  + {} {} ({})", entry.chunk_type, entry.name, entry.file);
        }
        println!();
    }

    if !result.removed.is_empty() {
        println!("{} ({}):", "Removed".red().bold(), result.removed.len());
        for entry in &result.removed {
            println!("  - {} {} ({})", entry.chunk_type, entry.name, entry.file);
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
                entry.chunk_type, entry.name, entry.file, sim
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

fn display_diff_json(result: &DiffResult) -> Result<()> {
    let added: Vec<_> = result
        .added
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file,
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
                "file": e.file,
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
                "file": e.file,
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
