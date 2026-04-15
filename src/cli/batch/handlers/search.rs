//! Search dispatch handler.

use anyhow::{Context, Result};

use super::super::types::ChunkOutput;
use super::super::BatchContext;
use crate::cli::args::SearchArgs;

/// Dispatches a search query and returns results as JSON.
/// #947: takes the shared `SearchArgs` directly, no batch-local `SearchParams`
/// redirection. Both CLI top-level search and batch search deserialize into
/// the same struct, eliminating per-field drift as a possibility.
/// # Arguments
/// * `ctx` - The batch processing context containing the store and embedder
/// * `args` - Parsed search arguments (shared with CLI top-level)
/// # Returns
/// A `Result` containing a JSON object with:
/// * `results` - Array of matching search results
/// * `query` - The original query string
/// * `total` - Number of results returned
/// # Errors
/// Returns an error if:
/// * The embedder cannot be initialized
/// * Query embedding fails
/// * The language parameter is invalid
/// * Store operations fail
pub(in crate::cli::batch) fn dispatch_search(
    ctx: &BatchContext,
    args: &SearchArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query = %args.query).entered();

    // Accepted for CLI parity; batch JSON doesn't use line-context, parent
    // expansion, include-docs, pattern, or no-stale-check yet. Assigning to
    // `_` avoids clippy unused-field warnings while preserving forwards
    // compat: when the batch path wires these up, removing the `_ =` line
    // makes the compiler surface remaining call-sites.
    let _ = (args.context, args.expand, args.no_stale_check);
    let _ = (args.include_docs, args.pattern.as_ref());

    if args.name_only {
        let results = ctx
            .store()
            .search_by_name(&args.query, args.limit.clamp(1, 100))?;
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::to_value(ChunkOutput::from_search_result(r, false))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, name = %r.chunk.name, "ChunkOutput serialization failed (NaN score?)");
                        serde_json::json!({"error": "serialization failed", "name": r.chunk.name})
                    })
            })
            .collect();
        return Ok(serde_json::json!({
            "results": json_results,
            "query": args.query,
            "total": json_results.len(),
        }));
    }

    let embedder = ctx.embedder()?;
    let query_embedding = embedder
        .embed_query(&args.query)
        .context("Failed to embed query")?;

    let languages = match &args.lang {
        Some(l) => Some(vec![l
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid language '{}'", l))?]),
        None => None,
    };

    let limit = args.limit.clamp(1, 100);
    let effective_limit = if args.rerank {
        (limit * 4).min(100)
    } else {
        limit
    };

    // Parse include/exclude type filters (CQ-5)
    let include_types = match &args.include_type {
        Some(types) => {
            let parsed: Result<Vec<cqs::parser::ChunkType>, _> =
                types.iter().map(|t| t.parse()).collect();
            Some(parsed.map_err(|e| anyhow::anyhow!("Invalid --include-type: {e}"))?)
        }
        None => Some(cqs::parser::ChunkType::code_types()),
    };
    let exclude_types = match &args.exclude_type {
        Some(types) => {
            let parsed: Result<Vec<cqs::parser::ChunkType>, _> =
                types.iter().map(|t| t.parse()).collect();
            Some(parsed.map_err(|e| anyhow::anyhow!("Invalid --exclude-type: {e}"))?)
        }
        None => None,
    };

    // Classify query for per-category routing (SPLADE alpha + base/enriched index).
    let classification = cqs::search::router::classify_query(&args.query);

    // Per-category SPLADE routing: if --splade flag is set, use it directly.
    // Otherwise, resolve per-category alpha from classification.
    let (use_splade, splade_alpha) = if args.splade {
        (true, args.splade_alpha)
    } else {
        (
            true,
            cqs::search::router::resolve_splade_alpha(&classification.category),
        )
    };

    // Phase 5: base/enriched index routing.
    let use_base = matches!(
        classification.strategy,
        cqs::search::router::SearchStrategy::DenseBase
    ) || std::env::var("CQS_FORCE_BASE_INDEX").as_deref() == Ok("1");

    let filter = cqs::SearchFilter {
        languages,
        include_types,
        exclude_types,
        path_pattern: args.path.clone(),
        name_boost: args.name_boost,
        query_text: args.query.clone(),
        enable_rrf: args.rrf,
        enable_demotion: !args.no_demote,
        enable_splade: use_splade,
        splade_alpha,
        type_boost_types: classification.type_hints.clone(),
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    // --ref scoped search: search only the named reference
    if let Some(ref ref_name) = args.ref_name {
        let ref_idx = crate::cli::commands::resolve::find_reference(&ctx.root, ref_name)?;
        let ref_limit = if args.rerank {
            (limit * 4).min(100)
        } else {
            limit
        };
        let threshold = args.threshold;
        let mut results = cqs::reference::search_reference(
            &ref_idx,
            &query_embedding,
            &filter,
            ref_limit,
            threshold,
            false,
        )?;

        // Re-rank ref results
        if args.rerank && results.len() > 1 {
            let reranker = ctx.reranker()?;
            reranker
                .rerank(&args.query, &mut results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }

        let show_content = !args.no_content;
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::to_value(ChunkOutput::from_search_result(r, show_content))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, name = %r.chunk.name, "ChunkOutput serialization failed (NaN score?)");
                        serde_json::json!({"error": "serialization failed", "name": r.chunk.name})
                    })
            })
            .collect();

        return Ok(serde_json::json!({
            "results": json_results,
            "query": args.query,
            "total": json_results.len(),
            "source": ref_name,
        }));
    }

    // SPLADE sparse encoding (if enabled by --splade flag OR per-category routing)
    let splade_query = if use_splade {
        ctx.splade_encoder()
            .and_then(|enc| match enc.encode(&args.query) {
                Ok(sv) => Some(sv),
                Err(e) => {
                    tracing::warn!(error = %e, "SPLADE query encoding failed, falling back to cosine-only");
                    None
                }
            })
    } else {
        None
    };
    if use_splade {
        ctx.ensure_splade_index();
    }

    let audit_mode = ctx.audit_state();

    let index = if use_base {
        match ctx.base_vector_index()? {
            Some(base_idx) => {
                tracing::info!(
                    category = %classification.category,
                    "Router selected base HNSW for non-enriched query (batch)"
                );
                Some(base_idx)
            }
            None => {
                tracing::info!("Base HNSW unavailable — falling back to enriched index (batch)");
                ctx.vector_index()?
            }
        }
    } else {
        ctx.vector_index()?
    };
    let index = index.as_deref();

    let splade_index_ref = ctx.borrow_splade_index();

    let splade_arg = splade_query
        .as_ref()
        .and_then(|sq| splade_index_ref.as_ref().map(|si| (si, sq)));

    let threshold = args.threshold;
    let results = if audit_mode.is_active() || splade_arg.is_some() {
        let code_results = ctx.store().search_hybrid(
            &query_embedding,
            &filter,
            effective_limit,
            threshold,
            index,
            splade_arg,
        )?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        ctx.store().search_unified_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            threshold,
            index,
        )?
    };

    // Re-rank if requested
    let results = if args.rerank && results.len() > 1 {
        let mut code_results: Vec<cqs::store::SearchResult> = results
            .into_iter()
            .map(|r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr,
            })
            .collect();
        let reranker = ctx.reranker()?;
        reranker
            .rerank(&args.query, &mut code_results, limit)
            .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        results
    };

    // --include-refs: merge reference results
    let results = if args.include_refs {
        let config = cqs::config::Config::load(&ctx.root);
        let references = cqs::reference::load_references(&config.references);
        if !references.is_empty() {
            use rayon::prelude::*;
            let ref_results: Vec<_> = references
                .par_iter()
                .filter_map(|ref_idx| {
                    match cqs::reference::search_reference(
                        ref_idx,
                        &query_embedding,
                        &filter,
                        limit,
                        threshold,
                        true,
                    ) {
                        Ok(r) if !r.is_empty() => Some((ref_idx.name.clone(), r)),
                        Err(e) => {
                            tracing::warn!(reference = %ref_idx.name, error = %e, "Reference search failed");
                            None
                        }
                        _ => None,
                    }
                })
                .collect();
            let tagged = cqs::reference::merge_results(results, ref_results, limit);
            tagged.into_iter().map(|t| t.result).collect()
        } else {
            results
        }
    } else {
        results
    };

    // Token-budget packing (shared with CLI search)
    let (results, token_info) = if let Some(budget) = args.tokens {
        let embedder = ctx.embedder()?;
        crate::cli::commands::token_pack_results(
            results,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            embedder,
            |r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
            },
            |r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.score,
            },
            "batch_search",
        )
    } else {
        (results, None)
    };

    let show_content = !args.no_content;
    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| match r {
            cqs::store::UnifiedResult::Code(sr) => {
                serde_json::to_value(ChunkOutput::from_search_result(sr, show_content))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, name = %sr.chunk.name, "ChunkOutput serialization failed (NaN score?)");
                        serde_json::json!({"error": "serialization failed", "name": sr.chunk.name})
                    })
            }
        })
        .collect();

    let mut response = serde_json::json!({
        "results": json_results,
        "query": args.query,
        "total": json_results.len(),
    });
    crate::cli::commands::inject_token_info(&mut response, token_info);
    Ok(response)
}
