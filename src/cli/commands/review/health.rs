//! Health command — codebase quality snapshot

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;

use cqs::Parser;

// ---------------------------------------------------------------------------
// Args + core (surface-agnostic, MCP-ready)
// ---------------------------------------------------------------------------

/// Input for [`health_core`]. The health snapshot takes no user parameters
/// today; the struct exists so the core matches the established
/// `*Args` + `*_core` shape and can grow Deserialize-able fields without a
/// signature change (MCP-ready).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct HealthArgs {}

/// Surface-agnostic core for `cqs health`. Returns the typed
/// [`cqs::health::HealthReport`] (already the schema). The adapter owns file
/// enumeration so the hot daemon path can pass its cached `file_set` (the CLI
/// builds the set once via [`enumerate_for_health`]); mirrors the
/// `stale_core` split.
pub(crate) fn health_core(
    store: &cqs::Store<cqs::store::ReadOnly>,
    file_set: &HashSet<PathBuf>,
    cqs_dir: &Path,
    root: &Path,
    _args: &HealthArgs,
) -> Result<cqs::health::HealthReport> {
    let _span = tracing::info_span!("health_core").entered();
    Ok(cqs::health::health_check(store, file_set, cqs_dir, root)?)
}

/// Enumerate on-disk source files for the health staleness check. CLI helper —
/// the daemon supplies its cached `file_set` directly to [`health_core`].
pub(crate) fn enumerate_for_health(root: &Path) -> Result<HashSet<PathBuf>> {
    let parser = Parser::new()?;
    let files = crate::cli::enumerate_files(root, &parser, false)?;
    Ok(files.into_iter().collect())
}

pub(crate) fn cmd_health(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_health").entered();

    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    let file_set = enumerate_for_health(root)?;
    let report = health_core(&ctx.store, &file_set, cqs_dir, root, &HealthArgs::default())?;

    if json {
        let json_val = serde_json::to_value(&report)?;
        crate::cli::json_envelope::emit_json(&json_val)?;
    } else {
        // Dashboard display
        println!("{}", "Codebase Health".bold());
        println!("{}", "===============".bold());
        println!();

        // Index overview
        println!(
            "Index: {} chunks across {} files (schema v{}, {})",
            report.stats.total_chunks,
            report.stats.total_files,
            report.stats.schema_version,
            report.stats.model_name,
        );
        match report.hnsw_vectors {
            Some(v) => println!("HNSW:  {} vectors", v),
            None => println!("HNSW:  {}", "not built".yellow()),
        }
        println!(
            "Notes: {} ({} warnings)",
            report.note_count, report.note_warnings
        );

        // Staleness
        println!();
        if report.stale_count == 0 && report.missing_count == 0 {
            println!("Freshness: {}", "up to date".green());
        } else {
            if report.stale_count > 0 {
                println!(
                    "Freshness: {} stale file{}",
                    report.stale_count.to_string().yellow(),
                    if report.stale_count == 1 { "" } else { "s" },
                );
            }
            if report.missing_count > 0 {
                println!(
                    "           {} missing file{}",
                    report.missing_count.to_string().red(),
                    if report.missing_count == 1 { "" } else { "s" },
                );
            }
        }

        // Dead code
        println!();
        if report.dead_confident == 0 && report.dead_possible == 0 {
            println!("Dead code: {}", "none detected".green());
        } else {
            println!(
                "Dead code: {} confident, {} possible",
                if report.dead_confident > 0 {
                    report.dead_confident.to_string().red().to_string()
                } else {
                    "0".to_string()
                },
                report.dead_possible,
            );
        }

        // Hotspots
        if !report.hotspots.is_empty() {
            println!();
            println!("{}:", "Top hotspots".cyan());
            for h in &report.hotspots {
                println!("  {} ({} callers)", h.name, h.caller_count);
            }
        }

        // Untested hotspots (high-risk)
        if !report.untested_hotspots.is_empty() {
            println!();
            println!(
                "{} ({}):",
                "Untested hotspots".red().bold(),
                report.untested_hotspots.len()
            );
            for h in &report.untested_hotspots {
                println!(
                    "  {} ({} callers, {} tests)",
                    h.name,
                    h.caller_count,
                    "0".red()
                );
            }
        }

        // Warnings from degraded queries
        if !report.warnings.is_empty() {
            println!();
            for w in &report.warnings {
                eprintln!("{} {}", "Warning:".yellow().bold(), w);
            }
        }
    }

    Ok(())
}
