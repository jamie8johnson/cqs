//! Info dispatch handlers: stats, context, explain, similar, read, blame, onboard.

use anyhow::Result;

use super::super::BatchContext;
use crate::cli::validate_finite_f32;
use cqs::normalize_path;

/// Dispatches a blame analysis request for a specified target and returns the results as JSON.
/// This function orchestrates the blame operation by building blame data for the given target and converting it to JSON format. It uses tracing instrumentation to log the operation.
/// # Arguments
/// * `ctx` - The batch context containing the store and root directory path
/// * `target` - The target identifier to analyze for blame information
/// * `depth` - The depth level for traversing blame dependencies
/// * `show_callers` - Whether to include caller information in the blame data
/// # Returns
/// Returns a `Result` containing a `serde_json::Value` representing the blame analysis in JSON format, or an error if the blame data construction fails.
/// # Errors
/// Returns an error if building the blame data fails, such as when the target cannot be found or accessed in the store.
pub(in crate::cli::batch) fn dispatch_blame(
    ctx: &BatchContext,
    target: &str,
    depth: usize,
    show_callers: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_blame", target).entered();
    let data = crate::cli::commands::blame::build_blame_data(
        &ctx.store(),
        &ctx.root,
        target,
        depth,
        show_callers,
    )?;
    Ok(crate::cli::commands::blame::blame_to_json(&data, &ctx.root))
}

/// Dispatches an explain request for a target in batch mode, retrieving and formatting explanation data.
/// # Arguments
/// * `ctx` - The batch execution context providing access to the vector index, embedder, store, and configuration.
/// * `target` - The name or identifier of the target to explain.
/// * `tokens` - Optional token limit for embedder processing. If provided, the embedder will be initialized.
/// # Returns
/// A JSON value containing the formatted explanation data for the specified target.
/// # Errors
/// Returns an error if the vector index cannot be retrieved, the embedder fails to initialize (when tokens are specified), or if the explanation data cannot be built or converted to JSON.
pub(in crate::cli::batch) fn dispatch_explain(
    ctx: &BatchContext,
    target: &str,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_explain", target).entered();

    let index = ctx.vector_index()?;
    let index = index.as_deref();
    let embedder = if tokens.is_some() {
        Some(ctx.embedder()?)
    } else {
        None
    };

    let data = crate::cli::commands::explain::build_explain_data(
        &ctx.store(),
        &ctx.cqs_dir,
        target,
        tokens,
        Some(index),
        embedder,
        &ctx.model_config,
    )?;

    Ok(crate::cli::commands::explain::explain_to_json(
        &data, &ctx.root,
    ))
}

/// Searches for chunks similar to a specified target chunk using vector embeddings.
/// Resolves the target chunk by name, retrieves its embedding, and performs a similarity search against the vector index. Returns the top matching chunks ranked by similarity score, excluding the target chunk itself.
/// # Arguments
/// * `ctx` - The batch processing context containing the data store and vector index
/// * `target` - The name or identifier of the chunk to find similar chunks for
/// * `limit` - Maximum number of results to return (clamped to 1-100)
/// * `threshold` - Minimum similarity score (0.0-1.0) for results to be included
/// # Returns
/// A JSON object containing:
/// * `results` - Array of matching chunks with their names, file paths, and similarity scores
/// * `target` - Name of the queried chunk
/// * `total` - Number of results returned
/// # Errors
/// Returns an error if:
/// * The threshold is not a finite number
/// * The named chunk cannot be resolved
/// * The chunk embedding cannot be loaded
/// * The vector index is unavailable or search fails
pub(in crate::cli::batch) fn dispatch_similar(
    ctx: &BatchContext,
    name: &str,
    limit: usize,
    threshold: f32,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_similar", name).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;
    let limit = limit.clamp(1, 100);

    let resolved = cqs::resolve_target(&ctx.store(), name)?;
    let chunk = &resolved.chunk;

    let (source_chunk, embedding) = ctx
        .store()
        .get_chunk_with_embedding(&chunk.id)?
        .ok_or_else(|| anyhow::anyhow!("Could not load embedding for '{}'", chunk.name))?;

    let filter = cqs::SearchFilter::default();

    let index = ctx.vector_index()?;
    let index = index.as_deref();
    let results = ctx.store().search_filtered_with_index(
        &embedding,
        &filter,
        limit.saturating_add(1),
        threshold,
        index,
    )?;

    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| r.chunk.id != source_chunk.id)
        .take(limit)
        .collect();

    let json_results: Vec<serde_json::Value> = filtered
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.chunk.name,
                "file": normalize_path(&r.chunk.file),
                "score": r.score,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "results": json_results,
        "target": chunk.name,
        "total": json_results.len(),
    }))
}

/// Dispatches a context query for a given file path in batch mode, returning JSON data.
/// # Arguments
/// * `ctx` - The batch context containing the indexed data store
/// * `path` - The file path to query context for
/// * `summary` - If true, returns aggregated caller/callee counts; if false, returns full context data
/// * `compact` - If true, returns compacted context data regardless of other flags
/// * `tokens` - Optional token limit for packing the full context response
/// # Returns
/// Returns a `Result` containing a `serde_json::Value` with the context data. The structure varies based on flags: compact mode returns compacted representation, summary mode returns total caller/callee counts, and full mode returns detailed context information.
/// # Errors
/// Returns an error if the file at `path` is not indexed or if data retrieval from the store fails.
pub(in crate::cli::batch) fn dispatch_context(
    ctx: &BatchContext,
    path: &str,
    summary: bool,
    compact: bool,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_context", path).entered();

    if compact {
        let data = crate::cli::commands::context::build_compact_data(&ctx.store(), path)?;
        return Ok(crate::cli::commands::context::compact_to_json(&data, path));
    }

    if summary {
        // Batch summary is a simpler aggregation (total counts, no per-caller detail)
        let chunks = ctx.store().get_chunks_by_origin(path)?;
        if chunks.is_empty() {
            anyhow::bail!(
                "No indexed chunks found for '{}'. Is the file indexed?",
                path
            );
        }
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store().get_caller_counts_batch(&names)?;
        let callee_counts = ctx.store().get_callee_counts_batch(&names)?;
        let total_callers: u64 = caller_counts.values().sum();
        let total_callees: u64 = callee_counts.values().sum();

        return Ok(serde_json::json!({
            "file": path,
            "chunk_count": chunks.len(),
            "total_callers": total_callers,
            "total_callees": total_callees,
        }));
    }

    // Full context -- with optional token packing
    let chunks = ctx.store().get_chunks_by_origin(path)?;
    if chunks.is_empty() {
        anyhow::bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    let (chunks, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store().get_caller_counts_batch(&names)?;
        let (included, used) = crate::cli::commands::context::pack_by_relevance(
            &chunks,
            &caller_counts,
            budget,
            embedder,
        );
        let filtered: Vec<_> = chunks
            .into_iter()
            .filter(|c| included.contains(&c.name))
            .collect();
        (filtered, Some((used, budget)))
    } else {
        (chunks, None)
    };

    let entries: Vec<_> = chunks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "language": c.language.to_string(),
                "lines": [c.line_start, c.line_end],
                "signature": c.signature,
                "content": c.content,
            })
        })
        .collect();

    let mut response = serde_json::json!({
        "file": path,
        "chunks": entries,
        "total": entries.len(),
    });
    if let Some((used, budget)) = token_info {
        response["token_count"] = serde_json::json!(used);
        response["token_budget"] = serde_json::json!(budget);
    }
    Ok(response)
}

/// Collects and aggregates statistics from the batch processing context into a JSON response.
/// This function gathers various metrics from the store including chunk counts, file counts, notes, errors, call graph statistics, type graph statistics, and breakdowns by language and type. All statistics are combined into a single JSON object for reporting.
/// # Arguments
/// `ctx` - The batch processing context containing the store and error counter.
/// # Returns
/// A JSON value containing aggregated statistics with the following top-level fields: `total_chunks`, `total_files`, `notes`, `errors`, `call_graph` (with `total_calls`, `unique_callers`, `unique_callees`), `type_graph` (with `total_edges`, `unique_types`), `by_language`, `by_type`, `model`, and `schema_version`.
/// # Errors
/// Returns an error if any of the store queries fail (stats, note_count, function_call_stats, or type_edge_stats).
pub(in crate::cli::batch) fn dispatch_stats(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stats").entered();
    let errors = ctx.error_count.load(std::sync::atomic::Ordering::Relaxed);

    let mut json = crate::cli::commands::stats_to_json(&ctx.store())?;
    // Batch-specific field
    json["errors"] = serde_json::json!(errors);
    Ok(json)
}

/// Dispatches an onboarding request that identifies relevant code entry points and their relationships, with optional token-based budget limiting.
/// # Arguments
/// * `ctx` - Batch execution context containing the code store and embedder
/// * `query` - Search query string to find relevant code entry points
/// * `depth` - Traversal depth for call chain exploration (clamped to 1-5)
/// * `tokens` - Optional token budget; if provided, limits serialization to fit within budget
/// # Returns
/// Returns a JSON value containing the onboarding result with the entry point, call chain hierarchy with depth-based scoring, and related callers. If tokens budget is not specified, returns the complete serialized result. If budget is specified, performs batch fetching of code chunks to optimize token usage.
/// # Errors
/// Returns an error if embedder initialization fails, onboarding query fails, or serialization fails.
pub(in crate::cli::batch) fn dispatch_onboard(
    ctx: &BatchContext,
    query: &str,
    depth: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_onboard", query, depth).entered();
    let embedder = ctx.embedder()?;
    let depth = depth.clamp(1, 5);
    let result = cqs::onboard(&ctx.store(), embedder, query, &ctx.root, depth)?;

    let Some(budget) = tokens else {
        return cqs::onboard_to_json(&result)
            .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"));
    };

    let named_items = crate::cli::commands::onboard_scored_names(&result);
    let (content_map, used) =
        crate::cli::commands::fetch_and_pack_content(&ctx.store(), embedder, &named_items, budget);

    let mut json = cqs::onboard_to_json(&result)
        .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"))?;
    crate::cli::commands::inject_content_into_onboard_json(&mut json, &content_map, &result);
    json["token_count"] = serde_json::json!(used);
    json["token_budget"] = serde_json::json!(budget);
    Ok(json)
}

/// Dispatches a read operation on a file within a batch context, optionally with focused reading on a specific note.
/// # Arguments
/// * `ctx` - The batch execution context containing root directory and audit state
/// * `path` - The file path to read, relative to the context root
/// * `focus` - Optional focus identifier to read a specific note instead of the full file
/// # Returns
/// A JSON object containing:
/// * `path` - The requested file path
/// * `content` - The file content, optionally prepended with an audit note header
/// * `notes_injected` - Boolean indicating whether notes were injected into the header
/// # Errors
/// Returns an error if file validation or reading fails.
pub(in crate::cli::batch) fn dispatch_read(
    ctx: &BatchContext,
    path: &str,
    focus: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return dispatch_read_focused(ctx, focus);
    }

    let (file_path, content) = crate::cli::commands::read::validate_and_read_file(&ctx.root, path)?;

    let audit_state = ctx.audit_state();
    let notes = ctx.notes();
    let (header, notes_injected) =
        crate::cli::commands::read::build_file_note_header(path, &file_path, audit_state, &notes);

    let enriched = if header.is_empty() {
        content
    } else {
        format!("{}{}", header, content)
    };

    Ok(serde_json::json!({
        "path": path,
        "content": enriched,
        "notes_injected": notes_injected,
    }))
}

/// Dispatches a focused read operation and returns the results as JSON.
/// Builds output for a specific focused target from the store and formats it as a JSON object containing the focus identifier, content, and optional hints about callers and tests.
/// # Arguments
/// * `ctx` - The batch execution context containing store, root path, audit state, and notes
/// * `focus` - The identifier of the target to focus on for the read operation
/// # Returns
/// A JSON value containing:
/// - `focus`: the focus identifier
/// - `content`: the generated output for the focused target
/// - `hints` (optional): an object with caller_count, test_count, no_callers, and no_tests fields
/// # Errors
/// Returns an error if building the focused output fails.
fn dispatch_read_focused(ctx: &BatchContext, focus: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read_focused", focus).entered();

    let audit_state = ctx.audit_state();
    let notes = ctx.notes();
    let result = crate::cli::commands::read::build_focused_output(
        &ctx.store(),
        focus,
        &ctx.root,
        audit_state,
        &notes,
    )?;

    let mut json = serde_json::json!({
        "focus": focus,
        "content": result.output,
    });
    if let Some(ref h) = result.hints {
        json["hints"] = serde_json::json!({
            "caller_count": h.caller_count,
            "test_count": h.test_count,
            "no_callers": h.caller_count == 0,
            "no_tests": h.test_count == 0,
        });
    }

    Ok(json)
}
