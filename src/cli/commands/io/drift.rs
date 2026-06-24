//! Drift command — semantic change detection between reference snapshots
//!
//! ## Command-core split (Phase 2b)
//!
//! [`drift_core`] owns the surface-agnostic drift logic: it takes the
//! already-resolved reference + project stores plus a typed [`DriftArgs`] and
//! returns the typed [`DriftOutput`] (the single JSON-schema source). Store
//! resolution stays in each adapter (CLI [`cmd_drift`] / daemon
//! `dispatch_drift`); both drive the same core, so the wire shape is identical.

use anyhow::{bail, Context, Result};
use colored::Colorize;

use cqs::{normalize_path, Store};

use crate::cli::find_project_root;

// ---------------------------------------------------------------------------
// Args (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`drift_core`] — the drift knobs both the CLI and a future MCP
/// `drift` tool deserialize into. Store resolution is the adapter's job.
///
/// `#[serde(default)]` so a wire caller can supply just `reference` and inherit
/// the production defaults (matching clap's `DriftArgs`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct DriftArgs {
    /// Reference name to compare the project against (echoed into output).
    pub reference: String,
    /// Similarity threshold: pairs below are reported as drifted.
    pub threshold: f32,
    /// Minimum drift value to include in the output.
    pub min_drift: f32,
    /// Restrict the comparison to this language (e.g. `rust`).
    pub lang: Option<String>,
    /// Cap on the number of drifted entries returned (`None` = all).
    pub limit: Option<usize>,
}

impl Default for DriftArgs {
    fn default() -> Self {
        // Mirrors clap's `DriftArgs` defaults (threshold 0.95, min_drift 0.0).
        DriftArgs {
            reference: String::new(),
            threshold: 0.95,
            min_drift: 0.0,
            lang: None,
            limit: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// A single entry in the drift output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DriftEntryOutput {
    name: String,
    file: String,
    chunk_type: String,
    similarity: f32,
    drift: f32,
}

/// Top-level JSON output for the drift command — the single JSON-schema source
/// shared by the CLI and daemon adapters.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DriftOutput {
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
            file: normalize_path(&e.file),
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

// ---------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------

/// Surface-agnostic core for `cqs drift`. Takes the already-resolved reference
/// + project stores (the adapter owns resolution) plus a [`DriftArgs`], runs
/// the `detect_drift` primitive, and assembles the typed [`DriftOutput`]
/// (applying the optional limit). Reads no env and never prints.
pub(crate) fn drift_core<Mode1, Mode2>(
    ref_store: &Store<Mode1>,
    project_store: &Store<Mode2>,
    args: &DriftArgs,
) -> Result<DriftOutput> {
    let _span = tracing::info_span!("drift_core", reference = %args.reference).entered();
    let result = cqs::drift::detect_drift(
        ref_store,
        project_store,
        &args.reference,
        args.threshold,
        args.min_drift,
        args.lang.as_deref(),
    )?;
    Ok(build_drift_output(&result, args.limit))
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

    let index_path = cqs::resolve_index_db(&cqs_dir);
    if !index_path.exists() {
        bail!("Project index not found. Run 'cqs init && cqs index' first.");
    }
    let project_store = Store::open(&index_path)
        .with_context(|| format!("Failed to open project store at {}", index_path.display()))?;

    let args = DriftArgs {
        reference: reference.to_string(),
        threshold,
        min_drift,
        lang: lang.map(str::to_string),
        limit,
    };
    let result = cqs::drift::detect_drift(
        &ref_store,
        &project_store,
        &args.reference,
        args.threshold,
        args.min_drift,
        args.lang.as_deref(),
    )?;
    // Full (pre-limit) drifted count for the text summary — the limit only
    // truncates the displayed/serialized list, never the reported totals.
    let total_drifted = result.drifted.len();
    let output = build_drift_output(&result, args.limit);

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        println!(
            "Drift from {} (threshold: {:.2}, showing \u{2265}{:.2} drift)\n",
            reference.bold(),
            threshold,
            min_drift
        );

        if output.drifted.is_empty() {
            println!("  No drift detected.");
        } else {
            for entry in &output.drifted {
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
            total_drifted, output.total_compared, output.unchanged
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

    /// A wire/MCP caller can supply only `reference` and inherit defaults.
    #[test]
    fn drift_args_deserialize_minimal() {
        let args: DriftArgs = serde_json::from_str(r#"{"reference": "v1.0"}"#).unwrap();
        assert_eq!(args.reference, "v1.0");
        assert!((args.threshold - 0.95).abs() < 1e-6);
        assert!((args.min_drift - 0.0).abs() < 1e-6);
        assert!(args.lang.is_none());
        assert!(args.limit.is_none());
    }

    /// `DriftArgs::default` must match the clap `DriftArgs` defaults exactly.
    /// Parses `cqs drift <reference>` via a throwaway `clap::Parser` wrapper.
    #[test]
    fn drift_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::DriftArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-drift", "v1.0"]).unwrap().args;
        let core = DriftArgs {
            reference: clap_args.reference.clone(),
            threshold: clap_args.threshold,
            min_drift: clap_args.min_drift,
            lang: clap_args.lang.clone(),
            limit: clap_args.limit,
        };
        let expected = DriftArgs {
            reference: "v1.0".to_string(),
            ..DriftArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap drift defaults drifted from DriftArgs::default — update both together"
        );
    }

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

    fn make_drift_result(count: usize) -> cqs::drift::DriftResult {
        let drifted: Vec<cqs::drift::DriftEntry> = (0..count)
            .map(|i| cqs::drift::DriftEntry {
                name: format!("fn_{}", i),
                file: std::path::PathBuf::from(format!("src/mod_{}.rs", i)),
                chunk_type: cqs::parser::ChunkType::Function,
                similarity: 0.7 - (i as f32 * 0.05),
                drift: 0.3 + (i as f32 * 0.05),
            })
            .collect();
        cqs::drift::DriftResult {
            reference: "v3.0".into(),
            threshold: 0.9,
            min_drift: 0.1,
            drifted,
            total_compared: 20,
            unchanged: 20 - count,
        }
    }

    #[test]
    fn test_build_drift_output_no_limit() {
        let result = make_drift_result(5);
        let output = build_drift_output(&result, None);

        assert_eq!(output.drifted.len(), 5, "All 5 entries should be present");
        assert_eq!(output.total_compared, 20);
        assert_eq!(output.unchanged, 15);
        assert_eq!(output.reference, "v3.0");
        // Verify entry names preserved in order
        for (i, entry) in output.drifted.iter().enumerate() {
            assert_eq!(entry.name, format!("fn_{}", i));
        }
    }

    #[test]
    fn test_build_drift_output_with_limit() {
        let result = make_drift_result(5);
        let output = build_drift_output(&result, Some(2));

        assert_eq!(output.drifted.len(), 2, "Should be truncated to 2 entries");
        assert_eq!(
            output.total_compared, 20,
            "total_compared must not change with limit"
        );
        assert_eq!(output.unchanged, 15, "unchanged must not change with limit");
        assert_eq!(output.drifted[0].name, "fn_0");
        assert_eq!(output.drifted[1].name, "fn_1");
    }
}
