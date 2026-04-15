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

    // Enumerate current files on disk
    let parser = Parser::new()?;
    let exts = parser.supported_extensions();
    let files = cqs::enumerate_files(root, &exts, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    let report = store.list_stale_files(&file_set, root)?;

    if json {
        let output = build_stale(&report);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let stale_count = report.stale.len();
        let missing_count = report.missing.len();

        if stale_count == 0 && missing_count == 0 {
            if !ctx.cli.quiet {
                println!(
                    "Index is fresh. {} file{} indexed.",
                    report.total_indexed,
                    if report.total_indexed == 1 { "" } else { "s" }
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
                report.total_indexed,
                if report.total_indexed == 1 { "" } else { "s" }
            );
        }

        // File list (unless --count-only)
        if !count_only && !ctx.cli.quiet {
            if !report.stale.is_empty() {
                println!("\nStale:");
                for f in &report.stale {
                    println!("  {}", cqs::normalize_path(&f.file));
                }
            }
            if !report.missing.is_empty() {
                println!("\nMissing:");
                for f in &report.missing {
                    println!("  {}", cqs::normalize_path(f));
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
