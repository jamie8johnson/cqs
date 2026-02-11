//! Stale command for cqs
//!
//! Reports files that have changed since last index.

use std::collections::HashSet;

use anyhow::{bail, Result};

use cqs::{Parser, Store};

use crate::cli::{find_project_root, Cli};

/// Report stale (modified) and missing files in the index
pub(crate) fn cmd_stale(cli: &Cli, json: bool, count_only: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_stale").entered();

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

    // Enumerate current files on disk
    let parser = Parser::new()?;
    let files = cqs::enumerate_files(&root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    let report = store.list_stale_files(&file_set)?;

    if json {
        let stale_json: Vec<_> = report
            .stale
            .iter()
            .map(|f| {
                serde_json::json!({
                    "file": f.origin.replace('\\', "/"),
                    "stored_mtime": f.stored_mtime,
                    "current_mtime": f.current_mtime,
                })
            })
            .collect();

        let missing_json: Vec<_> = report
            .missing
            .iter()
            .map(|f| f.replace('\\', "/"))
            .collect();

        let result = serde_json::json!({
            "stale": stale_json,
            "missing": missing_json,
            "stale_count": report.stale.len(),
            "missing_count": report.missing.len(),
            "total_indexed": report.total_indexed,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let stale_count = report.stale.len();
        let missing_count = report.missing.len();

        if stale_count == 0 && missing_count == 0 {
            if !cli.quiet {
                println!(
                    "Index is fresh. {} file{} indexed.",
                    report.total_indexed,
                    if report.total_indexed == 1 { "" } else { "s" }
                );
            }
            return Ok(());
        }

        // Summary line
        if !cli.quiet {
            println!(
                "{} stale, {} missing (of {} indexed file{})",
                stale_count,
                missing_count,
                report.total_indexed,
                if report.total_indexed == 1 { "" } else { "s" }
            );
        }

        // File list (unless --count-only)
        if !count_only && !cli.quiet {
            if !report.stale.is_empty() {
                println!("\nStale:");
                for f in &report.stale {
                    println!("  {}", f.origin.replace('\\', "/"));
                }
            }
            if !report.missing.is_empty() {
                println!("\nMissing:");
                for f in &report.missing {
                    println!("  {}", f.replace('\\', "/"));
                }
            }
            println!("\nRun 'cqs index' to update.");
        }
    }

    Ok(())
}
