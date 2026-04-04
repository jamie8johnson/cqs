//! Drift command — semantic change detection between reference snapshots

use anyhow::{bail, Context, Result};
use colored::Colorize;

use cqs::Store;

use crate::cli::find_project_root;

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// A single entry in the drift output.
#[derive(Debug, serde::Serialize)]
struct DriftEntryOutput {
    name: String,
    file: String,
    chunk_type: String,
    similarity: f32,
    drift: f32,
}

/// Top-level JSON output for the drift command.
#[derive(Debug, serde::Serialize)]
struct DriftOutput {
    reference: String,
    threshold: f32,
    min_drift: f32,
    drifted: Vec<DriftEntryOutput>,
    total_compared: usize,
    unchanged: usize,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build typed drift output from a `DriftResult`, applying optional limit.
fn build_drift_output(result: &cqs::drift::DriftResult, limit: Option<usize>) -> DriftOutput {
    let _span = tracing::info_span!(
        "build_drift_output",
        drifted = result.drifted.len(),
        total = result.total_compared,
    )
    .entered();

    let entries: Vec<DriftEntryOutput> = result
        .drifted
        .iter()
        .map(|e| DriftEntryOutput {
            name: e.name.clone(),
            file: e.file.display().to_string(),
            chunk_type: e.chunk_type.to_string(),
            similarity: e.similarity,
            drift: e.drift,
        })
        .collect();

    let drifted = if let Some(lim) = limit {
        entries.into_iter().take(lim).collect()
    } else {
        entries
    };

    DriftOutput {
        reference: result.reference.clone(),
        threshold: result.threshold,
        min_drift: result.min_drift,
        drifted,
        total_compared: result.total_compared,
        unchanged: result.unchanged,
    }
}

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

    let ref_store =
        crate::cli::commands::resolve::resolve_reference_store_readonly(&root, reference)?;

    let index_path = cqs_dir.join("index.db");
    if !index_path.exists() {
        bail!("Project index not found. Run 'cqs init && cqs index' first.");
    }
    let project_store = Store::open(&index_path)
        .with_context(|| format!("Failed to open project store at {}", index_path.display()))?;

    let result = cqs::drift::detect_drift(
        &ref_store,
        &project_store,
        reference,
        threshold,
        min_drift,
        lang,
    )?;

    if json {
        let output = build_drift_output(&result, limit);
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
                    entry.file.display().to_string().dimmed(),
                    entry.chunk_type.to_string().dimmed()
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_output_empty() {
        let output = DriftOutput {
            reference: "v1.0".into(),
            threshold: 0.9,
            min_drift: 0.05,
            drifted: vec![],
            total_compared: 10,
            unchanged: 10,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["reference"], "v1.0");
        let threshold = json["threshold"].as_f64().unwrap();
        assert!(
            (threshold - 0.9).abs() < 1e-6,
            "threshold was {}",
            threshold
        );
        assert!(json["drifted"].as_array().unwrap().is_empty());
        assert_eq!(json["total_compared"], 10);
        assert_eq!(json["unchanged"], 10);
    }

    #[test]
    fn drift_output_serialization() {
        let output = DriftOutput {
            reference: "v2.0".into(),
            threshold: 0.85,
            min_drift: 0.1,
            drifted: vec![DriftEntryOutput {
                name: "search".into(),
                file: "src/search.rs".into(),
                chunk_type: "Function".into(),
                similarity: 0.75,
                drift: 0.25,
            }],
            total_compared: 50,
            unchanged: 49,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["drifted"][0]["name"], "search");
        assert_eq!(json["drifted"][0]["file"], "src/search.rs");
        let drift = json["drifted"][0]["drift"].as_f64().unwrap();
        assert!((drift - 0.25).abs() < 1e-6, "drift was {}", drift);
        let sim = json["drifted"][0]["similarity"].as_f64().unwrap();
        assert!((sim - 0.75).abs() < 1e-6, "similarity was {}", sim);
        assert_eq!(json["drifted"][0]["chunk_type"], "Function");
    }
}
