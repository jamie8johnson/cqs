//! Gather command — smart context assembly for a question

use anyhow::{bail, Result};
use colored::Colorize;

use cqs::{gather, GatherDirection, GatherOptions};
use cqs::{Embedder, Store};

use crate::cli::{find_project_root, staleness};

pub(crate) fn cmd_gather(
    cli: &crate::cli::Cli,
    query: &str,
    expand: usize,
    direction: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_gather", query_len = query.len(), expand, limit).entered();

    let root = find_project_root();
    let index_path = cqs::resolve_index_dir(&root).join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    let embedder = Embedder::new()?;
    let query_embedding = embedder.embed_query(query)?;

    let dir: GatherDirection = direction.parse()?;
    let opts = GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction: dir,
        limit,
        ..GatherOptions::default()
    };

    let result = gather(&store, &query_embedding, query, &opts, &root)?;

    // Proactive staleness warning
    if !cli.quiet && !cli.no_stale_check && !result.chunks.is_empty() {
        let origins: Vec<&str> = result
            .chunks
            .iter()
            .filter_map(|c| c.file.to_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !origins.is_empty() {
            staleness::warn_stale_results(&store, &origins);
        }
    }

    if json {
        let json_chunks: Vec<_> = result
            .chunks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": c.file.to_string_lossy().replace('\\', "/"),
                    "line_start": c.line_start,
                    "line_end": c.line_end,
                    "signature": c.signature,
                    "score": c.score,
                    "depth": c.depth,
                    "content": c.content,
                })
            })
            .collect();
        let output = serde_json::json!({
            "query": query,
            "chunks": json_chunks,
            "expansion_capped": result.expansion_capped,
            "search_degraded": result.search_degraded,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if result.chunks.is_empty() {
        println!("No relevant code found for: {}", query);
    } else {
        println!(
            "Gathered {} chunk{} for: {}",
            result.chunks.len(),
            if result.chunks.len() == 1 { "" } else { "s" },
            query.cyan(),
        );
        if result.expansion_capped {
            println!("{}", "Warning: expansion capped at 200 nodes".yellow());
        }
        if result.search_degraded {
            println!(
                "{}",
                "Warning: batch name search failed, results may be incomplete".yellow()
            );
        }
        println!();

        let mut current_file = String::new();
        for chunk in &result.chunks {
            let file_str = chunk.file.to_string_lossy().replace('\\', "/");
            if file_str != current_file {
                if !current_file.is_empty() {
                    println!();
                }
                println!("--- {} ---", file_str.cyan());
                current_file = file_str;
            }
            let depth_label = if chunk.depth == 0 {
                "seed".to_string()
            } else {
                format!("depth {}", chunk.depth)
            };
            println!(
                "  {} ({}:{}, {}, {:.3})",
                chunk.name.bold(),
                chunk.file.display(),
                chunk.line_start,
                depth_label,
                chunk.score,
            );
            println!("  {}", chunk.signature.dimmed());
        }
    }

    Ok(())
}
