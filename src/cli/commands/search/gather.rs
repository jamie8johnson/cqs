//! Gather command — smart context assembly for a question

use anyhow::Result;
use colored::Colorize;

use cqs::Embedder;
use cqs::{gather, gather_cross_index_with_index, normalize_path, GatherDirection, GatherOptions};

use crate::cli::staleness;

/// Infrastructure context for gather commands.
pub(crate) struct GatherContext<'a> {
    pub ctx: &'a crate::cli::CommandContext<'a>,
    pub query: &'a str,
    pub expand: usize,
    pub direction: GatherDirection,
    pub limit: usize,
    pub max_tokens: Option<usize>,
    pub ref_name: Option<&'a str>,
    pub json: bool,
}

pub(crate) fn cmd_gather(gctx: &GatherContext<'_>) -> Result<()> {
    let ctx = gctx.ctx;
    let query = gctx.query;
    let expand = gctx.expand;
    let direction = gctx.direction;
    let limit = gctx.limit;
    let max_tokens = gctx.max_tokens;
    let ref_name = gctx.ref_name;
    let json = gctx.json;
    let _span = tracing::info_span!(
        "cmd_gather",
        query_len = query.len(),
        expand,
        limit,
        ?max_tokens,
        ?ref_name
    )
    .entered();

    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;
    let embedder = Embedder::new(ctx.model_config().clone())?;

    // When token-budgeted, fetch more chunks than limit so we have candidates to pack
    let fetch_limit = if max_tokens.is_some() {
        limit.max(50) // Fetch at least 50 candidates for token packing
    } else {
        limit
    };

    let opts = GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction,
        limit: fetch_limit,
        ..GatherOptions::default()
    };

    // Cross-index gather: seed from reference, bridge into project code
    let mut result = if let Some(rn) = ref_name {
        let query_embedding = embedder.embed_query(query)?;
        let ref_idx = crate::cli::commands::resolve::find_reference(root, rn)?;
        let index = crate::cli::build_vector_index(store, cqs_dir)?;
        gather_cross_index_with_index(
            store,
            &ref_idx,
            &query_embedding,
            query,
            &opts,
            root,
            index.as_deref(),
        )?
    } else {
        gather(store, &embedder, query, &opts, root)?
    };

    // Token-budgeted packing: keep highest-scoring chunks within token budget
    let token_count_used = if let Some(budget) = max_tokens {
        let _pack_span = tracing::info_span!("token_pack", budget).entered();

        let chunks = std::mem::take(&mut result.chunks);
        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let token_counts = crate::cli::commands::count_tokens_batch(&embedder, &texts);
        let overhead = if json {
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT
        } else {
            0
        };
        let (mut packed, used) =
            crate::cli::commands::token_pack(chunks, &token_counts, budget, overhead, |c| c.score);
        tracing::info!(
            chunks = packed.len(),
            tokens = used,
            budget,
            "Token-budgeted gather"
        );

        // Re-sort to reading order (ref first, then project, each in file/line order)
        packed.sort_by(|a, b| {
            let source_ord = match (&a.source, &b.source) {
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            };
            source_ord
                .then(a.file.cmp(&b.file))
                .then(a.line_start.cmp(&b.line_start))
                .then(a.name.cmp(&b.name))
        });
        result.chunks = packed;
        Some(used)
    } else {
        None
    };

    // Proactive staleness warning (only for project chunks)
    if !ctx.cli.quiet && !ctx.cli.no_stale_check && !result.chunks.is_empty() {
        let origins: Vec<&str> = result
            .chunks
            .iter()
            .filter(|c| c.source.is_none()) // only project chunks
            .filter_map(|c| c.file.to_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !origins.is_empty() {
            staleness::warn_stale_results(store, &origins, root);
        }
    }

    if json {
        let json_chunks: Vec<serde_json::Value> = result
            .chunks
            .iter()
            .filter_map(|c| match serde_json::to_value(c) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(error = %e, chunk = %c.name, "Failed to serialize chunk");
                    None
                }
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
        let ref_label = ref_name
            .map(|rn| format!(" (cross-index via '{}')", rn))
            .unwrap_or_default();
        println!(
            "Gathered {} chunk{}{}{} for: {}",
            result.chunks.len(),
            if result.chunks.len() == 1 { "" } else { "s" },
            ref_label,
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

        let is_cross_index = ref_name.is_some();
        let mut current_file = String::new();
        let mut current_source: Option<String> = None;
        for chunk in &result.chunks {
            // Show source headers only in cross-index mode
            if is_cross_index {
                let source_label = chunk.source.as_deref().unwrap_or("project").to_string();
                if Some(&source_label) != current_source.as_ref() {
                    if current_source.is_some() {
                        println!();
                    }
                    if chunk.source.is_some() {
                        println!("=== Reference: {} ===", source_label.yellow());
                    } else {
                        println!("=== Project ===");
                    }
                    current_source = Some(source_label);
                    current_file.clear();
                }
            }

            let file_str = normalize_path(&chunk.file);
            if file_str != current_file {
                if !current_file.is_empty() {
                    println!();
                }
                println!("--- {} ---", file_str.cyan());
                current_file = file_str;
            }
            let depth_label = if chunk.depth == 0 {
                if is_cross_index {
                    if chunk.source.is_some() {
                        "ref seed".to_string()
                    } else {
                        "bridge".to_string()
                    }
                } else {
                    "seed".to_string()
                }
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
