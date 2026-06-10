//! Diff command — semantic diff between indexed snapshots
//!
//! ## Command-core split (Phase 2b)
//!
//! [`diff_core`] owns the surface-agnostic diff logic: it takes the two
//! already-resolved stores plus a typed [`DiffArgs`] and returns the typed
//! [`DiffOutput`] (the single JSON-schema source). Store resolution
//! (`project` vs. a named reference) needs config load + the reference LRU, so
//! it stays in each adapter (CLI [`cmd_diff`] / daemon `dispatch_diff`); both
//! drive the same core afterward, so the wire shape is identical.

use anyhow::{bail, Context, Result};
use colored::Colorize;

use cqs::Store;
use cqs::{normalize_path, semantic_diff, DiffResult};

use crate::cli::find_project_root;

// ---------------------------------------------------------------------------
// Args (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`diff_core`] — the diff knobs both the CLI and a future MCP
/// `diff` tool deserialize into. Store resolution (which reference / project)
/// is the adapter's job; the core takes the resolved stores plus these
/// settings.
///
/// `#[serde(default)]` so a wire caller can omit `target`/`lang` and inherit
/// the production defaults; `threshold` defaults to the clap value.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct DiffArgs {
    /// Source reference name (echoed into the output label).
    pub source: String,
    /// Target label — `project` or another reference name.
    pub target: String,
    /// Similarity threshold for the "modified" bucket: pairs above are
    /// unchanged, below are modified.
    pub threshold: f32,
    /// Restrict the comparison to this language (e.g. `rust`).
    pub lang: Option<String>,
}

impl Default for DiffArgs {
    fn default() -> Self {
        // Mirrors clap's `DiffArgs` defaults (threshold 0.95, target "project").
        DiffArgs {
            source: String::new(),
            target: "project".to_string(),
            threshold: 0.95,
            lang: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// A single entry in the diff output (added, removed, or modified).
#[derive(Debug, serde::Serialize)]
pub(crate) struct DiffEntryOutput {
    name: String,
    file: String,
    #[serde(rename = "type")]
    chunk_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity: Option<f32>,
}

/// Summary counts for the diff.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DiffSummary {
    added: usize,
    removed: usize,
    modified: usize,
    unchanged: usize,
}

/// Top-level JSON output for the diff command — the single JSON-schema source
/// shared by the CLI and daemon adapters.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DiffOutput {
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

// ---------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------

/// Surface-agnostic core for `cqs diff`. Takes the two already-resolved stores
/// (the adapter owns reference/project resolution) plus a [`DiffArgs`], runs
/// the `semantic_diff` primitive, and assembles the typed [`DiffOutput`].
/// Reads no env and never prints — the adapter renders text or JSON from the
/// returned struct.
pub(crate) fn diff_core<Mode1, Mode2>(
    source_store: &Store<Mode1>,
    target_store: &Store<Mode2>,
    args: &DiffArgs,
) -> Result<DiffOutput> {
    let _span =
        tracing::info_span!("diff_core", source = %args.source, target = %args.target).entered();
    let result = semantic_diff(
        source_store,
        target_store,
        &args.source,
        &args.target,
        args.threshold,
        args.lang.as_deref(),
    )?;
    Ok(build_diff_output(&result))
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
        let index_path = cqs::resolve_index_db(&cqs_dir);
        if !index_path.exists() {
            bail!("Project index not found. Run 'cqs init && cqs index' first.");
        }
        Store::open(&index_path)
            .with_context(|| format!("Failed to open project store at {}", index_path.display()))?
    } else {
        crate::cli::commands::resolve::resolve_reference_store(&root, target_label)?
    };

    let args = DiffArgs {
        source: source.to_string(),
        target: target_label.to_string(),
        threshold,
        lang: lang.map(str::to_string),
    };
    let output = diff_core(&source_store, &target_store, &args)?;

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        display_diff_from_output(&output);
    }

    Ok(())
}

/// Displays a formatted diff report from the typed [`DiffOutput`] — the same
/// struct the JSON path serializes, so text and JSON share one data source.
fn display_diff_from_output(output: &DiffOutput) {
    println!("Diff: {} → {}", output.source.bold(), output.target.bold());
    println!();

    if !output.added.is_empty() {
        println!("{} ({}):", "Added".green().bold(), output.added.len());
        for entry in &output.added {
            println!("  + {} {} ({})", entry.chunk_type, entry.name, entry.file);
        }
        println!();
    }

    if !output.removed.is_empty() {
        println!("{} ({}):", "Removed".red().bold(), output.removed.len());
        for entry in &output.removed {
            println!("  - {} {} ({})", entry.chunk_type, entry.name, entry.file);
        }
        println!();
    }

    if !output.modified.is_empty() {
        println!(
            "{} ({}):",
            "Modified".yellow().bold(),
            output.modified.len()
        );
        for entry in &output.modified {
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
        output.summary.added,
        output.summary.removed,
        output.summary.modified,
        output.summary.unchanged,
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A wire/MCP caller can supply only `source` and inherit the production
    /// defaults via `#[serde(default)]`.
    #[test]
    fn diff_args_deserialize_minimal() {
        let args: DiffArgs = serde_json::from_str(r#"{"source": "v1.0"}"#).unwrap();
        assert_eq!(args.source, "v1.0");
        assert_eq!(args.target, "project");
        assert!((args.threshold - 0.95).abs() < 1e-6);
        assert!(args.lang.is_none());
    }

    /// `DiffArgs::default` must match the clap `DiffArgs` defaults exactly so
    /// the wire surface and the CLI agree on omitted-field behavior. Parses a
    /// real minimal CLI invocation (`cqs diff <source>`) via a throwaway
    /// `clap::Parser` wrapper around the shared clap `DiffArgs`; a changed clap
    /// default breaks this test instead of silently diverging.
    #[test]
    fn diff_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::DiffArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-diff", "v1.0"]).unwrap().args;
        // Build the surface-agnostic args the way the adapter does.
        let core = DiffArgs {
            source: clap_args.source.clone(),
            target: clap_args.target.clone().unwrap_or_else(|| "project".into()),
            threshold: clap_args.threshold,
            lang: clap_args.lang.clone(),
        };
        let expected = DiffArgs {
            source: "v1.0".to_string(),
            ..DiffArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap diff defaults drifted from DiffArgs::default — update both together"
        );
    }

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

    // ===== NaN similarity serialization =====

    #[test]
    fn tc16_diff_entry_nan_similarity_becomes_null() {
        // serde_json silently converts NaN f32 to null in JSON output. If
        // semantic_diff produces a NaN similarity (e.g., identical-hash
        // chunks with zero-norm embeddings), the "similarity" field becomes
        // null instead of a number, which agents don't expect.
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
        // Full DiffOutput with NaN modified entry — verify silent null
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
