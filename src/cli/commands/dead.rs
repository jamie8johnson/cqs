//! Dead code detection command

use std::path::Path;

use anyhow::Result;

use crate::cli::Cli;

/// Find functions/methods with no callers in the indexed codebase
pub(crate) fn cmd_dead(cli: &Cli, json: bool, include_pub: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_dead").entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let (confident, possibly_pub) = store.find_dead_code(include_pub)?;

    if json {
        display_dead_json(&confident, &possibly_pub, &root)?;
    } else {
        display_dead_text(&confident, &possibly_pub, &root, cli.quiet);
    }

    Ok(())
}

fn display_dead_text(
    confident: &[cqs::store::ChunkSummary],
    possibly_pub: &[cqs::store::ChunkSummary],
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
        for chunk in confident {
            let rel = cqs::rel_display(&chunk.file, root);
            println!(
                "  {} {}:{}  [{}]",
                chunk.name, rel, chunk.line_start, chunk.chunk_type
            );
            if !quiet {
                println!("    {}", chunk.signature.lines().next().unwrap_or(""));
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
        for chunk in possibly_pub {
            let rel = cqs::rel_display(&chunk.file, root);
            println!(
                "  {} {}:{}  [{}]",
                chunk.name, rel, chunk.line_start, chunk.chunk_type
            );
        }
    }
}

fn display_dead_json(
    confident: &[cqs::store::ChunkSummary],
    possibly_pub: &[cqs::store::ChunkSummary],
    root: &Path,
) -> Result<()> {
    let format_chunk = |chunk: &cqs::store::ChunkSummary| {
        serde_json::json!({
            "name": chunk.name,
            "file": cqs::rel_display(&chunk.file, root),
            "line_start": chunk.line_start,
            "line_end": chunk.line_end,
            "chunk_type": chunk.chunk_type.to_string(),
            "signature": chunk.signature,
            "language": chunk.language.to_string(),
        })
    };

    let result = serde_json::json!({
        "dead": confident.iter().map(&format_chunk).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_chunk).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    });

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
