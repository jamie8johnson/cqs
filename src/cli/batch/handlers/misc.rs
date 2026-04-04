//! Misc dispatch handlers: notes, gc, plan, task, scout, where, gather, diff, drift, refresh, help.

use anyhow::{Context, Result};

use super::super::commands::BatchInput;
use super::super::BatchContext;
use crate::cli::validate_finite_f32;

/// Parameters for the gather dispatch operation.
pub(in crate::cli::batch) struct GatherParams<'a> {
    pub query: &'a str,
    pub expand: usize,
    pub direction: cqs::GatherDirection,
    pub limit: usize,
    pub tokens: Option<usize>,
    pub ref_name: Option<&'a str>,
}

/// Performs a semantic search gather operation with optional cross-index querying and token budget constraints.
pub(in crate::cli::batch) fn dispatch_gather(
    ctx: &BatchContext,
    params: &GatherParams<'_>,
) -> Result<serde_json::Value> {
    let GatherParams {
        query,
        expand,
        direction,
        limit,
        tokens,
        ref_name,
    } = params;
    let (expand, direction, limit, tokens, ref_name) =
        (*expand, *direction, *limit, *tokens, *ref_name);
    let _span = tracing::info_span!("batch_gather", query, ?ref_name).entered();

    let embedder = ctx.embedder()?;

    let opts = cqs::GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction,
        limit: limit.clamp(1, 100),
        ..cqs::GatherOptions::default()
    };

    let result = if let Some(rn) = ref_name {
        let query_embedding = embedder
            .embed_query(query)
            .context("Failed to embed query")?;
        ctx.get_ref(rn)?;
        let ref_idx = ctx
            .borrow_ref(rn)
            .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", rn))?;
        let index = ctx.vector_index()?;
        let index = index.as_deref();
        cqs::gather_cross_index_with_index(
            &ctx.store(),
            &ref_idx,
            &query_embedding,
            query,
            &opts,
            &ctx.root,
            index,
        )?
    } else {
        cqs::gather(&ctx.store(), embedder, query, &opts, &ctx.root)?
    };

    // Token-budget packing
    let (chunks, token_info): (Vec<cqs::GatheredChunk>, _) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let (packed, used) = crate::cli::commands::pack_gather_chunks(
            result.chunks,
            embedder,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
        );
        (packed, Some((used, budget)))
    } else {
        (result.chunks, None)
    };

    let json_chunks: Vec<serde_json::Value> = chunks
        .iter()
        .filter_map(|c| match serde_json::to_value(c) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, chunk = %c.name, "Failed to serialize chunk");
                None
            }
        })
        .collect();

    let mut response = serde_json::json!({
        "query": query,
        "chunks": json_chunks,
        "expansion_capped": result.expansion_capped,
        "search_degraded": result.search_degraded,
    });
    if let Some((used, budget)) = token_info {
        response["token_count"] = serde_json::json!(used);
        response["token_budget"] = serde_json::json!(budget);
    }
    Ok(response)
}

/// Dispatches filtered notes from the batch context as a JSON response.
/// Retrieves all notes from the provided batch context and filters them based on the specified criteria. If `warnings` is true, only warning notes are included; if `patterns` is true, only pattern notes are included; otherwise, all notes are included. Each note is serialized to JSON with its text, sentiment score, sentiment label, and mentions.
/// # Arguments
/// * `ctx` - The batch context containing the notes to dispatch
/// * `warnings` - If true, filter to only warning notes
/// * `patterns` - If true, filter to only pattern notes
/// # Returns
/// A JSON object containing an array of filtered notes and the total count of notes matching the filter criteria.
/// # Errors
/// Returns an error if JSON serialization fails.
pub(in crate::cli::batch) fn dispatch_notes(
    ctx: &BatchContext,
    warnings: bool,
    patterns: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_notes", warnings, patterns).entered();

    let notes = ctx.notes();
    let filtered: Vec<_> = notes
        .iter()
        .filter(|n| {
            if warnings {
                n.is_warning()
            } else if patterns {
                n.is_pattern()
            } else {
                true
            }
        })
        .map(|n| {
            serde_json::json!({
                "text": n.text,
                "sentiment": n.sentiment,
                "sentiment_label": n.sentiment_label(),
                "mentions": n.mentions,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "notes": filtered,
        "total": filtered.len(),
    }))
}

/// Dispatches a task execution within a batch context, optionally with token budgeting.
/// This function executes a task based on a natural language description, retrieving relevant code chunks and generating a JSON representation of the results. When a token budget is specified, it applies waterfall budgeting similar to the CLI; otherwise, it returns the standard task JSON representation.
/// # Arguments
/// * `ctx` - The batch execution context containing store, embedder, and root path
/// * `description` - Natural language description of the task to execute
/// * `limit` - Maximum number of results to return (clamped to 1-10)
/// * `tokens` - Optional token budget for waterfall budgeting of results
/// # Returns
/// A `Result` containing a JSON value representing the task execution results, with optional token-based budgeting applied.
/// # Errors
/// Returns an error if the embedder, call graph, test chunks cannot be retrieved from the context, or if task execution fails.
pub(in crate::cli::batch) fn dispatch_task(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_task", description).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);
    let graph = ctx.call_graph()?;
    let test_chunks = ctx.test_chunks()?;
    let result = cqs::task_with_resources(
        &ctx.store(),
        embedder,
        description,
        &ctx.root,
        limit,
        &graph,
        &test_chunks,
    )?;

    // Full waterfall budgeting (same as CLI) when --tokens is specified
    let json = if let Some(budget) = tokens {
        crate::cli::commands::task::task_to_budgeted_json(&result, embedder, budget)
    } else {
        cqs::task_to_json(&result)
    };

    Ok(json)
}

/// Performs a scout search query with optional token budget packing.
/// Executes a scout search on the store using the provided query and returns results as JSON. If a token budget is specified, attempts to batch-fetch chunk content and pack results based on relevance scoring within the token limit.
/// # Arguments
/// * `ctx` - Batch context containing the embedder and data store
/// * `query` - Search query string
/// * `limit` - Maximum number of results to return (clamped to 1-50)
/// * `tokens` - Optional token budget for content packing; if None, returns results without content
/// # Returns
/// A JSON value containing scout search results with optional packed content based on token budget.
/// # Errors
/// Returns an error if embedder initialization fails or if the core scout search operation fails.
pub(in crate::cli::batch) fn dispatch_scout(
    ctx: &BatchContext,
    query: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_scout", query).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 50);
    let result = cqs::scout(&ctx.store(), embedder, query, &ctx.root, limit)?;

    let Some(budget) = tokens else {
        return Ok(cqs::scout_to_json(&result));
    };

    let named_items = crate::cli::commands::scout_scored_names(&result);
    let (content_map, used) =
        crate::cli::commands::fetch_and_pack_content(&ctx.store(), embedder, &named_items, budget);

    let mut json = cqs::scout_to_json(&result);
    crate::cli::commands::inject_content_into_scout_json(&mut json, &content_map);
    json["token_count"] = serde_json::json!(used);
    json["token_budget"] = serde_json::json!(budget);
    Ok(json)
}

/// Suggests optimal file placements for code based on a natural language description.
/// Uses an embedder to analyze the provided description and searches the codebase to find the most suitable locations for placing new code. Returns placement suggestions ranked by relevance score, along with contextual information about each candidate location.
/// # Arguments
/// * `ctx` - The batch processing context containing the code store and embedder.
/// * `description` - A natural language description of the code to be placed.
/// * `limit` - The maximum number of suggestions to return (clamped to 1-10).
/// # Returns
/// A JSON value containing the input description and an array of placement suggestions, each with file path, relevance score, insertion line, nearby function name, reasoning, and detected code patterns (imports, error handling, naming conventions, visibility, inline tests).
/// # Errors
/// Returns an error if the embedder cannot be initialized or if the placement suggestion operation fails.
pub(in crate::cli::batch) fn dispatch_where(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_where", description).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);
    let result = cqs::suggest_placement(&ctx.store(), embedder, description, limit)?;

    Ok(crate::cli::commands::where_to_json(
        &result,
        description,
        &ctx.root,
    ))
}

/// Detects content drift between a reference dataset and the current dataset by comparing similarity scores.
/// # Arguments
/// * `ctx` - The batch processing context containing reference and current data stores
/// * `reference` - The name of the reference dataset to compare against
/// * `threshold` - The similarity threshold (0.0-1.0) below which content is considered drifted
/// * `min_drift` - The minimum drift value to report
/// * `lang` - Optional language specification for drift detection
/// * `limit` - Optional maximum number of drifted items to return in results
/// # Returns
/// A JSON object containing:
/// - `reference`: The reference dataset name
/// - `threshold`: The similarity threshold used
/// - `min_drift`: The minimum drift value used
/// - `drifted`: Array of drifted items with name, file, chunk_type, similarity, and drift values
/// - `total_compared`: Total number of items compared
/// - `unchanged`: Number of unchanged items
/// # Errors
/// Returns an error if:
/// - The threshold or min_drift values are not finite numbers
/// - The reference dataset cannot be loaded or accessed
/// - Drift detection fails during comparison
pub(in crate::cli::batch) fn dispatch_drift(
    ctx: &BatchContext,
    reference: &str,
    threshold: f32,
    min_drift: f32,
    lang: Option<&str>,
    limit: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_drift", reference).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;
    let min_drift = validate_finite_f32(min_drift, "min_drift")?;

    // Use cached reference store (PERF-27/RM-17)
    ctx.get_ref(reference)?;
    let ref_idx = ctx
        .borrow_ref(reference)
        .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", reference))?;

    let result = cqs::drift::detect_drift(
        &ref_idx.store,
        &ctx.store(),
        reference,
        threshold,
        min_drift,
        lang,
    )?;

    let mut drifted_json: Vec<_> = result
        .drifted
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "chunk_type": e.chunk_type,
                "similarity": e.similarity,
                "drift": e.drift,
            })
        })
        .collect();
    if let Some(lim) = limit {
        drifted_json.truncate(lim);
    }

    Ok(serde_json::json!({
        "reference": result.reference,
        "threshold": result.threshold,
        "min_drift": result.min_drift,
        "drifted": drifted_json,
        "total_compared": result.total_compared,
        "unchanged": result.unchanged,
    }))
}

/// Runs semantic diff between a reference and the project (or another reference).
pub(in crate::cli::batch) fn dispatch_diff(
    ctx: &BatchContext,
    source: &str,
    target: Option<&str>,
    threshold: f32,
    lang: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_diff", source).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;

    let source_store = crate::cli::commands::resolve::resolve_reference_store(&ctx.root, source)?;

    let target_label = target.unwrap_or("project");
    let target_store = if target_label == "project" {
        // Reuse the batch context's store -- avoid re-opening
        &ctx.store()
    } else {
        // Need to load a separate reference store
        // We can't return a reference to a local, so use get_ref + borrow_ref
        ctx.get_ref(target_label)?;
        // Fall through to resolve below since we can't borrow RefMut as &Store
        // directly. Use resolve_reference_store which opens a fresh Store.
        &ctx.store() // placeholder -- replaced below
    };

    // For non-project targets, resolve properly
    let result = if target_label == "project" {
        cqs::semantic_diff(
            &source_store,
            target_store,
            source,
            target_label,
            threshold,
            lang,
        )?
    } else {
        let target_ref_store =
            crate::cli::commands::resolve::resolve_reference_store(&ctx.root, target_label)?;
        cqs::semantic_diff(
            &source_store,
            &target_ref_store,
            source,
            target_label,
            threshold,
            lang,
        )?
    };

    let added: Vec<_> = result
        .added
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "type": e.chunk_type.to_string(),
            })
        })
        .collect();

    let removed: Vec<_> = result
        .removed
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "type": e.chunk_type.to_string(),
            })
        })
        .collect();

    let modified: Vec<_> = result
        .modified
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file.display().to_string(),
                "type": e.chunk_type.to_string(),
                "similarity": e.similarity,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "source": result.source,
        "target": result.target,
        "added": added,
        "removed": removed,
        "modified": modified,
        "summary": {
            "added": result.added.len(),
            "removed": result.removed.len(),
            "modified": result.modified.len(),
            "unchanged": result.unchanged_count,
        }
    }))
}

/// Runs task planning with template classification and returns results as JSON.
pub(in crate::cli::batch) fn dispatch_plan(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_plan", description).entered();

    let embedder = ctx.embedder()?;
    let result = cqs::plan::plan(&ctx.store(), embedder, description, &ctx.root, limit)
        .context("Plan generation failed")?;

    let mut json = cqs::plan::plan_to_json(&result);
    if let Some(budget) = tokens {
        json["token_budget"] = serde_json::json!(budget);
    }
    Ok(json)
}

/// Runs garbage collection on the index.
/// In batch mode, GC skips HNSW rebuild (the batch session holds the index)
/// and reports what was pruned.
pub(in crate::cli::batch) fn dispatch_gc(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_gc").entered();

    let file_set = ctx.file_set()?;
    let (stale_count, missing_count) = match ctx.store().count_stale_files(&file_set) {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count stale files");
            (0, 0)
        }
    };

    let prune = ctx
        .store()
        .prune_all(&file_set)
        .context("Failed to prune stale entries from index")?;

    Ok(serde_json::json!({
        "stale_files": stale_count,
        "missing_files": missing_count,
        "pruned_chunks": prune.pruned_chunks,
        "pruned_calls": prune.pruned_calls,
        "pruned_type_edges": prune.pruned_type_edges,
        "pruned_summaries": prune.pruned_summaries,
        "hnsw_rebuilt": false,
    }))
}

/// Manually invalidates all mutable caches and re-opens the Store.
pub(in crate::cli::batch) fn dispatch_refresh(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_refresh").entered();
    ctx.invalidate()?;
    Ok(serde_json::json!({"status": "ok", "message": "Caches invalidated, Store re-opened"}))
}

/// Generates help documentation for the BatchInput command and returns it as JSON.
/// # Returns
/// A Result containing a JSON object with a "help" key mapped to the formatted help text for the BatchInput command.
/// # Errors
/// Returns an error if writing help text to the buffer fails or if UTF-8 conversion fails.
pub(in crate::cli::batch) fn dispatch_help() -> Result<serde_json::Value> {
    use clap::CommandFactory;
    let mut buf = Vec::new();
    BatchInput::command().write_help(&mut buf)?;
    let help_text = String::from_utf8_lossy(&buf).to_string();
    Ok(serde_json::json!({"help": help_text}))
}
