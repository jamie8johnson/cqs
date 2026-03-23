//! Search dispatch handler.

use anyhow::{Context, Result};

use super::super::types::ChunkOutput;
use super::super::BatchContext;

/// Parameters for batch search dispatch.
pub(in crate::cli::batch) struct SearchParams {
    pub query: String,
    pub limit: usize,
    pub name_only: bool,
    pub semantic_only: bool,
    pub rerank: bool,
    pub lang: Option<String>,
    pub path: Option<String>,
    pub tokens: Option<usize>,
}

/// Dispatches a search query and returns results as JSON.
///
/// Performs either a name-only search or a full semantic search using embeddings. For name-only searches, queries the store directly by name. For semantic searches, embeds the query and retrieves results, optionally reranking them.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the store and embedder
/// * `params` - Search parameters including query text, limit, language filter, and search mode
///
/// # Returns
///
/// A `Result` containing a JSON object with:
/// * `results` - Array of matching search results
/// * `query` - The original query string
/// * `total` - Number of results returned
///
/// # Errors
///
/// Returns an error if:
/// * The embedder cannot be initialized
/// * Query embedding fails
/// * The language parameter is invalid
/// * Store operations fail
///
/// # Panics
///
/// Panics indirectly if JSON serialization fails unexpectedly (logs warning and returns error object instead for known cases).
pub(in crate::cli::batch) fn dispatch_search(
    ctx: &BatchContext,
    params: &SearchParams,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query = %params.query).entered();

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

    let filter = cqs::SearchFilter {
        languages,
        path_pattern: params.path.clone(),
        name_boost: cqs::store::DEFAULT_NAME_BOOST,
        query_text: params.query.clone(),
        enable_rrf: !params.semantic_only,
        ..Default::default()
    };

    // Check audit mode (cached per session)
    let audit_mode = ctx.audit_state();
    let index = ctx.vector_index()?;
    let index = index.as_deref();

    let results = if audit_mode.is_active() {
        let code_results = ctx.store().search_filtered_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            0.3,
            index,
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
        if code_results.len() > 1 {
            let reranker = ctx.reranker()?;
            reranker
                .rerank(&params.query, &mut code_results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        results
    };

    // Token-budget packing
    let (results, token_info) = if let Some(budget) = params.tokens {
        let embedder = ctx.embedder()?;
        let texts: Vec<&str> = results
            .iter()
            .map(|r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
            })
            .collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
        let (packed, used) = crate::cli::commands::token_pack(
            results,
            &counts,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            |r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.score,
            },
        );
        (packed, Some((used, budget)))
    } else {
        (results, None)
    };

    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| match r {
            cqs::store::UnifiedResult::Code(sr) => {
                serde_json::to_value(ChunkOutput::from_search_result(sr, true))
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
    if let Some((used, budget)) = token_info {
        response["token_count"] = serde_json::json!(used);
        response["token_budget"] = serde_json::json!(budget);
    }
    Ok(response)
}
