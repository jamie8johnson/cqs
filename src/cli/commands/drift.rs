//! Drift command — semantic change detection between reference snapshots

use anyhow::{bail, Result};
use colored::Colorize;

use cqs::Store;

use crate::cli::find_project_root;

/// Detect semantic drift between a reference and the current project.
pub(crate) fn cmd_drift(
    reference: &str,
    threshold: f32,
    min_drift: f32,
    lang: Option<&str>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    crate::cli::validate_finite_f32(threshold, "threshold")?;
    crate::cli::validate_finite_f32(min_drift, "min-drift")?;
    let _span = tracing::info_span!("cmd_drift", reference).entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);

    // Load config to find reference path
    let config = cqs::config::Config::load(&root);

    let ref_cfg = config
        .references
        .iter()
        .find(|r| r.name == reference)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                reference
            )
        })?;

    let ref_db = ref_cfg.path.join("index.db");
    if !ref_db.exists() {
        bail!(
            "Reference '{}' has no index at {}. Run 'cqs ref update {}' first.",
            reference,
            ref_db.display(),
            reference
        );
    }
    let ref_store = Store::open_readonly(&ref_db)?;

    let index_path = cqs_dir.join("index.db");
    if !index_path.exists() {
        bail!("Project index not found. Run 'cqs init && cqs index' first.");
    }
    let project_store = Store::open(&index_path)?;

    let result = cqs::drift::detect_drift(
        &ref_store,
        &project_store,
        reference,
        threshold,
        min_drift,
        lang,
    )?;

    if json {
        let mut drifted_json: Vec<_> = result
            .drifted
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "file": e.file,
                    "chunk_type": e.chunk_type,
                    "similarity": e.similarity,
                    "drift": e.drift,
                })
            })
            .collect();
        if let Some(lim) = limit {
            drifted_json.truncate(lim);
        }
        let output = serde_json::json!({
            "reference": result.reference,
            "threshold": result.threshold,
            "min_drift": result.min_drift,
            "drifted": drifted_json,
            "total_compared": result.total_compared,
            "unchanged": result.unchanged,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "Drift from {} (threshold: {:.2}, showing \u{2265}{:.2} drift)\n",
            reference.bold(),
            threshold,
            min_drift
        );

        let entries = if let Some(lim) = limit {
            &result.drifted[..result.drifted.len().min(lim)]
        } else {
            &result.drifted
        };

        if entries.is_empty() {
            println!("  No drift detected.");
        } else {
            for entry in entries {
                println!(
                    "  {:.2}  {}  {}  {}",
                    entry.drift,
                    entry.name,
                    entry.file.dimmed(),
                    entry.chunk_type.dimmed()
                );
            }
        }

        println!(
            "\n{} drifted of {} compared ({} unchanged)",
            result.drifted.len(),
            result.total_compared,
            result.unchanged
        );
    }

    Ok(())
}
