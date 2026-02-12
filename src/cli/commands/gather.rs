//! Gather command — smart context assembly for a question

use anyhow::Result;
use colored::Colorize;

use cqs::Embedder;
use cqs::{gather, GatherDirection, GatherOptions};

use crate::cli::staleness;

pub(crate) fn cmd_gather(
    cli: &crate::cli::Cli,
    query: &str,
    expand: usize,
    direction: &str,
    limit: usize,
    max_tokens: Option<usize>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!(
        "cmd_gather",
        query_len = query.len(),
        expand,
        limit,
        ?max_tokens
    )
    .entered();

    let (store, root, _) = crate::cli::open_project_store()?;
    let embedder = Embedder::new()?;
    let query_embedding = embedder.embed_query(query)?;

    let dir: GatherDirection = direction
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    // When token-budgeted, fetch more chunks than limit so we have candidates to pack
    let fetch_limit = if max_tokens.is_some() {
        limit.max(50) // Fetch at least 50 candidates for token packing
    } else {
        limit
    };

    let opts = GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction: dir,
        limit: fetch_limit,
        ..GatherOptions::default()
    };

    let mut result = gather(&store, &query_embedding, query, &opts, &root)?;

    // Token-budgeted packing: keep highest-scoring chunks within token budget
    let token_count_used = if let Some(budget) = max_tokens {
        let _pack_span = tracing::info_span!("token_pack", budget).entered();
        // Chunks are in file/line order after gather. Re-sort by score for greedy packing.
        result.chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut used: usize = 0;
        let mut packed = Vec::new();
        for chunk in result.chunks {
            let tokens = embedder.token_count(&chunk.content).unwrap_or_else(|e| {
                tracing::warn!(error = %e, chunk = %chunk.name, "Token count failed, estimating");
                chunk.content.len() / 4 // rough fallback
            });
            if used + tokens > budget && !packed.is_empty() {
                break; // budget exhausted (always include at least 1 chunk)
            }
            used += tokens;
            packed.push(chunk);
        }
        tracing::info!(
            chunks = packed.len(),
            tokens = used,
            budget,
            "Token-budgeted gather"
        );

        // Re-sort to reading order
        packed.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line_start.cmp(&b.line_start))
                .then(a.name.cmp(&b.name))
        });
        result.chunks = packed;
        Some(used)
    } else {
        None
    };

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
            staleness::warn_stale_results(&store, &origins, &root);
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
        let mut output = serde_json::json!({
            "query": query,
            "chunks": json_chunks,
            "expansion_capped": result.expansion_capped,
            "search_degraded": result.search_degraded,
        });
        if let Some(tokens) = token_count_used {
            output["token_count"] = serde_json::json!(tokens);
            output["token_budget"] = serde_json::json!(max_tokens.unwrap_or(0));
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if result.chunks.is_empty() {
        println!("No relevant code found for: {}", query);
    } else {
        let token_info = match (token_count_used, max_tokens) {
            (Some(used), Some(budget)) => format!(" ({} of {} tokens)", used, budget),
            _ => String::new(),
        };
        println!(
            "Gathered {} chunk{}{} for: {}",
            result.chunks.len(),
            if result.chunks.len() == 1 { "" } else { "s" },
            token_info,
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
