//! Stale command for cqs
//!
//! Reports files that have changed since last index.
//!
//! Core struct is [`StaleOutput`]; build with [`build_stale`].
//! CLI uses text output for human display, batch serializes with `serde_json::to_value()`.

use std::collections::HashSet;

use anyhow::Result;

use cqs::store::StaleReport;
use cqs::Parser;

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct StaleEntry {
    pub file: String,
    pub stored_mtime: i64,
    pub current_mtime: i64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StaleOutput {
    pub stale: Vec<StaleEntry>,
    pub missing: Vec<String>,
    pub stale_count: usize,
    pub missing_count: usize,
    pub total_indexed: usize,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build the typed stale report shared between CLI and batch.
pub(crate) fn build_stale(report: &StaleReport) -> StaleOutput {
    let _span = tracing::info_span!("build_stale").entered();

    let stale = report
        .stale
        .iter()
        .map(|f| StaleEntry {
            file: cqs::normalize_path(&f.file),
            stored_mtime: f.stored_mtime,
            current_mtime: f.current_mtime,
        })
        .collect();

    let missing = report
        .missing
        .iter()
        .map(|f| cqs::normalize_path(f))
        .collect();

    StaleOutput {
        stale_count: report.stale.len(),
        missing_count: report.missing.len(),
        total_indexed: report.total_indexed as usize,
        stale,
        missing,
    }
}

// ---------------------------------------------------------------------------
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`stale_core`]. `count_only` only affects rendering / the
/// daemon's JSON projection — the core always returns the full
/// [`StaleOutput`]; adapters decide whether to drop the per-file lists.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct StaleArgs {
    /// Suppress the per-file `stale` / `missing` lists in the rendered output
    /// (counts only). The core ignores this — it is honored by the adapter so
    /// the typed schema stays complete for callers that want the file lists.
    #[serde(default)]
    pub count_only: bool,
}

/// Surface-agnostic core for `cqs stale`.
///
/// Diffs the supplied `file_set` against the index's recorded mtimes and
/// returns the full typed [`StaleOutput`]. The adapter owns file enumeration
/// so the hot daemon path can keep its cached `file_set` (re-enumerating on
/// every probe would be a perf regression); the CLI uses
/// [`enumerate_for_stale`] to build the set once. `count_only` lives on
/// [`StaleArgs`] for schema completeness but does not change what the core
/// computes — the adapter chooses how much to render (CLI hides the file list
/// in text mode; the daemon projects a 3-field count subset).
pub(crate) fn stale_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    root: &std::path::Path,
    file_set: &HashSet<std::path::PathBuf>,
    _args: &StaleArgs,
) -> Result<StaleOutput> {
    let _span = tracing::info_span!("stale_core").entered();
    let report = store.list_stale_files(file_set, root)?;
    Ok(build_stale(&report))
}

/// Enumerate the on-disk source files for the stale diff. CLI helper —
/// the daemon supplies its cached `file_set` directly to [`stale_core`].
pub(crate) fn enumerate_for_stale(root: &std::path::Path) -> Result<HashSet<std::path::PathBuf>> {
    let parser = Parser::new()?;
    let exts = parser.supported_extensions();
    let files = cqs::enumerate_files(root, &exts, false)?;
    Ok(files.into_iter().collect())
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

/// Report stale (modified) and missing files in the index
pub(crate) fn cmd_stale(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    json: bool,
    count_only: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_stale").entered();

    let store = &ctx.store;
    let root = &ctx.root;

    let args = StaleArgs { count_only };
    let file_set = enumerate_for_stale(root)?;
    let output = stale_core(store, root, &file_set, &args)?;

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        let stale_count = output.stale_count;
        let missing_count = output.missing_count;
        let total_indexed = output.total_indexed;

        if stale_count == 0 && missing_count == 0 {
            if !ctx.cli.quiet {
                println!(
                    "Index is fresh. {} file{} indexed.",
                    total_indexed,
                    if total_indexed == 1 { "" } else { "s" }
                );
            }
            return Ok(());
        }

        // Summary line
        if !ctx.cli.quiet {
            println!(
                "{} stale, {} missing (of {} indexed file{})",
                stale_count,
                missing_count,
                total_indexed,
                if total_indexed == 1 { "" } else { "s" }
            );
        }

        // File list (unless --count-only). `output` already carries
        // normalized paths (build_stale normalizes), so no re-normalize here.
        // Read `count_only` off the typed args so the field has a live reader
        // (it is otherwise adapter-honored, not core-honored).
        if !args.count_only && !ctx.cli.quiet {
            if !output.stale.is_empty() {
                println!("\nStale:");
                for f in &output.stale {
                    println!("  {}", f.file);
                }
            }
            if !output.missing.is_empty() {
                println!("\nMissing:");
                for f in &output.missing {
                    println!("  {}", f);
                }
            }
            println!("\nRun 'cqs index' to update.");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `StaleArgs` defaults must match the clap `args::StaleArgs` defaults so
    /// the wire surface and the core agree on the omitted `--count-only`
    /// behavior. Parses a real minimal `cqs stale` invocation.
    #[test]
    fn stale_args_default_matches_clap_defaults() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::StaleArgs,
        }
        let clap_args = Wrap::try_parse_from(["cqs-stale"]).unwrap().args;
        let core = StaleArgs {
            count_only: clap_args.count_only,
        };
        assert_eq!(
            core.count_only,
            StaleArgs::default().count_only,
            "clap stale default drifted from StaleArgs::default"
        );
    }

    /// `--count-only` flows into the core Args unchanged.
    #[test]
    fn stale_args_count_only_flag_parses() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::StaleArgs,
        }
        let clap_args = Wrap::try_parse_from(["cqs-stale", "--count-only"])
            .unwrap()
            .args;
        assert!(clap_args.count_only);
    }

    /// Empty-object deserialize (MCP no-params) yields the default args.
    #[test]
    fn stale_args_deserialize_empty_matches_default() {
        let from_empty: StaleArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty.count_only, StaleArgs::default().count_only);
    }

    #[test]
    fn test_stale_output_empty() {
        let output = StaleOutput {
            stale: vec![],
            missing: vec![],
            stale_count: 0,
            missing_count: 0,
            total_indexed: 50,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["stale_count"], 0);
        assert!(json["stale"].as_array().unwrap().is_empty());
        assert!(json["missing"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_stale_output_serialization() {
        let output = StaleOutput {
            stale: vec![StaleEntry {
                file: "src/main.rs".into(),
                stored_mtime: 1000,
                current_mtime: 2000,
            }],
            missing: vec!["src/deleted.rs".into()],
            stale_count: 1,
            missing_count: 1,
            total_indexed: 50,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["stale_count"], 1);
        assert_eq!(json["stale"][0]["file"], "src/main.rs");
        assert!(json.get("missing").is_some());
    }
}
