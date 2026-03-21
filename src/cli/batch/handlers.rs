//! Batch command handlers — one function per BatchCmd variant.

use std::collections::HashMap;

use anyhow::{Context, Result};

use super::commands::BatchInput;
use super::types::ChunkOutput;
use super::BatchContext;
use cqs::store::DeadConfidence;

use crate::cli::validate_finite_f32;
use cqs::normalize_path;

// ─── Handlers ────────────────────────────────────────────────────────────────

/// Dispatches a blame analysis request for a specified target and returns the results as JSON.
///
/// This function orchestrates the blame operation by building blame data for the given target and converting it to JSON format. It uses tracing instrumentation to log the operation.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the store and root directory path
/// * `target` - The target identifier to analyze for blame information
/// * `depth` - The depth level for traversing blame dependencies
/// * `show_callers` - Whether to include caller information in the blame data
///
/// # Returns
///
/// Returns a `Result` containing a `serde_json::Value` representing the blame analysis in JSON format, or an error if the blame data construction fails.
///
/// # Errors
///
/// Returns an error if building the blame data fails, such as when the target cannot be found or accessed in the store.
pub(super) fn dispatch_blame(
    ctx: &BatchContext,
    target: &str,
    depth: usize,
    show_callers: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_blame", target).entered();
    let data = crate::cli::commands::blame::build_blame_data(
        &ctx.store,
        &ctx.root,
        target,
        depth,
        show_callers,
    )?;
    Ok(crate::cli::commands::blame::blame_to_json(&data, &ctx.root))
}

/// Parameters for batch search dispatch.
pub(super) struct SearchParams {
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
pub(super) fn dispatch_search(
    ctx: &BatchContext,
    params: &SearchParams,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query = %params.query).entered();

    if params.name_only {
        let results = ctx
            .store
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

    let results = if audit_mode.is_active() {
        let code_results = ctx.store.search_filtered_with_index(
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
        ctx.store.search_unified_with_index(
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

/// Dispatches a dependency query for a given name, returning either the types used by it or the code locations that use it.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the store and root path
/// * `name` - The name of the type or function to query dependencies for
/// * `reverse` - If `true`, returns types used by `name`; if `false`, returns code locations that use `name`
///
/// # Returns
///
/// A JSON value containing:
/// - When `reverse` is `true`: an object with the queried function name, a list of types it uses (with type names and edge kinds), and the count of types
/// - When `reverse` is `false`: an array of objects describing code locations that use the type, each with name, file path, line number, and chunk type
///
/// # Errors
///
/// Returns an error if the store query fails.
pub(super) fn dispatch_deps(
    ctx: &BatchContext,
    name: &str,
    reverse: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_deps", name, reverse).entered();

    if reverse {
        let types = ctx.store.get_types_used_by(name)?;
        Ok(serde_json::json!({
            "function": name,
            "types": types.iter().map(|t| {
                serde_json::json!({"type_name": t.type_name, "edge_kind": t.edge_kind})
            }).collect::<Vec<_>>(),
            "count": types.len(),
        }))
    } else {
        let users = ctx.store.get_type_users(name)?;
        let json_users: Vec<serde_json::Value> = users
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": cqs::rel_display(&c.file, &ctx.root),
                    "line_start": c.line_start,
                    "chunk_type": c.chunk_type.to_string(),
                })
            })
            .collect();
        Ok(serde_json::json!(json_users))
    }
}

/// Retrieves and serializes caller information for a given function name.
///
/// This function fetches the complete caller data for the specified function name from the batch context's store, then transforms it into a JSON array containing the caller's name, normalized file path, and line number.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the store to query for caller information
/// * `name` - The name of the function for which to retrieve callers
///
/// # Returns
///
/// A `Result` containing a JSON array of caller objects, each with `name`, `file`, and `line` fields. Returns an error if the store query fails.
pub(super) fn dispatch_callers(ctx: &BatchContext, name: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callers", name).entered();
    let callers = ctx.store.get_callers_full(name)?;
    let json_callers: Vec<serde_json::Value> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": normalize_path(&c.file),
                "line": c.line,
            })
        })
        .collect();
    Ok(serde_json::json!(json_callers))
}

/// Dispatches a request to retrieve all functions called by a specified function.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the store for querying callees
/// * `name` - The name of the function whose callees should be retrieved
///
/// # Returns
///
/// Returns a JSON object containing:
/// - `function`: the name of the queried function
/// - `calls`: an array of objects with `name` and `line` fields for each callee
/// - `count`: the total number of callees found
///
/// # Errors
///
/// Returns an error if the store fails to retrieve the callees for the given function name.
pub(super) fn dispatch_callees(ctx: &BatchContext, name: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callees", name).entered();
    let callees = ctx.store.get_callees_full(name, None)?;
    Ok(serde_json::json!({
        "function": name,
        "calls": callees.iter().map(|(n, line)| {
            serde_json::json!({"name": n, "line": line})
        }).collect::<Vec<_>>(),
        "count": callees.len(),
    }))
}

/// Dispatches an explain request for a target in batch mode, retrieving and formatting explanation data.
///
/// # Arguments
///
/// * `ctx` - The batch execution context providing access to the vector index, embedder, store, and configuration.
/// * `target` - The name or identifier of the target to explain.
/// * `tokens` - Optional token limit for embedder processing. If provided, the embedder will be initialized.
///
/// # Returns
///
/// A JSON value containing the formatted explanation data for the specified target.
///
/// # Errors
///
/// Returns an error if the vector index cannot be retrieved, the embedder fails to initialize (when tokens are specified), or if the explanation data cannot be built or converted to JSON.
pub(super) fn dispatch_explain(
    ctx: &BatchContext,
    target: &str,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_explain", target).entered();

    let index = ctx.vector_index()?;
    let embedder = if tokens.is_some() {
        Some(ctx.embedder()?)
    } else {
        None
    };

    let data = crate::cli::commands::explain::build_explain_data(
        &ctx.store,
        &ctx.cqs_dir,
        target,
        tokens,
        Some(index),
        embedder,
    )?;

    Ok(crate::cli::commands::explain::explain_to_json(
        &data, &ctx.root,
    ))
}

/// Searches for chunks similar to a specified target chunk using vector embeddings.
///
/// Resolves the target chunk by name, retrieves its embedding, and performs a similarity search against the vector index. Returns the top matching chunks ranked by similarity score, excluding the target chunk itself.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the data store and vector index
/// * `target` - The name or identifier of the chunk to find similar chunks for
/// * `limit` - Maximum number of results to return (clamped to 1-100)
/// * `threshold` - Minimum similarity score (0.0-1.0) for results to be included
///
/// # Returns
///
/// A JSON object containing:
/// * `results` - Array of matching chunks with their names, file paths, and similarity scores
/// * `target` - Name of the queried chunk
/// * `total` - Number of results returned
///
/// # Errors
///
/// Returns an error if:
/// * The threshold is not a finite number
/// * The target chunk cannot be resolved
/// * The chunk embedding cannot be loaded
/// * The vector index is unavailable or search fails
pub(super) fn dispatch_similar(
    ctx: &BatchContext,
    target: &str,
    limit: usize,
    threshold: f32,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_similar", target).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;
    let limit = limit.clamp(1, 100);

    let resolved = cqs::resolve_target(&ctx.store, target)?;
    let chunk = &resolved.chunk;

    let (source_chunk, embedding) = ctx
        .store
        .get_chunk_with_embedding(&chunk.id)?
        .ok_or_else(|| anyhow::anyhow!("Could not load embedding for '{}'", chunk.name))?;

    let filter = cqs::SearchFilter::default();

    let index = ctx.vector_index()?;
    let results = ctx.store.search_filtered_with_index(
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
/// Performs a semantic search gather operation with optional cross-index querying and token budget constraints.
///
/// # Arguments
///
/// * `ctx` - The batch execution context containing store, embedder, and vector index
/// * `query` - The search query string to embed and match against
/// * `expand` - Depth of expansion (clamped 0-5) for gathering related chunks
/// * `direction` - The direction to gather results (forward, backward, or bidirectional)
/// * `limit` - Maximum number of results to return (clamped 1-100)
/// * `tokens` - Optional token budget to limit response size
/// * `ref_name` - Optional reference index name for cross-index search
///
/// # Returns
///
/// Returns a JSON value containing the gathered results and optional token usage information.
///
/// # Errors
///
/// Returns an error if embedding fails, the reference index is not loaded, vector index access fails, or the gather operation fails.

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_gather(
    ctx: &BatchContext,
    query: &str,
    expand: usize,
    direction: cqs::GatherDirection,
    limit: usize,
    tokens: Option<usize>,
    ref_name: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_gather", query, ?ref_name).entered();

    let embedder = ctx.embedder()?;
    let query_embedding = embedder
        .embed_query(query)
        .context("Failed to embed query")?;

    let opts = cqs::GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction,
        limit: limit.clamp(1, 100),
        ..cqs::GatherOptions::default()
    };

    let result = if let Some(rn) = ref_name {
        ctx.get_ref(rn)?;
        let ref_idx = ctx
            .borrow_ref(rn)
            .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", rn))?;
        let index = ctx.vector_index()?;
        cqs::gather_cross_index_with_index(
            &ctx.store,
            &ref_idx,
            &query_embedding,
            query,
            &opts,
            &ctx.root,
            index,
        )?
    } else {
        cqs::gather(&ctx.store, &query_embedding, query, &opts, &ctx.root)?
    };

    // Token-budget packing
    let (chunks, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let texts: Vec<&str> = result.chunks.iter().map(|c| c.content.as_str()).collect();
        let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
        let (packed, used) = crate::cli::commands::token_pack(
            result.chunks,
            &counts,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            |c| c.score,
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

/// Analyzes the impact of changes to a target and returns the results as JSON.
///
/// # Arguments
///
/// * `ctx` - The batch execution context containing the code store and root path.
/// * `name` - The name of the target to analyze.
/// * `depth` - The maximum depth for impact analysis, clamped between 1 and 10.
/// * `do_suggest_tests` - Whether to include test suggestions in the output.
/// * `include_types` - Whether to include type information in the impact analysis.
///
/// # Returns
///
/// A JSON value containing the impact analysis results. If `do_suggest_tests` is true, includes a `test_suggestions` array with recommended test names, files, functions, patterns, and inline flags.
///
/// # Errors
///
/// Returns an error if the target cannot be resolved or if the impact analysis fails.
pub(super) fn dispatch_impact(
    ctx: &BatchContext,
    name: &str,
    depth: usize,
    do_suggest_tests: bool,
    include_types: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_impact", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let chunk = &resolved.chunk;
    let depth = depth.clamp(1, 10);

    let result = cqs::analyze_impact(&ctx.store, &chunk.name, depth, include_types)?;

    let mut json = cqs::impact_to_json(&result, &ctx.root);

    if do_suggest_tests {
        let suggestions = cqs::suggest_tests(&ctx.store, &result);
        let suggestions_json: Vec<_> = suggestions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "test_name": s.test_name,
                    "suggested_file": normalize_path(&s.suggested_file),
                    "for_function": s.for_function,
                    "pattern_source": s.pattern_source,
                    "inline": s.inline,
                })
            })
            .collect();
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "test_suggestions".into(),
                serde_json::json!(suggestions_json),
            );
        }
    }

    Ok(json)
}

/// Performs a reverse breadth-first search through the call graph to find all test chunks that call a specified target chunk, up to a maximum depth.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the store and call graph information
/// * `name` - The name of the target chunk to search for callers
/// * `max_depth` - The maximum depth to traverse in the call graph (0 means only direct callers)
///
/// # Returns
///
/// Returns a `Result` containing a `serde_json::Value` representing the test matches found, including their names, file locations, line numbers, depths, and call chains.
///
/// # Errors
///
/// Returns an error if the target chunk cannot be resolved, if the call graph cannot be built, or if test chunks cannot be retrieved from the store.
pub(super) fn dispatch_test_map(
    ctx: &BatchContext,
    name: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_test_map", name).entered();

    let resolved = cqs::resolve_target(&ctx.store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = ctx.call_graph()?;
    let test_chunks = ctx.store.find_test_chunks()?;

    // Reverse BFS from target
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    ancestors.insert(target_name.clone(), (0, String::new()));
    queue.push_back((target_name.clone(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), (depth + 1, current.clone()));
                    queue.push_back((caller.clone(), depth + 1));
                }
            }
        }
    }

    struct TestMatch {
        name: String,
        file: String,
        line: u32,
        depth: usize,
        chain: Vec<String>,
    }

    let mut matches: Vec<TestMatch> = Vec::new();
    for test in &test_chunks {
        if let Some((depth, _)) = ancestors.get(&test.name) {
            if *depth > 0 {
                let mut chain = Vec::new();
                let mut current = test.name.clone();
                let chain_limit = max_depth + 1;
                while !current.is_empty() && chain.len() < chain_limit {
                    chain.push(current.clone());
                    if current == target_name {
                        break;
                    }
                    current = match ancestors.get(&current) {
                        Some((_, p)) if !p.is_empty() => p.clone(),
                        _ => {
                            tracing::debug!(node = %current, "Chain walk hit dead end");
                            break;
                        }
                    };
                }
                let rel_file = cqs::rel_display(&test.file, &ctx.root);
                matches.push(TestMatch {
                    name: test.name.clone(),
                    file: rel_file,
                    line: test.line_start,
                    depth: *depth,
                    chain,
                });
            }
        }
    }

    matches.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));

    let tests_json: Vec<_> = matches
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "file": m.file,
                "line": m.line,
                "call_depth": m.depth,
                "call_chain": m.chain,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "function": target_name,
        "tests": tests_json,
        "test_count": matches.len(),
    }))
}

/// Traces a dependency path between two targets using breadth-first search through the call graph.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the store and call graph
/// * `source` - The source target identifier to start the trace from
/// * `target` - The target identifier to trace to
/// * `max_depth` - The maximum depth to search in the call graph
///
/// # Returns
///
/// A JSON value containing the trace path information, including source and target names, the sequence of intermediate nodes, and the depth of the path found.
///
/// # Errors
///
/// Returns an error if target resolution fails or if the call graph cannot be constructed.
pub(super) fn dispatch_trace(
    ctx: &BatchContext,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_trace", source, target).entered();

    let source_resolved = cqs::resolve_target(&ctx.store, source)?;
    let target_resolved = cqs::resolve_target(&ctx.store, target)?;
    let source_name = source_resolved.chunk.name.clone();
    let target_name = target_resolved.chunk.name.clone();

    if source_name == target_name {
        let rel_file = cqs::rel_display(&source_resolved.chunk.file, &ctx.root);
        return Ok(serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": [{
                "name": source_name,
                "file": rel_file,
                "line": source_resolved.chunk.line_start,
                "signature": source_resolved.chunk.signature,
            }],
            "depth": 0,
        }));
    }

    let graph = ctx.call_graph()?;

    // BFS shortest path
    let mut visited: HashMap<String, String> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    visited.insert(source_name.clone(), String::new());
    queue.push_back((source_name.clone(), 0));
    let mut found_path: Option<Vec<String>> = None;

    while let Some((current, depth)) = queue.pop_front() {
        if current == target_name {
            let mut path = vec![current.clone()];
            let mut node = &current;
            while let Some(pred) = visited.get(node) {
                if pred.is_empty() {
                    break;
                }
                path.push(pred.clone());
                node = pred;
            }
            path.reverse();
            found_path = Some(path);
            break;
        }
        if depth >= max_depth {
            continue;
        }
        if let Some(callees) = graph.forward.get(&current) {
            for callee in callees {
                if !visited.contains_key(callee) {
                    visited.insert(callee.clone(), current.clone());
                    queue.push_back((callee.clone(), depth + 1));
                }
            }
        }
    }

    match found_path {
        Some(names) => {
            // Batch lookup instead of N+1 queries (PERF-20)
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let batch_results = ctx.store.search_by_names_batch(&name_refs, 1)?;

            let mut path_json = Vec::new();
            for name in &names {
                let entry = match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                    Some(r) => {
                        let rel = cqs::rel_display(&r.chunk.file, &ctx.root);
                        serde_json::json!({
                            "name": name,
                            "file": rel,
                            "line": r.chunk.line_start,
                            "signature": r.chunk.signature,
                        })
                    }
                    None => serde_json::json!({"name": name}),
                };
                path_json.push(entry);
            }

            Ok(serde_json::json!({
                "source": source_name,
                "target": target_name,
                "path": path_json,
                "depth": names.len() - 1,
            }))
        }
        None => Ok(serde_json::json!({
            "source": source_name,
            "target": target_name,
            "path": null,
            "message": format!("No call path found within depth {}", max_depth),
        })),
    }
}

/// Identifies and reports dead code in a codebase.
///
/// Analyzes code to find functions that are never called, filtering results based on confidence level and visibility. Returns structured JSON containing categorized dead code findings.
///
/// # Arguments
///
/// * `ctx` - Batch context containing the code store and root directory path
/// * `include_pub` - Whether to include public functions in the dead code analysis
/// * `min_confidence` - Minimum confidence threshold for including results
///
/// # Returns
///
/// A JSON object with four fields:
/// - `dead`: Array of confidently identified dead functions
/// - `possibly_dead_pub`: Array of possibly dead public functions
/// - `total_dead`: Count of confidently dead functions
/// - `total_possibly_dead_pub`: Count of possibly dead public functions
///
/// Each function entry includes name, file path, line range, type, signature, language, and confidence level.
///
/// # Errors
///
/// Returns an error if the code store query fails.
pub(super) fn dispatch_dead(
    ctx: &BatchContext,
    include_pub: bool,
    min_confidence: &DeadConfidence,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_dead").entered();

    let (confident, possibly_pub) = ctx.store.find_dead_code(include_pub)?;

    let confident: Vec<_> = confident
        .into_iter()
        .filter(|d| d.confidence >= *min_confidence)
        .collect();
    let possibly_pub: Vec<_> = possibly_pub
        .into_iter()
        .filter(|d| d.confidence >= *min_confidence)
        .collect();

    let format_dead = |dead: &cqs::store::DeadFunction| {
        let confidence = dead.confidence.as_str();
        serde_json::json!({
            "name": dead.chunk.name,
            "file": cqs::rel_display(&dead.chunk.file, &ctx.root),
            "line_start": dead.chunk.line_start,
            "line_end": dead.chunk.line_end,
            "chunk_type": dead.chunk.chunk_type.to_string(),
            "signature": dead.chunk.signature,
            "language": dead.chunk.language.to_string(),
            "confidence": confidence,
        })
    };

    Ok(serde_json::json!({
        "dead": confident.iter().map(&format_dead).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_dead).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    }))
}

/// Dispatches a request to find functions related to a given function name based on shared callers, callees, and types.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the data store and root path
/// * `name` - The name of the function to find related functions for
/// * `limit` - The maximum number of related results per category (clamped to 1-100)
///
/// # Returns
///
/// A JSON object containing:
/// * `target` - The target function name
/// * `shared_callers` - Array of functions that call the target
/// * `shared_callees` - Array of functions called by the target
/// * `shared_types` - Array of functions sharing type relationships
///
/// Each related function includes its name, file path, line number, and overlap count.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub(super) fn dispatch_related(
    ctx: &BatchContext,
    name: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_related", name).entered();
    let limit = limit.clamp(1, 100);

    let result = cqs::find_related(&ctx.store, name, limit)?;

    let to_json = |items: &[cqs::RelatedFunction]| -> Vec<serde_json::Value> {
        items
            .iter()
            .map(|r| {
                let rel = cqs::rel_display(&r.file, &ctx.root);
                serde_json::json!({
                    "name": r.name,
                    "file": rel,
                    "line": r.line,
                    "overlap_count": r.overlap_count,
                })
            })
            .collect()
    };

    Ok(serde_json::json!({
        "target": result.target,
        "shared_callers": to_json(&result.shared_callers),
        "shared_callees": to_json(&result.shared_callees),
        "shared_types": to_json(&result.shared_types),
    }))
}

/// Dispatches a context query for a given file path in batch mode, returning JSON data.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the indexed data store
/// * `path` - The file path to query context for
/// * `summary` - If true, returns aggregated caller/callee counts; if false, returns full context data
/// * `compact` - If true, returns compacted context data regardless of other flags
/// * `tokens` - Optional token limit for packing the full context response
///
/// # Returns
///
/// Returns a `Result` containing a `serde_json::Value` with the context data. The structure varies based on flags: compact mode returns compacted representation, summary mode returns total caller/callee counts, and full mode returns detailed context information.
///
/// # Errors
///
/// Returns an error if the file at `path` is not indexed or if data retrieval from the store fails.
pub(super) fn dispatch_context(
    ctx: &BatchContext,
    path: &str,
    summary: bool,
    compact: bool,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_context", path).entered();

    if compact {
        let data = crate::cli::commands::context::build_compact_data(&ctx.store, path)?;
        return Ok(crate::cli::commands::context::compact_to_json(&data, path));
    }

    if summary {
        // Batch summary is a simpler aggregation (total counts, no per-caller detail)
        let chunks = ctx.store.get_chunks_by_origin(path)?;
        if chunks.is_empty() {
            anyhow::bail!(
                "No indexed chunks found for '{}'. Is the file indexed?",
                path
            );
        }
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;
        let callee_counts = ctx.store.get_callee_counts_batch(&names)?;
        let total_callers: u64 = caller_counts.values().sum();
        let total_callees: u64 = callee_counts.values().sum();

        return Ok(serde_json::json!({
            "file": path,
            "chunk_count": chunks.len(),
            "total_callers": total_callers,
            "total_callees": total_callees,
        }));
    }

    // Full context — with optional token packing
    let chunks = ctx.store.get_chunks_by_origin(path)?;
    if chunks.is_empty() {
        anyhow::bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    let (chunks, token_info) = if let Some(budget) = tokens {
        let embedder = ctx.embedder()?;
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = ctx.store.get_caller_counts_batch(&names)?;
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
///
/// This function gathers various metrics from the store including chunk counts, file counts, notes, errors, call graph statistics, type graph statistics, and breakdowns by language and type. All statistics are combined into a single JSON object for reporting.
///
/// # Arguments
///
/// `ctx` - The batch processing context containing the store and error counter.
///
/// # Returns
///
/// A JSON value containing aggregated statistics with the following top-level fields: `total_chunks`, `total_files`, `notes`, `errors`, `call_graph` (with `total_calls`, `unique_callers`, `unique_callees`), `type_graph` (with `total_edges`, `unique_types`), `by_language`, `by_type`, `model`, and `schema_version`.
///
/// # Errors
///
/// Returns an error if any of the store queries fail (stats, note_count, function_call_stats, or type_edge_stats).
pub(super) fn dispatch_stats(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stats").entered();
    let stats = ctx.store.stats()?;
    let note_count = ctx.store.note_count()?;
    let fc_stats = ctx.store.function_call_stats()?;
    let te_stats = ctx.store.type_edge_stats()?;
    let errors = ctx.error_count.load(std::sync::atomic::Ordering::Relaxed);

    Ok(serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "notes": note_count,
        "errors": errors,
        "call_graph": {
            "total_calls": fc_stats.total_calls,
            "unique_callers": fc_stats.unique_callers,
            "unique_callees": fc_stats.unique_callees,
        },
        "type_graph": {
            "total_edges": te_stats.total_edges,
            "unique_types": te_stats.unique_types,
        },
        "by_language": stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect::<HashMap<String, _>>(),
        "by_type": stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect::<HashMap<String, _>>(),
        "model": stats.model_name,
        "schema_version": stats.schema_version,
    }))
}

/// Dispatches an onboarding request that identifies relevant code entry points and their relationships, with optional token-based budget limiting.
///
/// # Arguments
///
/// * `ctx` - Batch execution context containing the code store and embedder
/// * `query` - Search query string to find relevant code entry points
/// * `depth` - Traversal depth for call chain exploration (clamped to 1-5)
/// * `tokens` - Optional token budget; if provided, limits serialization to fit within budget
///
/// # Returns
///
/// Returns a JSON value containing the onboarding result with the entry point, call chain hierarchy with depth-based scoring, and related callers. If tokens budget is not specified, returns the complete serialized result. If budget is specified, performs batch fetching of code chunks to optimize token usage.
///
/// # Errors
///
/// Returns an error if embedder initialization fails, onboarding query fails, or serialization fails.
pub(super) fn dispatch_onboard(
    ctx: &BatchContext,
    query: &str,
    depth: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_onboard", query, depth).entered();
    let embedder = ctx.embedder()?;
    let depth = depth.clamp(1, 5);
    let result = cqs::onboard(&ctx.store, embedder, query, &ctx.root, depth)?;

    let Some(budget) = tokens else {
        return cqs::onboard_to_json(&result)
            .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"));
    };

    // Flatten entries with depth-based scores
    let mut entries: Vec<(String, f32)> = Vec::new();
    entries.push((result.entry_point.name.clone(), 1.0));
    for (i, c) in result.call_chain.iter().enumerate() {
        entries.push((c.name.clone(), 1.0 / (i as f32 + 2.0)));
    }
    for c in &result.callers {
        entries.push((c.name.clone(), 0.3));
    }

    // Batch-fetch content
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    let chunks_by_name = match ctx.store.get_chunks_by_names_batch(&names) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for onboard token packing");
            HashMap::new()
        }
    };

    let items: Vec<(String, String, f32)> = entries
        .into_iter()
        .filter_map(|(name, score)| {
            let content = chunks_by_name.get(name.as_str())?.first()?.content.clone();
            Some((name, content, score))
        })
        .collect();

    let texts: Vec<&str> = items.iter().map(|(_, c, _)| c.as_str()).collect();
    let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
    let (packed, used) =
        crate::cli::commands::token_pack(items, &counts, budget, 0, |&(_, _, s)| s);
    let content_map: HashMap<String, String> = packed
        .into_iter()
        .map(|(name, content, _)| (name, content))
        .collect();

    // Build JSON, injecting content for packed entries
    let mut json = cqs::onboard_to_json(&result)
        .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"))?;

    // Inject content into entry_point
    if let Some(content) = content_map.get(&result.entry_point.name) {
        json["entry_point"]["content"] = serde_json::json!(content);
    }
    // Inject into call_chain
    if let Some(chain) = json.get_mut("call_chain").and_then(|v| v.as_array_mut()) {
        for (i, entry) in chain.iter_mut().enumerate() {
            if let Some(c) = result.call_chain.get(i) {
                if let Some(content) = content_map.get(&c.name) {
                    entry["content"] = serde_json::json!(content);
                }
            }
        }
    }
    // Inject into callers
    if let Some(callers) = json.get_mut("callers").and_then(|v| v.as_array_mut()) {
        for (i, entry) in callers.iter_mut().enumerate() {
            if let Some(c) = result.callers.get(i) {
                if let Some(content) = content_map.get(&c.name) {
                    entry["content"] = serde_json::json!(content);
                }
            }
        }
    }

    json["token_count"] = serde_json::json!(used);
    json["token_budget"] = serde_json::json!(budget);
    Ok(json)
}

/// Performs a scout search query with optional token budget packing.
///
/// Executes a scout search on the store using the provided query and returns results as JSON. If a token budget is specified, attempts to batch-fetch chunk content and pack results based on relevance scoring within the token limit.
///
/// # Arguments
///
/// * `ctx` - Batch context containing the embedder and data store
/// * `query` - Search query string
/// * `limit` - Maximum number of results to return (clamped to 1-50)
/// * `tokens` - Optional token budget for content packing; if None, returns results without content
///
/// # Returns
///
/// A JSON value containing scout search results with optional packed content based on token budget.
///
/// # Errors
///
/// Returns an error if embedder initialization fails or if the core scout search operation fails.
pub(super) fn dispatch_scout(
    ctx: &BatchContext,
    query: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_scout", query).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 50);
    let result = cqs::scout(&ctx.store, embedder, query, &ctx.root, limit)?;

    let Some(budget) = tokens else {
        return Ok(cqs::scout_to_json(&result, &ctx.root));
    };

    // Batch-fetch content for all chunks
    let all_names: Vec<&str> = result
        .file_groups
        .iter()
        .flat_map(|g| g.chunks.iter().map(|c| c.name.as_str()))
        .collect();
    let chunks_by_name = match ctx.store.get_chunks_by_names_batch(&all_names) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for scout token packing");
            HashMap::new()
        }
    };

    // Build (name, content, score) triples for packing
    let items: Vec<(String, String, f32)> = result
        .file_groups
        .iter()
        .flat_map(|g| {
            g.chunks.iter().filter_map(|c| {
                let content = chunks_by_name
                    .get(c.name.as_str())?
                    .first()?
                    .content
                    .clone();
                Some((c.name.clone(), content, g.relevance_score * c.search_score))
            })
        })
        .collect();

    let texts: Vec<&str> = items
        .iter()
        .map(|(_, content, _)| content.as_str())
        .collect();
    let counts = crate::cli::commands::count_tokens_batch(embedder, &texts);
    let (packed, used) =
        crate::cli::commands::token_pack(items, &counts, budget, 0, |&(_, _, score)| score);
    let content_map: HashMap<String, String> = packed
        .into_iter()
        .map(|(name, content, _)| (name, content))
        .collect();

    // Build JSON with content for packed items
    let mut json = cqs::scout_to_json(&result, &ctx.root);
    if let Some(groups) = json.get_mut("file_groups").and_then(|v| v.as_array_mut()) {
        for group in groups {
            if let Some(chunks) = group.get_mut("chunks").and_then(|v| v.as_array_mut()) {
                for chunk in chunks {
                    if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
                        if let Some(content) = content_map.get(name) {
                            chunk["content"] = serde_json::json!(content);
                        }
                    }
                }
            }
        }
    }
    json["token_count"] = serde_json::json!(used);
    json["token_budget"] = serde_json::json!(budget);
    Ok(json)
}

/// Suggests optimal file placements for code based on a natural language description.
///
/// Uses an embedder to analyze the provided description and searches the codebase to find the most suitable locations for placing new code. Returns placement suggestions ranked by relevance score, along with contextual information about each candidate location.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the code store and embedder.
/// * `description` - A natural language description of the code to be placed.
/// * `limit` - The maximum number of suggestions to return (clamped to 1-10).
///
/// # Returns
///
/// A JSON value containing the input description and an array of placement suggestions, each with file path, relevance score, insertion line, nearby function name, reasoning, and detected code patterns (imports, error handling, naming conventions, visibility, inline tests).
///
/// # Errors
///
/// Returns an error if the embedder cannot be initialized or if the placement suggestion operation fails.
pub(super) fn dispatch_where(
    ctx: &BatchContext,
    description: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_where", description).entered();
    let embedder = ctx.embedder()?;
    let limit = limit.clamp(1, 10);
    let result = cqs::suggest_placement(&ctx.store, embedder, description, limit)?;

    let suggestions_json: Vec<_> = result
        .suggestions
        .iter()
        .map(|s| {
            let rel = cqs::rel_display(&s.file, &ctx.root);
            serde_json::json!({
                "file": rel,
                "score": s.score,
                "insertion_line": s.insertion_line,
                "near_function": s.near_function,
                "reason": s.reason,
                "patterns": {
                    "imports": s.patterns.imports,
                    "error_handling": s.patterns.error_handling,
                    "naming_convention": s.patterns.naming_convention,
                    "visibility": s.patterns.visibility,
                    "has_inline_tests": s.patterns.has_inline_tests,
                }
            })
        })
        .collect();

    Ok(serde_json::json!({
        "description": description,
        "suggestions": suggestions_json,
    }))
}

/// Dispatches a read operation on a file within a batch context, optionally with focused reading on a specific note.
///
/// # Arguments
///
/// * `ctx` - The batch execution context containing root directory and audit state
/// * `path` - The file path to read, relative to the context root
/// * `focus` - Optional focus identifier to read a specific note instead of the full file
///
/// # Returns
///
/// A JSON object containing:
/// * `path` - The requested file path
/// * `content` - The file content, optionally prepended with an audit note header
/// * `notes_injected` - Boolean indicating whether notes were injected into the header
///
/// # Errors
///
/// Returns an error if file validation or reading fails.
pub(super) fn dispatch_read(
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
    let (header, notes_injected) = crate::cli::commands::read::build_file_note_header(
        path,
        &file_path,
        audit_state,
        ctx.notes(),
    );

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
///
/// Builds output for a specific focused target from the store and formats it as a JSON object containing the focus identifier, content, and optional hints about callers and tests.
///
/// # Arguments
///
/// * `ctx` - The batch execution context containing store, root path, audit state, and notes
/// * `focus` - The identifier of the target to focus on for the read operation
///
/// # Returns
///
/// A JSON value containing:
/// - `focus`: the focus identifier
/// - `content`: the generated output for the focused target
/// - `hints` (optional): an object with caller_count, test_count, no_callers, and no_tests fields
///
/// # Errors
///
/// Returns an error if building the focused output fails.
fn dispatch_read_focused(ctx: &BatchContext, focus: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read_focused", focus).entered();

    let audit_state = ctx.audit_state();
    let result = crate::cli::commands::read::build_focused_output(
        &ctx.store,
        focus,
        &ctx.root,
        audit_state,
        ctx.notes(),
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

/// Dispatches a request to identify stale and missing files in the batch store.
///
/// Retrieves the file set from the batch context and queries the store for files whose modification times have changed or are no longer present on disk. Returns a JSON report containing lists of stale files with their stored and current modification times, missing files, and summary statistics.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the store and file set information.
///
/// # Returns
///
/// A JSON object containing:
/// - `stale`: Array of stale files with their origin path, stored mtime, and current mtime
/// - `missing`: Array of missing file paths
/// - `total_indexed`: Total number of indexed files
/// - `stale_count`: Count of stale files
/// - `missing_count`: Count of missing files
///
/// # Errors
///
/// Returns an error if the file set cannot be retrieved from the context or if the store query fails.
pub(super) fn dispatch_stale(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stale").entered();

    let file_set = ctx.file_set()?;
    let report = ctx.store.list_stale_files(file_set)?;

    let stale_json: Vec<_> = report
        .stale
        .iter()
        .map(|f| {
            serde_json::json!({
                "origin": normalize_path(&f.file),
                "stored_mtime": f.stored_mtime,
                "current_mtime": f.current_mtime,
            })
        })
        .collect();

    let missing_json: Vec<_> = report
        .missing
        .iter()
        .map(|path| serde_json::json!(normalize_path(path)))
        .collect();

    Ok(serde_json::json!({
        "stale": stale_json,
        "missing": missing_json,
        "total_indexed": report.total_indexed,
        "stale_count": report.stale.len(),
        "missing_count": report.missing.len(),
    }))
}

/// Performs a health check on the batch processing system and returns the results as JSON.
///
/// This function executes a comprehensive health check that validates the store, file set, and CQS directory, then serializes the health report to a JSON value for reporting purposes.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing the store, file set, and CQS directory paths.
///
/// # Returns
///
/// A `Result` containing a `serde_json::Value` representing the health check report, or an error if the health check fails or serialization fails.
///
/// # Errors
///
/// Returns an error if retrieving the file set fails, if the health check itself fails, or if serializing the report to JSON fails.
pub(super) fn dispatch_health(ctx: &BatchContext) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_health").entered();

    let file_set = ctx.file_set()?;
    let report = cqs::health::health_check(&ctx.store, file_set, &ctx.cqs_dir)?;

    Ok(serde_json::to_value(&report)?)
}

/// Detects content drift between a reference dataset and the current dataset by comparing similarity scores.
///
/// # Arguments
///
/// * `ctx` - The batch processing context containing reference and current data stores
/// * `reference` - The name of the reference dataset to compare against
/// * `threshold` - The similarity threshold (0.0-1.0) below which content is considered drifted
/// * `min_drift` - The minimum drift value to report
/// * `lang` - Optional language specification for drift detection
/// * `limit` - Optional maximum number of drifted items to return in results
///
/// # Returns
///
/// A JSON object containing:
/// - `reference`: The reference dataset name
/// - `threshold`: The similarity threshold used
/// - `min_drift`: The minimum drift value used
/// - `drifted`: Array of drifted items with name, file, chunk_type, similarity, and drift values
/// - `total_compared`: Total number of items compared
/// - `unchanged`: Number of unchanged items
///
/// # Errors
///
/// Returns an error if:
/// - The threshold or min_drift values are not finite numbers
/// - The reference dataset cannot be loaded or accessed
/// - Drift detection fails during comparison
pub(super) fn dispatch_drift(
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
        &ctx.store,
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

/// Dispatches filtered notes from the batch context as a JSON response.
///
/// Retrieves all notes from the provided batch context and filters them based on the specified criteria. If `warnings` is true, only warning notes are included; if `patterns` is true, only pattern notes are included; otherwise, all notes are included. Each note is serialized to JSON with its text, sentiment score, sentiment label, and mentions.
///
/// # Arguments
///
/// * `ctx` - The batch context containing the notes to dispatch
/// * `warnings` - If true, filter to only warning notes
/// * `patterns` - If true, filter to only pattern notes
///
/// # Returns
///
/// A JSON object containing an array of filtered notes and the total count of notes matching the filter criteria.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub(super) fn dispatch_notes(
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
///
/// This function executes a task based on a natural language description, retrieving relevant code chunks and generating a JSON representation of the results. When a token budget is specified, it applies waterfall budgeting similar to the CLI; otherwise, it returns the standard task JSON representation.
///
/// # Arguments
///
/// * `ctx` - The batch execution context containing store, embedder, and root path
/// * `description` - Natural language description of the task to execute
/// * `limit` - Maximum number of results to return (clamped to 1-10)
/// * `tokens` - Optional token budget for waterfall budgeting of results
///
/// # Returns
///
/// A `Result` containing a JSON value representing the task execution results, with optional token-based budgeting applied.
///
/// # Errors
///
/// Returns an error if the embedder, call graph, test chunks cannot be retrieved from the context, or if task execution fails.
pub(super) fn dispatch_task(
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
        &ctx.store,
        embedder,
        description,
        &ctx.root,
        limit,
        graph,
        test_chunks,
    )?;

    // Full waterfall budgeting (same as CLI) when --tokens is specified
    let json = if let Some(budget) = tokens {
        crate::cli::commands::task::task_to_budgeted_json(&result, &ctx.root, embedder, budget)
    } else {
        cqs::task_to_json(&result, &ctx.root)
    };

    Ok(json)
}

/// Generates help documentation for the BatchInput command and returns it as JSON.
///
/// # Returns
///
/// A Result containing a JSON object with a "help" key mapped to the formatted help text for the BatchInput command.
///
/// # Errors
///
/// Returns an error if writing help text to the buffer fails or if UTF-8 conversion fails.
pub(super) fn dispatch_help() -> Result<serde_json::Value> {
    use clap::CommandFactory;
    let mut buf = Vec::new();
    BatchInput::command().write_help(&mut buf)?;
    let help_text = String::from_utf8_lossy(&buf).to_string();
    Ok(serde_json::json!({"help": help_text}))
}
