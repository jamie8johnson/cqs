//! Diff command — semantic diff between indexed snapshots

use anyhow::{bail, Context, Result};
use colored::Colorize;

use cqs::Store;
use cqs::{normalize_path, semantic_diff, DiffResult};

use crate::cli::find_project_root;

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// A single entry in the diff output (added, removed, or modified).
#[derive(Debug, serde::Serialize)]
struct DiffEntryOutput {
    name: String,
    file: String,
    #[serde(rename = "type")]
    chunk_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity: Option<f32>,
}

/// Summary counts for the diff.
#[derive(Debug, serde::Serialize)]
struct DiffSummary {
    added: usize,
    removed: usize,
    modified: usize,
    unchanged: usize,
}

/// Top-level JSON output for the diff command.
#[derive(Debug, serde::Serialize)]
struct DiffOutput {
    source: String,
    target: String,
    added: Vec<DiffEntryOutput>,
    removed: Vec<DiffEntryOutput>,
    modified: Vec<DiffEntryOutput>,
    summary: DiffSummary,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build typed diff output from a `DiffResult`.
fn build_diff_output(result: &DiffResult) -> DiffOutput {
    let _span = tracing::info_span!(
        "build_diff_output",
        added = result.added.len(),
        removed = result.removed.len(),
        modified = result.modified.len(),
    )
    .entered();

    let convert = |e: &cqs::DiffEntry, include_sim: bool| DiffEntryOutput {
        name: e.name.clone(),
        file: normalize_path(&e.file),
        chunk_type: e.chunk_type.to_string(),
        similarity: if include_sim { e.similarity } else { None },
    };

    DiffOutput {
        source: result.source.clone(),
        target: result.target.clone(),
        added: result.added.iter().map(|e| convert(e, false)).collect(),
        removed: result.removed.iter().map(|e| convert(e, false)).collect(),
        modified: result.modified.iter().map(|e| convert(e, true)).collect(),
        summary: DiffSummary {
            added: result.added.len(),
            removed: result.removed.len(),
            modified: result.modified.len(),
            unchanged: result.unchanged_count,
        },
    }
}

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
    let source_store = crate::cli::commands::resolve::resolve_reference_store(&root, source)?;

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
        crate::cli::commands::resolve::resolve_reference_store(&root, target_label)?
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
fn display_diff_json(result: &DiffResult) -> Result<()> {
    let output = build_diff_output(result);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_output_empty() {
        let output = DiffOutput {
            source: "v1.0".into(),
            target: "project".into(),
            added: vec![],
            removed: vec![],
            modified: vec![],
            summary: DiffSummary {
                added: 0,
                removed: 0,
                modified: 0,
                unchanged: 5,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["source"], "v1.0");
        assert_eq!(json["target"], "project");
        assert!(json["added"].as_array().unwrap().is_empty());
        assert_eq!(json["summary"]["unchanged"], 5);
    }

    #[test]
    fn diff_output_serialization() {
        let output = DiffOutput {
            source: "v1.0".into(),
            target: "v2.0".into(),
            added: vec![DiffEntryOutput {
                name: "new_fn".into(),
                file: "src/lib.rs".into(),
                chunk_type: "Function".into(),
                similarity: None,
            }],
            removed: vec![],
            modified: vec![DiffEntryOutput {
                name: "changed_fn".into(),
                file: "src/search.rs".into(),
                chunk_type: "Function".into(),
                similarity: Some(0.85),
            }],
            summary: DiffSummary {
                added: 1,
                removed: 0,
                modified: 1,
                unchanged: 10,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["added"][0]["name"], "new_fn");
        assert!(json["added"][0].get("similarity").is_none());
        let sim = json["modified"][0]["similarity"].as_f64().unwrap();
        assert!((sim - 0.85).abs() < 1e-6, "similarity was {}", sim);
        assert_eq!(json["modified"][0]["type"], "Function");
        assert_eq!(json["summary"]["added"], 1);
        assert_eq!(json["summary"]["modified"], 1);
    }

    // ===== TC-16: NaN similarity serialization =====

    #[test]
    fn tc16_diff_entry_nan_similarity_becomes_null() {
        // TC-16: serde_json silently converts NaN f32 to null in JSON output.
        // This is the actual gap: if semantic_diff produces a NaN similarity
        // (e.g., identical-hash chunks with zero-norm embeddings), the "similarity"
        // field becomes null instead of a number, which agents don't expect.
        let entry = DiffEntryOutput {
            name: "modified_fn".into(),
            file: "src/lib.rs".into(),
            chunk_type: "Function".into(),
            similarity: Some(f32::NAN),
        };

        // to_string_pretty (used by cmd_diff) silently converts NaN to null
        let json_str = serde_json::to_string_pretty(&entry).unwrap();
        assert!(
            json_str.contains("null"),
            "NaN similarity should serialize as null in JSON string"
        );

        // to_value also converts NaN to null
        let json = serde_json::to_value(&entry).unwrap();
        // Option<f32> with Some(NaN) becomes present but null -- NOT omitted by skip_serializing_if
        assert!(
            json.get("similarity").is_some(),
            "Some(NaN) should not be omitted by skip_serializing_if (Option::is_none is false)"
        );
        assert!(
            json["similarity"].is_null(),
            "NaN similarity should become null via to_value"
        );
    }

    #[test]
    fn tc16_diff_output_nan_modified_entry_produces_null() {
        // TC-16: Full DiffOutput with NaN modified entry — verify silent null
        let output = DiffOutput {
            source: "v1.0".into(),
            target: "v2.0".into(),
            added: vec![],
            removed: vec![],
            modified: vec![DiffEntryOutput {
                name: "changed_fn".into(),
                file: "src/lib.rs".into(),
                chunk_type: "Function".into(),
                similarity: Some(f32::NAN),
            }],
            summary: DiffSummary {
                added: 0,
                removed: 0,
                modified: 1,
                unchanged: 5,
            },
        };
        let json_str = serde_json::to_string_pretty(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // The modified entry's similarity should be null (not a number, not omitted)
        assert!(
            parsed["modified"][0]["similarity"].is_null(),
            "NaN similarity in DiffOutput should serialize as null"
        );
    }

    #[test]
    fn tc16_diff_entry_none_similarity_serializes_ok() {
        // Contrast: None similarity (added/removed entries) should serialize fine
        let entry = DiffEntryOutput {
            name: "new_fn".into(),
            file: "src/lib.rs".into(),
            chunk_type: "Function".into(),
            similarity: None,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(
            json.get("similarity").is_none(),
            "None similarity should be omitted via skip_serializing_if"
        );
    }

    #[test]
    fn tc16_diff_entry_boundary_similarity_values() {
        // Verify boundary values (0.0, 1.0) serialize correctly via both paths
        for &val in &[0.0f32, 1.0, -0.0, f32::MIN_POSITIVE] {
            let entry = DiffEntryOutput {
                name: "fn".into(),
                file: "f.rs".into(),
                chunk_type: "Function".into(),
                similarity: Some(val),
            };
            // to_string_pretty should succeed for valid floats
            let string_result = serde_json::to_string_pretty(&entry);
            assert!(
                string_result.is_ok(),
                "similarity {} should serialize via to_string_pretty",
                val
            );
            // to_value should also succeed
            let json = serde_json::to_value(&entry).unwrap();
            assert!(
                json["similarity"].is_number(),
                "similarity {} should be a number in JSON",
                val
            );
        }
    }
}
