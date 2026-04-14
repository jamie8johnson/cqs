//! Search dispatch handler.

use anyhow::{Context, Result};

use super::super::types::ChunkOutput;
use super::super::BatchContext;

/// Parameters for batch search dispatch.
pub(in crate::cli::batch) struct SearchParams {
    pub query: String,
    pub limit: usize,
    pub name_only: bool,
    pub rrf: bool,
    pub rerank: bool,
    pub splade: bool,
    pub splade_alpha: f32,
    pub lang: Option<String>,
    pub path: Option<String>,
    pub include_type: Option<Vec<String>>,
    pub exclude_type: Option<Vec<String>>,
    pub tokens: Option<usize>,
    pub no_demote: bool,
    pub name_boost: f32,
    pub ref_name: Option<String>,
    pub include_refs: bool,
    pub no_content: bool,
    pub context: Option<usize>,
    pub expand: bool,
    pub no_stale_check: bool,
}

/// Dispatches a search query and returns results as JSON.
/// Performs either a name-only search or a full semantic search using embeddings. For name-only searches, queries the store directly by name. For semantic searches, embeds the query and retrieves results, optionally reranking them.
/// # Arguments
/// * `ctx` - The batch processing context containing the store and embedder
/// * `params` - Search parameters including query text, limit, language filter, and search mode
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
/// # Panics
/// Panics indirectly if JSON serialization fails unexpectedly (logs warning and returns error object instead for known cases).
pub(in crate::cli::batch) fn dispatch_search(
    ctx: &BatchContext,
    params: &SearchParams,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query = %params.query).entered();

    // Accepted for CLI parity; batch JSON doesn't use line-context or parent expansion yet
    let _ = (params.context, params.expand, params.no_stale_check);

    if params.name_only {
        let results = ctx
            .store()
            .search_by_name(&params.query, params.limit.clamp(1, 100))?;
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
            "query": params.query,
            "total": json_results.len(),
        }));
    }

    let embedder = ctx.embedder()?;
    let query_embedding = embedder
        .embed_query(&params.query)
        .context("Failed to embed query")?;

    let languages = match &params.lang {
        Some(l) => Some(vec![l
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid language '{}'", l))?]),
        None => None,
    };

    let limit = params.limit.clamp(1, 100);
    let effective_limit = if params.rerank {
        (limit * 4).min(100)
    } else {
        limit
    };

    // Parse include/exclude type filters (CQ-5)
    let include_types = match &params.include_type {
        Some(types) => {
            let parsed: Result<Vec<cqs::parser::ChunkType>, _> =
                types.iter().map(|t| t.parse()).collect();
            Some(parsed.map_err(|e| anyhow::anyhow!("Invalid --include-type: {e}"))?)
        }
        None => Some(cqs::parser::ChunkType::code_types()),
    };
    let exclude_types = match &params.exclude_type {
        Some(types) => {
            let parsed: Result<Vec<cqs::parser::ChunkType>, _> =
                types.iter().map(|t| t.parse()).collect();
            Some(parsed.map_err(|e| anyhow::anyhow!("Invalid --exclude-type: {e}"))?)
        }
        None => None,
    };

    // Classify query for per-category routing (SPLADE alpha + base/enriched index).
    let classification = cqs::search::router::classify_query(&params.query);

    // Per-category SPLADE routing: if --splade flag is set, use it directly.
    // Otherwise, resolve per-category alpha from classification.
    let (use_splade, splade_alpha) = if params.splade {
        (true, params.splade_alpha)
    } else {
        let alpha = cqs::search::router::resolve_splade_alpha(&classification.category);
        if alpha < 1.0 {
            tracing::info!(
                category = %classification.category,
                alpha,
                "SPLADE activated by per-category alpha (batch)"
            );
            (true, alpha)
        } else {
            (false, 1.0)
        }
    };

    // Phase 5: base/enriched index routing. DenseBase queries use the
    // non-enriched HNSW (no LLM summaries). CQS_FORCE_BASE_INDEX=1
    // overrides all queries to base for A/B eval.
    let use_base = matches!(
        classification.strategy,
        cqs::search::router::SearchStrategy::DenseBase
    ) || std::env::var("CQS_FORCE_BASE_INDEX").as_deref() == Ok("1");

    let filter = cqs::SearchFilter {
        languages,
        include_types,
        exclude_types,
        path_pattern: params.path.clone(),
        name_boost: params.name_boost,
        query_text: params.query.clone(),
        enable_rrf: params.rrf,
        enable_demotion: !params.no_demote,
        enable_splade: use_splade,
        splade_alpha,
        type_boost_types: None,
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    // --ref scoped search: search only the named reference
    if let Some(ref ref_name) = params.ref_name {
        let ref_idx = crate::cli::commands::resolve::find_reference(&ctx.root, ref_name)?;
        let ref_limit = if params.rerank {
            (limit * 4).min(100)
        } else {
            limit
        };
        let mut results = cqs::reference::search_reference(
            &ref_idx,
            &query_embedding,
            &filter,
            ref_limit,
            0.3,
            false,
        )?;

        // Re-rank ref results
        if params.rerank && results.len() > 1 {
            let reranker = ctx.reranker()?;
            reranker
                .rerank(&params.query, &mut results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }

        let show_content = !params.no_content;
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
            "query": params.query,
            "total": json_results.len(),
            "source": ref_name,
        }));
    }

    // SPLADE sparse encoding (if enabled by --splade flag OR per-category routing)
    let splade_query = if use_splade {
        ctx.splade_encoder()
            .and_then(|enc| match enc.encode(&params.query) {
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
    let splade_index_ref = ctx.borrow_splade_index();

    // Build SPLADE arg from borrowed references
    let splade_arg = splade_query
        .as_ref()
        .and_then(|sq| splade_index_ref.as_ref().map(|si| (si, sq)));

    // Check audit mode (cached per session)
    let audit_mode = ctx.audit_state();

    // Phase 5: select enriched or base HNSW based on router classification.
    // If base is requested but unavailable, fall back to enriched.
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

    let results = if audit_mode.is_active() || splade_arg.is_some() {
        let code_results = ctx.store().search_hybrid(
            &query_embedding,
            &filter,
            effective_limit,
            0.3,
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
            0.3,
            index,
        )?
    };

    // Re-rank if requested
    let results = if params.rerank && results.len() > 1 {
        let mut code_results: Vec<cqs::store::SearchResult> = results
            .into_iter()
            .map(|r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr,
            })
            .collect();
        let reranker = ctx.reranker()?;
        reranker
            .rerank(&params.query, &mut code_results, limit)
            .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        results
    };

    // --include-refs: merge reference results
    let results = if params.include_refs {
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
                        0.3,
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
            // Convert tagged results back to UnifiedResult for uniform handling
            tagged.into_iter().map(|t| t.result).collect()
        } else {
            results
        }
    } else {
        results
    };

    // Token-budget packing (shared with CLI search)
    let (results, token_info) = if let Some(budget) = params.tokens {
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

    let show_content = !params.no_content;
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
        "query": params.query,
        "total": json_results.len(),
    });
    crate::cli::commands::inject_token_info(&mut response, token_info);
    Ok(response)
}
