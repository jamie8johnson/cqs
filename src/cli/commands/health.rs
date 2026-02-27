//! Health command — codebase quality snapshot

use std::collections::HashSet;

use anyhow::Result;
use colored::Colorize;

use cqs::Parser;

pub(crate) fn cmd_health(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_health").entered();

    let (store, root, cqs_dir) = crate::cli::open_project_store()?;

    // Enumerate current files for staleness check
    let parser = Parser::new()?;
    let files = crate::cli::enumerate_files(&root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    let report = cqs::health::health_check(&store, &file_set, &cqs_dir)?;

    if json {
        let json_val = serde_json::to_value(&report)?;
        println!("{}", serde_json::to_string_pretty(&json_val)?);
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
