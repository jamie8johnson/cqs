//! Dead code detection command

use std::path::Path;

use anyhow::Result;
use cqs::store::{DeadConfidence, DeadFunction};

use crate::cli::Cli;

/// Find functions/methods with no callers in the indexed codebase
pub(crate) fn cmd_dead(
    cli: &Cli,
    json: bool,
    include_pub: bool,
    min_level: DeadConfidence,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_dead").entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let (confident, possibly_pub) = store.find_dead_code(include_pub)?;

    // Filter by minimum confidence
    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= min_level)
        .collect();

    if json {
        display_dead_json(&confident, &possibly_pub, &root)?;
    } else {
        display_dead_text(&confident, &possibly_pub, &root, cli.quiet);
    }

    Ok(())
}

/// Human-readable confidence label
fn confidence_label(c: DeadConfidence) -> &'static str {
    match c {
        DeadConfidence::High => "high",
        DeadConfidence::Medium => "medium",
        DeadConfidence::Low => "low",
    }
}

fn display_dead_text(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
    quiet: bool,
) {
    if confident.is_empty() && possibly_pub.is_empty() {
        println!("No dead code found.");
        return;
    }

    if !confident.is_empty() {
        if !quiet {
            println!("Dead code ({} functions):", confident.len());
            println!();
        }
        for dead in confident {
            let rel = cqs::rel_display(&dead.chunk.file, root);
            println!(
                "  {} {}:{}  [{}] ({})",
                dead.chunk.name,
                rel,
                dead.chunk.line_start,
                dead.chunk.chunk_type,
                confidence_label(dead.confidence),
            );
            if !quiet {
                println!("    {}", dead.chunk.signature.lines().next().unwrap_or(""));
            }
        }
    }

    if !possibly_pub.is_empty() {
        if !confident.is_empty() {
            println!();
        }
        println!(
            "Possibly dead (public API, {} functions):",
            possibly_pub.len()
        );
        if !quiet {
            println!("  (Use --include-pub to include these in the main list)");
        }
        println!();
        for dead in possibly_pub {
            let rel = cqs::rel_display(&dead.chunk.file, root);
            println!(
                "  {} {}:{}  [{}] ({})",
                dead.chunk.name,
                rel,
                dead.chunk.line_start,
                dead.chunk.chunk_type,
                confidence_label(dead.confidence),
            );
        }
    }
}

fn display_dead_json(
    confident: &[DeadFunction],
    possibly_pub: &[DeadFunction],
    root: &Path,
) -> Result<()> {
    let format_dead = |dead: &DeadFunction| {
        serde_json::json!({
            "name": dead.chunk.name,
            "file": cqs::rel_display(&dead.chunk.file, root),
            "line_start": dead.chunk.line_start,
            "line_end": dead.chunk.line_end,
            "chunk_type": dead.chunk.chunk_type.to_string(),
            "signature": dead.chunk.signature,
            "language": dead.chunk.language.to_string(),
            "confidence": confidence_label(dead.confidence),
        })
    };

    let result = serde_json::json!({
        "dead": confident.iter().map(&format_dead).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_dead).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    });

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
