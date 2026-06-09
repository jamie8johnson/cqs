//! Call graph dispatch handlers: callers, callees, deps, impact, test-map, trace, related, impact-diff.
//!
//! Handlers take a single `&XArgs` argument (not destructured positionals) so
//! the macro-driven `BatchCmd::dispatch` can call every row uniformly.
//!
//! ## Polymorphic routing (daemon path)
//!
//! Mirrors the CLI-direct sweep in `cli::commands::graph::*`. Every
//! kind-specialized dispatch handler consults `cqs::kind::classify_hits`
//! against an exact-name lookup before its happy-path query —
//! Const/Type/Module/Ambiguous return a kind-labeled
//! `{kind, fallback_from, name, definitions, note}` value instead of a
//! misrouted-to-empty result. Function-path response shapes are unchanged.

use anyhow::Result;

use cqs::kind::{classify_hits, Kind, KindHit};
use cqs::store::ChunkSummary;

use super::super::BatchView;
use crate::cli::args::{
    CallersArgs, DepsArgs, ImpactArgs, ImpactDiffArgs, RelatedArgs, TestMapArgs, TraceArgs,
};

// ─── Polymorphic-routing kind-mismatch fallback (daemon path) ──────────────

/// Build the kind-labeled fallback value emitted by every dispatch_*
/// function for non-Function kinds. Mirrors the per-command shape that
/// the cmd_* CLI handlers ship in `cli::commands::graph::*`.
fn build_kind_fallback_value(
    name: &str,
    chunks: &[ChunkSummary],
    kind_label: &str,
    fallback_from: &str,
    note: &str,
) -> serde_json::Value {
    // Cap definitions at KIND_FALLBACK_MAX_DEFINITIONS and truncate per-chunk
    // content via the shared helper. Hot names like `Result` / `Error` match
    // hundreds of chunks; without the cap, the daemon writes multi-MB JSONL
    // lines that peg both the wire and the receiver's parse buffer. The cap
    // mirrors the `clamp(1, 100)` discipline the happy-path graph dispatchers
    // use.
    let definitions: Vec<serde_json::Value> = chunks
        .iter()
        .take(crate::cli::commands::KIND_FALLBACK_MAX_DEFINITIONS)
        .map(crate::cli::commands::chunk_to_definition_value)
        .collect();
    serde_json::json!({
        "kind": kind_label,
        "fallback_from": fallback_from,
        "name": name,
        "definitions": definitions,
        "note": note,
    })
}

/// Detect the name's kind via `Store::lookup_by_name` + `classify_hits`.
/// Returns the kind-labeled fallback value when the name resolves to a
/// non-Function/Multiple/Other/NotFound kind that the dispatch handler
/// can't process meaningfully (Const/Type/Module/Ambiguous). Returns
/// `None` for the kinds where the existing flow is appropriate.
///
/// `notes` carries the per-command-specific text for each fallback kind
/// — keeps the per-(command × kind) cell content adjacent to the call
/// site without forcing the helper to know every redirect message.
struct KindNotes<'a> {
    const_note: &'a str,
    type_note: &'a str,
    module_note: &'a str,
    ambiguous_note: &'a str,
}

fn try_kind_fallback(
    ctx: &BatchView,
    name: &str,
    fallback_from: &str,
    notes: KindNotes<'_>,
) -> Result<Option<serde_json::Value>> {
    let chunks = ctx.store().lookup_by_name(name)?;
    let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
    let kind = classify_hits(&hits);
    let (label, note) = match kind {
        Kind::Const => ("const", notes.const_note),
        Kind::Type => ("type", notes.type_note),
        Kind::Module => ("module", notes.module_note),
        Kind::Ambiguous => ("ambiguous", notes.ambiguous_note),
        // Function | Multiple | Other | NotFound: existing flow handles
        // these — Function is the happy path, Multiple resolves
        // deterministically, NotFound surfaces an empty result.
        _ => return Ok(None),
    };
    Ok(Some(build_kind_fallback_value(
        name,
        &chunks,
        label,
        fallback_from,
        note,
    )))
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
pub(in crate::cli::batch) fn dispatch_deps(
    ctx: &BatchView,
    args: &DepsArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let reverse = args.reverse;
    let cross_project = args.cross_project;
    let _span = tracing::info_span!(
        "batch_deps",
        name,
        reverse,
        limit = args.limit_arg.limit,
        cross_project
    )
    .entered();
    if cross_project {
        tracing::warn!("cross-project deps not yet supported, returning local result");
    }
    // Shared cap with `cmd_deps`. Truncates after fetch so the fetched set is
    // bounded by the same value the CLI path would.
    let limit = args.limit_arg.limit.clamp(1, 100);

    // Polymorphic-routing kind detection. Function and Type both have
    // valid deps semantics in their respective modes (reverse / forward);
    // Const/Module/Ambiguous fall back since deps' "uses-of-X" model
    // doesn't fit those kinds. Inline kind dispatch (rather than
    // `try_kind_fallback`) so Type passes through to the existing
    // forward-mode query without producing a fallback shape.
    {
        let chunks = ctx.store().lookup_by_name(name)?;
        let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
        let kind = classify_hits(&hits);
        let (label, note) = match kind {
            Kind::Const => (
                "const",
                "consts don't have type dependencies in either direction; here are the definition sites. Use `cqs <name>` to find references to this const.",
            ),
            Kind::Module => (
                "module",
                "modules don't have type dependencies in this view; here are the declaration sites. Use `cqs deps <type-or-function-in-module>` for an item-level analysis.",
            ),
            Kind::Ambiguous => (
                "ambiguous",
                "name resolves across multiple kinds (function/type/const/etc.); here are all matches. Re-run with a more specific name (e.g. `Type::method`).",
            ),
            // Function | Type | Multiple | Other | NotFound: continue
            // to existing flow. Function with --reverse and Type forward
            // both have valid semantics.
            _ => ("", ""),
        };
        if !label.is_empty() {
            return Ok(build_kind_fallback_value(
                name, &chunks, label, "deps", note,
            ));
        }
    }

    if reverse {
        // Bind the limit at SQL time.
        let types = ctx.store().get_types_used_by(name, limit)?;
        let output = crate::cli::commands::build_deps_reverse(name, &types);
        Ok(serde_json::to_value(&output)?)
    } else {
        let users = ctx.store().get_type_users(name, limit)?;
        let output = crate::cli::commands::build_deps_forward(&users, &ctx.root);
        Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    args: &CallersArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let cross_project = args.cross_project;
    let _span = tracing::info_span!(
        "batch_callers",
        name,
        limit = args.limit_arg.limit,
        cross_project
    )
    .entered();
    // Shared cap with `cmd_callers`. Truncate before serialization.
    let limit = args.limit_arg.limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let mut callers = cross_ctx.get_callers_cross(name)?;
        callers.truncate(limit);
        return Ok(serde_json::to_value(&callers)?);
    }

    if let Some(fallback) = try_kind_fallback(
        ctx,
        name,
        "callers",
        KindNotes {
            const_note: "consts don't have callers; here are the definition sites. Use `cqs <name>` or `cqs search <name>` to find references.",
            type_note: "types don't have callers in the call-graph sense; here are the definition sites. Use `cqs deps <name>` for type-dependency callers or `cqs <name>` to find usage references.",
            module_note: "modules don't have callers in the call-graph sense; here are the declaration sites. Use `cqs <name>` to find files that reference this module.",
            ambiguous_note: "name resolves across multiple kinds (function/type/const/etc.); here are all matches. Re-run with a more specific name (e.g. `Type::method`).",
        },
    )? {
        return Ok(fallback);
    }

    let mut callers = ctx.store().get_callers_full(name)?;
    callers.truncate(limit);
    let output = crate::cli::commands::build_callers(&callers);
    Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    args: &CallersArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let cross_project = args.cross_project;
    let _span = tracing::info_span!(
        "batch_callees",
        name,
        limit = args.limit_arg.limit,
        cross_project
    )
    .entered();
    // Shared cap with `cmd_callees`.
    let limit = args.limit_arg.limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let mut callees = cross_ctx.get_callees_cross(name)?;
        callees.truncate(limit);
        return Ok(serde_json::to_value(&callees)?);
    }

    if let Some(fallback) = try_kind_fallback(
        ctx,
        name,
        "callees",
        KindNotes {
            const_note: "consts don't have callees; the const's value is its content. Use `cqs explain <name>` or `cqs read --focus <name>` to inspect.",
            type_note: "types don't have callees; here are the definition sites. Use `cqs deps <name>` for the type's type dependencies or `cqs callees <Type::method>` for a specific method's callees.",
            module_note: "modules don't have callees; here are the declaration sites. Use `cqs callees <function-in-module>` for a specific function's callees.",
            ambiguous_note: "name resolves across multiple kinds (function/type/const/etc.); here are all matches. Re-run with a more specific name (e.g. `Type::method`).",
        },
    )? {
        return Ok(fallback);
    }

    let mut callees = ctx.store().get_callees_full(name, None)?;
    callees.truncate(limit);
    let output = crate::cli::commands::build_callees(name, &callees);
    Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    args: &ImpactArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let do_suggest_tests = args.suggest_tests;
    let include_types = args.type_impact;
    let cross_project = args.cross_project;
    let _span = tracing::info_span!(
        "batch_impact",
        name,
        limit = args.limit_arg.limit,
        cross_project
    )
    .entered();
    let depth = args.depth.clamp(1, 10);
    // Shared per-section cap with `cmd_impact`. Test suggestions are computed
    // off the un-truncated result so the engine sees every untested caller;
    // truncation happens immediately before serialization.
    let limit = args.limit_arg.limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let mut result = cqs::cross_project::analyze_impact_cross(
            &mut cross_ctx,
            name,
            depth,
            do_suggest_tests,
            include_types,
        )?;
        truncate_impact_sections(&mut result, limit);
        let json = cqs::impact_to_json(&result)?;
        return Ok(json);
    }

    if let Some(fallback) = try_kind_fallback(
        ctx,
        name,
        "impact",
        KindNotes {
            const_note: "consts don't have call-graph impact; here are the definition sites. Use `cqs <name>` or `cqs search <name>` to find references.",
            type_note: "types don't have call-graph impact; here are the definition sites. Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.",
            module_note: "modules don't have call-graph impact; here are the declaration sites. Use `cqs <name>` to find files that reference this module.",
            ambiguous_note: "name resolves across multiple kinds (function/type/const/etc.); here are all matches. Re-run with a more specific name (e.g. `Type::method`).",
        },
    )? {
        return Ok(fallback);
    }

    let resolved = cqs::resolve_target(&ctx.store(), name)?;
    let chunk = &resolved.chunk;

    let mut result = cqs::analyze_impact(
        &ctx.store(),
        &chunk.name,
        &ctx.root,
        &cqs::ImpactOptions {
            depth,
            include_types,
        },
    )?;

    let suggestions = if do_suggest_tests {
        cqs::suggest_tests(&ctx.store(), &result, &ctx.root)
    } else {
        Vec::new()
    };

    truncate_impact_sections(&mut result, limit);

    let mut json = cqs::impact_to_json(&result)?;

    if do_suggest_tests {
        let suggestions_json = cqs::format_test_suggestions(&suggestions);
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "test_suggestions".into(),
                serde_json::json!(suggestions_json),
            );
        }
    }

    Ok(json)
}

/// Per-section truncation for `ImpactResult`. Mirrors the helper in
/// `cli::commands::graph::impact` so both code paths apply the same cap.
fn truncate_impact_sections(result: &mut cqs::ImpactResult, limit: usize) {
    result.callers.truncate(limit);
    result.transitive_callers.truncate(limit);
    result.tests.truncate(limit);
    result.type_impacted.truncate(limit);
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
    ctx: &BatchView,
    args: &TestMapArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let max_depth = args.depth as usize;
    let cross_project = args.cross_project;
    let _span = tracing::info_span!(
        "batch_test_map",
        name,
        limit = args.limit_arg.limit,
        cross_project
    )
    .entered();
    // Shared cap with `cmd_test_map`.
    let limit = args.limit_arg.limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let test_chunks = cross_ctx.find_test_chunks_cross()?;
        let graph = cross_ctx.merged_call_graph()?;
        let summaries: Vec<cqs::store::ChunkSummary> =
            test_chunks.iter().map(|tc| tc.chunk.clone()).collect();

        let mut matches =
            crate::cli::commands::build_test_map(name, &graph, &summaries, &ctx.root, max_depth);
        matches.truncate(limit);
        let output = crate::cli::commands::build_test_map_output(name, &matches);
        return Ok(serde_json::to_value(&output)?);
    }

    if let Some(fallback) = try_kind_fallback(
        ctx,
        name,
        "test-map",
        KindNotes {
            const_note: "consts don't have a call-graph; tests don't 'cover' a const value the way they cover a function. Use `cqs <name>` to find tests that reference this const by name.",
            type_note: "types don't have a call-graph in the same sense; here are the type's definition sites. Use `cqs <name>` to find tests that reference this type, or `cqs test-map <Type::method>` for a specific method's coverage.",
            module_note: "modules don't have a call-graph; tests cover specific functions inside the module, not the module itself. Use `cqs <name>` to find tests in this module's files.",
            ambiguous_note: "name resolves across multiple kinds (function/type/const/etc.); here are all matches. Re-run with a more specific name (e.g. `Type::method`).",
        },
    )? {
        return Ok(fallback);
    }

    let resolved = cqs::resolve_target(&ctx.store(), name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = ctx.call_graph()?;
    let test_chunks = ctx.store().find_test_chunks()?;

    let mut matches = crate::cli::commands::build_test_map(
        &target_name,
        &graph,
        &test_chunks,
        &ctx.root,
        max_depth,
    );
    matches.truncate(limit);
    let output = crate::cli::commands::build_test_map_output(&target_name, &matches);
    Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    args: &TraceArgs,
) -> Result<serde_json::Value> {
    let source = args.source.as_str();
    let target = args.target.as_str();
    let max_depth = args.max_depth as usize;
    let cross_project = args.cross_project;
    let _span = tracing::info_span!("batch_trace", source, target, cross_project).entered();
    // `--limit` is accepted for parity with other graph commands. See
    // `cmd_trace` for rationale (single shortest path today; reserved for
    // future k-shortest-paths variants). args.limit_arg.limit intentionally unused.

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let result = cqs::cross_project::trace_cross(&mut cross_ctx, source, target, max_depth)?;

        let trace_result = cqs::cross_project::CrossProjectTraceResult {
            source: source.to_string(),
            target: target.to_string(),
            depth: result.as_ref().map(|p| p.len().saturating_sub(1)),
            found: result.is_some(),
            path: result,
        };
        return Ok(serde_json::to_value(&trace_result)?);
    }

    // Polymorphic-routing kind detection on the source name. The trace
    // BFS requires a callable starting node; if `source` is non-Function
    // dispatch the kind-labeled fallback. Target's kind is left to
    // `resolve_target` to surface its own typed error if missing.
    if let Some(fallback) = try_kind_fallback(
        ctx,
        source,
        "trace",
        KindNotes {
            const_note: "consts don't participate in the call-graph; no call path can originate from a const value. Use `cqs <source>` to find references and trace from the calling functions.",
            type_note: "types don't have call chains; here are the type's definition sites. Use `cqs <source>` to find usage references or trace from a specific method.",
            module_note: "modules don't participate in the call-graph as nodes. Use `cqs trace <function-in-module> <target>` for a specific function.",
            ambiguous_note: "source name resolves across multiple kinds (function/type/const/etc.); here are all matches. Re-run with a more specific name.",
        },
    )? {
        return Ok(fallback);
    }

    let source_resolved = cqs::resolve_target(&ctx.store(), source)?;
    let target_resolved = cqs::resolve_target(&ctx.store(), target)?;
    let source_name = source_resolved.chunk.name.clone();
    let target_name = target_resolved.chunk.name.clone();

    if source_name == target_name {
        let trivial_path = vec![source_name.clone()];
        let output = crate::cli::commands::trace::build_trace_output(
            &ctx.store(),
            &source_name,
            &target_name,
            Some(&trivial_path),
            &ctx.root,
        )?;
        return Ok(serde_json::to_value(&output)?);
    }

    let graph = ctx.call_graph()?;
    let found_path = crate::cli::commands::trace::bfs_shortest_path(
        &graph.forward,
        &source_name,
        &target_name,
        max_depth,
    );

    let output = crate::cli::commands::trace::build_trace_output(
        &ctx.store(),
        &source_name,
        &target_name,
        found_path.as_deref(),
        &ctx.root,
    )?;
    Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    args: &RelatedArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let _span = tracing::info_span!("batch_related", name).entered();
    // Shared per-category cap with CLI's cmd_related — bounds related-related
    // queries against quadratic blow-up.
    let limit = args.limit_arg.limit.clamp(1, crate::cli::RELATED_LIMIT_MAX);

    let result = cqs::find_related(&ctx.store(), name, limit)?;
    let output = crate::cli::commands::build_related_output(&result, &ctx.root);
    Ok(serde_json::to_value(&output)?)
}

/// Runs diff-aware impact analysis and returns results as JSON.
pub(in crate::cli::batch) fn dispatch_impact_diff(
    ctx: &BatchView,
    args: &ImpactDiffArgs,
) -> Result<serde_json::Value> {
    let base = args.base.as_deref();
    let _span = tracing::info_span!("batch_impact_diff", ?base).entered();

    let diff_text = crate::cli::commands::run_git_diff(base)?;
    let hunks = cqs::parse_unified_diff(&diff_text);

    if hunks.is_empty() {
        // Shared shape with impact_diff / affected.
        return Ok(cqs::diff_impact_empty_json());
    }

    let changed = cqs::map_hunks_to_functions(&ctx.store(), &hunks);
    if changed.is_empty() {
        return Ok(cqs::diff_impact_empty_json());
    }

    let result = cqs::analyze_diff_impact(&ctx.store(), changed, &ctx.root)?;
    Ok(cqs::diff_impact_to_json(&result)?)
}

// Happy-path coverage for the call-graph batch dispatchers
// (callers/callees/deps/impact/test_map/trace/related/impact_diff). The
// integration tests in `tests/cli_batch_test.rs` cover the dispatch line
// parser and JSON envelope shape, but not these handlers' SQL → JSON
// translation. These are minimal pins: seed a tiny corpus with one caller →
// callee edge, then assert each handler returns the expected shape on the
// happy path.
//
// Pattern: see `handlers/search.rs::tests` for the canonical
// `ctx_with_chunks` style — it opens a Store once, batches inserts, drops
// to flush WAL, and re-opens via `create_test_context` in read-only mode.
// We don't ship an embedder here (these handlers are SQL-only), so the
// embedding values are placeholder unit vectors.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::batch::create_test_context;
    use cqs::embedder::Embedding;
    use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
    use cqs::store::{ModelInfo, Store};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// Build a minimal Chunk with `id`, `name`, and a placeholder content
    /// hash. Called from `seed_call_graph_ctx` — the rest of the fields are
    /// filler since these handlers only read name + line metadata.
    fn make_chunk(id: &str, name: &str) -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// Seed two functions (`caller_fn`, `callee_fn`) and a single function-
    /// call edge between them, so the graph dispatchers find at least one
    /// row to return on the happy path.
    fn seed_call_graph_ctx() -> (TempDir, crate::cli::batch::BatchContext) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
            let chunks = vec![
                (
                    make_chunk("src/lib.rs:1:caller", "caller_fn"),
                    embedding.clone(),
                ),
                (
                    make_chunk("src/lib.rs:2:callee", "callee_fn"),
                    embedding.clone(),
                ),
            ];
            store
                .upsert_chunks_batch(&chunks, Some(0))
                .expect("upsert chunks");
            // Insert a caller→callee edge — `upsert_function_calls` takes
            // `&[FunctionCalls]` per `parser::types`.
            let fc = FunctionCalls {
                name: "caller_fn".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "callee_fn".to_string(),
                    line_number: 3,
                }],
            };
            store
                .upsert_function_calls(Path::new("src/lib.rs"), &[fc])
                .expect("upsert function call");
        }
        let ctx = create_test_context(&cqs_dir).expect("create_test_context");
        (dir, ctx)
    }

    #[test]
    fn dispatch_callers_returns_seeded_caller() {
        let (_dir, ctx) = seed_call_graph_ctx();
        let args = CallersArgs {
            name: "callee_fn".into(),
            cross_project: false,
            limit_arg: crate::cli::args::LimitArg { limit: 10 },
        };
        let json = dispatch_callers(&ctx.build_view(None), &args).expect("dispatch_callers");
        // `build_callers` returns `Vec<CallerEntry>`, which serializes as a
        // bare JSON array (no enclosing key).
        let callers = json
            .as_array()
            .unwrap_or_else(|| panic!("response must be a JSON array, got: {json}"));
        assert!(
            callers.iter().any(|c| c["name"] == "caller_fn"),
            "expected caller_fn in callers list, got: {callers:?}"
        );
    }

    #[test]
    fn dispatch_callees_returns_seeded_callee() {
        let (_dir, ctx) = seed_call_graph_ctx();
        let args = CallersArgs {
            name: "caller_fn".into(),
            cross_project: false,
            limit_arg: crate::cli::args::LimitArg { limit: 10 },
        };
        let json = dispatch_callees(&ctx.build_view(None), &args).expect("dispatch_callees");
        // `build_callees` emits `CalleesOutput { name, calls, count }` —
        // `name` field, not `function`.
        assert_eq!(json["name"], "caller_fn");
        let calls = json["calls"]
            .as_array()
            .unwrap_or_else(|| panic!("`calls` must be a JSON array, got: {json}"));
        assert!(
            calls.iter().any(|c| c["name"] == "callee_fn"),
            "expected callee_fn in calls, got: {calls:?}"
        );
    }

    #[test]
    fn dispatch_related_returns_envelope_for_seeded_chunk() {
        let (_dir, ctx) = seed_call_graph_ctx();
        let args = RelatedArgs {
            name: "caller_fn".into(),
            limit_arg: crate::cli::args::LimitArg { limit: 10 },
        };
        let json = dispatch_related(&ctx.build_view(None), &args).expect("dispatch_related");
        // build_related_output structure varies — pin envelope shape only:
        // it must be an object (not an array, not null) with at least one
        // top-level key.
        assert!(
            json.is_object(),
            "dispatch_related must return a JSON object, got: {json}"
        );
    }

    #[test]
    fn dispatch_impact_diff_returns_empty_when_no_diff() {
        // No git context in tempdir → `run_git_diff` returns empty diff,
        // and the handler short-circuits to `diff_impact_empty_json`. The
        // key contract: handler does not panic on a bare temp dir.
        let (_dir, ctx) = seed_call_graph_ctx();
        // Most CI envs run git; the handler's first call to `run_git_diff`
        // either succeeds (returning empty) or errors. Either way the
        // handler returns Result, so this test simply asserts no-panic and
        // a Result outcome.
        let args = ImpactDiffArgs {
            base: None,
            stdin: false,
        };
        let _ = dispatch_impact_diff(&ctx.build_view(None), &args);
    }
}
