//! Call graph dispatch handlers: callers, callees, deps, impact, test-map, trace, related, impact-diff.

use std::collections::HashMap;

use anyhow::Result;

use super::super::BatchContext;
use cqs::normalize_path;

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
pub(in crate::cli::batch) fn dispatch_deps(
    ctx: &BatchContext,
    name: &str,
    reverse: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_deps", name, reverse).entered();

    if reverse {
        let types = ctx.store().get_types_used_by(name)?;
        Ok(serde_json::json!({
            "function": name,
            "types": types.iter().map(|t| {
                serde_json::json!({"type_name": t.type_name, "edge_kind": t.edge_kind})
            }).collect::<Vec<_>>(),
            "count": types.len(),
        }))
    } else {
        let users = ctx.store().get_type_users(name)?;
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
pub(in crate::cli::batch) fn dispatch_callers(
    ctx: &BatchContext,
    name: &str,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callers", name).entered();
    let callers = ctx.store().get_callers_full(name)?;
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
pub(in crate::cli::batch) fn dispatch_callees(
    ctx: &BatchContext,
    name: &str,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_callees", name).entered();
    let callees = ctx.store().get_callees_full(name, None)?;
    Ok(serde_json::json!({
        "function": name,
        "calls": callees.iter().map(|(n, line)| {
            serde_json::json!({"name": n, "line": line})
        }).collect::<Vec<_>>(),
        "count": callees.len(),
    }))
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
pub(in crate::cli::batch) fn dispatch_impact(
    ctx: &BatchContext,
    name: &str,
    depth: usize,
    do_suggest_tests: bool,
    include_types: bool,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_impact", name).entered();

    let resolved = cqs::resolve_target(&ctx.store(), name)?;
    let chunk = &resolved.chunk;
    let depth = depth.clamp(1, 10);

    let result = cqs::analyze_impact(&ctx.store(), &chunk.name, depth, include_types, &ctx.root)?;

    let mut json = cqs::impact_to_json(&result);

    if do_suggest_tests {
        let suggestions = cqs::suggest_tests(&ctx.store(), &result, &ctx.root);
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
pub(in crate::cli::batch) fn dispatch_test_map(
    ctx: &BatchContext,
    name: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_test_map", name).entered();

    let resolved = cqs::resolve_target(&ctx.store(), name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = ctx.call_graph()?;
    let test_chunks = ctx.store().find_test_chunks()?;

    // Reverse BFS from target
    let mut ancestors: HashMap<String, (usize, String)> = HashMap::new();
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    ancestors.insert(target_name.clone(), (0, String::new()));
    queue.push_back((target_name.clone(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(current.as_str()) {
            for caller in callers {
                if !ancestors.contains_key(caller.as_ref()) {
                    ancestors.insert(caller.to_string(), (depth + 1, current.clone()));
                    queue.push_back((caller.to_string(), depth + 1));
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
pub(in crate::cli::batch) fn dispatch_trace(
    ctx: &BatchContext,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_trace", source, target).entered();

    let source_resolved = cqs::resolve_target(&ctx.store(), source)?;
    let target_resolved = cqs::resolve_target(&ctx.store(), target)?;
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
        if let Some(callees) = graph.forward.get(current.as_str()) {
            for callee in callees {
                if !visited.contains_key(callee.as_ref()) {
                    visited.insert(callee.to_string(), current.clone());
                    queue.push_back((callee.to_string(), depth + 1));
                }
            }
        }
    }

    match found_path {
        Some(names) => {
            // Batch lookup instead of N+1 queries (PERF-20)
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let batch_results = ctx.store().search_by_names_batch(&name_refs, 1)?;

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
pub(in crate::cli::batch) fn dispatch_related(
    ctx: &BatchContext,
    name: &str,
    limit: usize,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_related", name).entered();
    let limit = limit.clamp(1, 100);

    let result = cqs::find_related(&ctx.store(), name, limit)?;

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

/// Runs diff-aware impact analysis and returns results as JSON.
pub(in crate::cli::batch) fn dispatch_impact_diff(
    ctx: &BatchContext,
    base: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_impact_diff", ?base).entered();

    let diff_text = crate::cli::commands::run_git_diff(base)?;
    let hunks = cqs::parse_unified_diff(&diff_text);

    if hunks.is_empty() {
        return Ok(serde_json::json!({
            "changed_functions": [],
            "callers": [],
            "tests": [],
            "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
        }));
    }

    let changed = cqs::map_hunks_to_functions(&ctx.store(), &hunks);
    if changed.is_empty() {
        return Ok(serde_json::json!({
            "changed_functions": [],
            "callers": [],
            "tests": [],
            "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
        }));
    }

    let result = cqs::analyze_diff_impact(&ctx.store(), changed, &ctx.root)?;
    Ok(cqs::diff_impact_to_json(&result))
}
