//! Info dispatch handlers: stats, context, explain, similar, read, blame, onboard.

use anyhow::Result;

use super::super::BatchView;
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
    ctx: &BatchView,
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
    ctx: &BatchView,
    target: &str,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_explain", target, limit).entered();
    // Task A3: shared cap with `cmd_explain`. Truncates the per-section
    // lists (callers / callees / similar) before serialization.
    let limit = limit.clamp(1, 100);

    let index = ctx.vector_index()?;
    let index = index.as_deref();
    let embedder = if tokens.is_some() {
        Some(ctx.embedder()?)
    } else {
        None
    };

    let mut data = crate::cli::commands::explain::build_explain_data(
        &ctx.store(),
        &ctx.cqs_dir,
        target,
        tokens,
        Some(index),
        embedder,
        &ctx.model_config,
    )?;
    data.callers.truncate(limit);
    data.callees.truncate(limit);
    data.similar.truncate(limit);

    let output = crate::cli::commands::explain::build_explain_output(&data, &ctx.root);
    Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    name: &str,
    limit: usize,
    threshold: f32,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_similar", name).entered();
    let threshold = validate_finite_f32(threshold, "threshold")?;
    // CQ-V1.25-2: shared with CLI's cmd_similar (which currently does not
    // clamp — adding clamp here + constant would regress; keep parity and
    // let CLI gain its clamp in a separate fix).
    let limit = limit.clamp(1, crate::cli::SIMILAR_LIMIT_MAX);

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

    // P2.1: emit canonical 9-field SearchResult shape so daemon/CLI parity holds.
    // Previously the batch path emitted only {name, file, score}, drifting from
    // the CLI's `r.to_json()` schema and breaking agents that expected uniform keys.
    let json_results: Vec<serde_json::Value> = filtered.iter().map(|r| r.to_json()).collect();

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
    ctx: &BatchView,
    path: &str,
    summary: bool,
    compact: bool,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_context", path).entered();

    // PB-V1.29-1: normalize backslash input from Windows / agent pipelines.
    // `get_chunks_by_origin` matches on the stored `origin` column which is
    // forward-slash-normalized; unnormalized `src\foo.rs` silently returns empty.
    // `build_compact_data` / `build_full_data` also normalize internally, but
    // we use the canonical form here for the fall-through full-context path
    // below and the JSON `file:` field so cross-platform consumers see slashes.
    let normalized = normalize_path(std::path::Path::new(path));

    if compact {
        let data = crate::cli::commands::context::build_compact_data(&ctx.store(), &normalized)?;
        return Ok(crate::cli::commands::context::compact_to_json(
            &data,
            &normalized,
        )?);
    }

    if summary {
        let data =
            crate::cli::commands::context::build_full_data(&ctx.store(), &normalized, &ctx.root)?;
        return Ok(crate::cli::commands::context::summary_to_json(
            &data,
            &normalized,
        )?);
    }

    // Full context -- with optional token packing
    let chunks = ctx.store().get_chunks_by_origin(&normalized)?;
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
            // #1167: chunks from `get_chunks_by_origin` come straight from the
            // user's project store, so they're always user-code.
            serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "language": c.language.to_string(),
                "lines": [c.line_start, c.line_end],
                "signature": c.signature,
                "content": c.content,
                "trust_level": "user-code",
            })
        })
        .collect();

    let mut response = serde_json::json!({
        "file": normalized,
        "chunks": entries,
        "total": entries.len(),
    });
    crate::cli::commands::inject_token_info(&mut response, token_info);
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
pub(in crate::cli::batch) fn dispatch_stats(ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_stats").entered();
    let errors = ctx.error_count.load(std::sync::atomic::Ordering::Relaxed);
    let mut output = crate::cli::commands::build_stats(&ctx.store(), &ctx.cqs_dir)?;
    output.errors = Some(errors as usize);

    // BUG-D.10: mirror cmd_stats:283-298 — the daemon previously emitted
    // `stale_files: null` / `missing_files: null` while the CLI populated
    // both, so agents auto-routed through the daemon silently treated
    // every project as fresh. Filesystem walk + `count_stale_files` is
    // cheap; the parser is constructed lazily and torn down.
    match cqs::Parser::new() {
        Ok(parser) => match crate::cli::enumerate_files(&ctx.root, &parser, false) {
            Ok(files) => {
                let file_set: std::collections::HashSet<_> = files.into_iter().collect();
                match ctx.store().count_stale_files(&file_set, &ctx.root) {
                    Ok((stale_count, missing_count)) => {
                        output.stale_files = Some(stale_count as usize);
                        output.missing_files = Some(missing_count as usize);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "dispatch_stats: count_stale_files failed; staleness fields omitted"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "dispatch_stats: enumerate_files failed; staleness fields omitted"
                );
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "dispatch_stats: Parser::new failed; staleness fields omitted"
            );
        }
    }

    Ok(serde_json::to_value(&output)?)
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
    ctx: &BatchView,
    query: &str,
    depth: usize,
    limit: usize,
    tokens: Option<usize>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_onboard", query, depth, limit).entered();
    let embedder = ctx.embedder()?;
    let depth = depth.clamp(1, 5);
    // Task A3: cap on call_chain + callers + tests. entry_point always kept.
    let limit = limit.clamp(1, 100);

    let mut result = cqs::onboard(&ctx.store(), embedder, query, &ctx.root, depth)?;
    result.call_chain.truncate(limit);
    result.callers.truncate(limit);
    result.tests.truncate(limit);

    let Some(budget) = tokens else {
        let mut json = serde_json::to_value(&result)
            .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"))?;
        crate::cli::commands::tag_user_code_trust_level(&mut json);
        return Ok(json);
    };

    let named_items = crate::cli::commands::onboard_scored_names(&result);
    let (content_map, used) =
        crate::cli::commands::fetch_and_pack_content(&ctx.store(), embedder, &named_items, budget);

    let mut json = serde_json::to_value(&result)
        .map_err(|e| anyhow::anyhow!("Failed to serialize onboard: {e}"))?;
    crate::cli::commands::inject_content_into_onboard_json(&mut json, &content_map, &result);
    crate::cli::commands::inject_token_info(&mut json, Some((used, budget)));
    // #1167: onboard only queries the project store — every chunk is user-code.
    crate::cli::commands::tag_user_code_trust_level(&mut json);
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
    ctx: &BatchView,
    path: &str,
    focus: Option<&str>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return dispatch_read_focused(ctx, focus);
    }

    let (file_path, content) = crate::cli::commands::read::validate_and_read_file(&ctx.root, path)?;

    // P2 #69: ctx.audit_state() now returns owned AuditMode (cached + TTL'd
    // reload). build_file_note_header still expects `&AuditMode`, so borrow.
    let audit_state = ctx.audit_state();
    let notes = ctx.notes();
    let (header, notes_injected) =
        crate::cli::commands::read::build_file_note_header(path, &file_path, &audit_state, &notes);

    let enriched = if header.is_empty() {
        content
    } else {
        format!("{}{}", header, content)
    };

    // SEC-V1.33-9: file-read path should also honor vendored detection so
    // `cqs read node_modules/lodash.js` reports the correct trust level
    // matching the chunks-side labeling. Match the user-supplied relative
    // path against the configured `[index].vendored_paths` (or defaults).
    let cfg = ctx.config();
    let prefixes = cqs::vendored::effective_prefixes(
        cfg.index
            .as_ref()
            .and_then(|ic| ic.vendored_paths.as_deref()),
    );
    let normalized = cqs::normalize_path(std::path::Path::new(path));
    let trust_level = if cqs::vendored::is_vendored_origin(&normalized, &prefixes) {
        "vendored-code"
    } else {
        "user-code"
    };

    Ok(serde_json::json!({
        "path": path,
        "content": enriched,
        "notes_injected": notes_injected,
        "trust_level": trust_level,
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
fn dispatch_read_focused(ctx: &BatchView, focus: &str) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_read_focused", focus).entered();

    // P2 #69: ctx.audit_state() now returns owned AuditMode; borrow at the
    // call site since build_focused_output takes `&AuditMode`.
    let audit_state = ctx.audit_state();
    let notes = ctx.notes();
    let result = crate::cli::commands::read::build_focused_output(
        &ctx.store(),
        focus,
        &ctx.root,
        &audit_state,
        &notes,
    )?;

    // SEC-V1.33-9: was hardcoded `"user-code"` even for chunks under
    // `node_modules/`/`vendor/`/etc, defeating the #1221 vendored-code
    // boundary. `build_focused_output` now surfaces the resolved chunk's
    // `vendored` flag (schema v24) so the daemon RPC matches the index-time
    // labeling shape used by search/scout JSON.
    let trust_level = if result.vendored {
        "vendored-code"
    } else {
        "user-code"
    };
    let mut json = serde_json::json!({
        "focus": focus,
        "content": result.output,
        "trust_level": trust_level,
    });
    if let Some(ref h) = result.hints {
        json["hints"] = serde_json::json!({
            "caller_count": h.caller_count,
            "test_count": h.test_count,
            "no_callers": h.caller_count == 0,
            "no_tests": h.test_count == 0,
        });
    }
    // P2.23: surface warnings into the batch response as well so daemon
    // consumers see why type-deps lookup may have come back empty.
    if !result.warnings.is_empty() {
        json["warnings"] = serde_json::json!(result.warnings);
    }

    Ok(json)
}

// P2.79: TC-HAP — happy-path coverage for the embedder-free info dispatchers.
// `dispatch_stats` was the only stats-shape test in the batch tree before this;
// the integration suite (`tests/cli_batch_test.rs`) covers the dispatch line
// parser but not the per-handler SQL → JSON contract. We pin
// `dispatch_stats` here against a freshly-seeded store to catch any
// regression in `build_stats` schema fields without paying the embedder
// load cost.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::batch::create_test_context;
    use cqs::embedder::Embedding;
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::{ModelInfo, Store};
    use std::path::PathBuf;
    use tempfile::TempDir;

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

    fn seed_minimal_ctx() -> (TempDir, crate::cli::batch::BatchContext) {
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
            let chunks = vec![(make_chunk("src/lib.rs:1:foo", "foo"), embedding)];
            store.upsert_chunks_batch(&chunks, Some(0)).expect("upsert");
        }
        let ctx = create_test_context(&cqs_dir).expect("ctx");
        (dir, ctx)
    }

    #[test]
    fn dispatch_stats_returns_expected_envelope_shape() {
        let (_dir, ctx) = seed_minimal_ctx();
        let json = dispatch_stats(&ctx.build_view(None)).expect("dispatch_stats");
        assert!(json.is_object(), "stats must be a JSON object, got: {json}");
        // Pin the canonical CLI/daemon-parity field set the production
        // `cmd_stats` emits — `total_chunks` is the smoke value.
        let total = json
            .get("total_chunks")
            .unwrap_or_else(|| panic!("`total_chunks` missing in stats: {json}"));
        assert!(
            total.as_u64().is_some_and(|n| n >= 1),
            "total_chunks must reflect the seeded chunk: got {total}"
        );
        // `errors` is always present (set from ctx.error_count, default 0).
        assert!(
            json.get("errors").and_then(|v| v.as_u64()).is_some(),
            "stats must carry an `errors` field, got: {json}"
        );
    }

    /// SEC-V1.33-9 regression: a vendored chunk surfaces `trust_level:
    /// "vendored-code"` from `dispatch_read --focus`. Pre-fix the daemon
    /// hardcoded `"user-code"` regardless of the chunk's actual origin,
    /// defeating the #1221 vendored-code boundary.
    fn make_chunk_at(id: &str, name: &str, file: &str) -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from(file),
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

    fn seed_with_vendored_chunk() -> (TempDir, crate::cli::batch::BatchContext) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let emb1 = Embedding::new(emb_vec.clone());
        let emb2 = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
            // Apply default vendored prefixes BEFORE upsert so `node_modules/`
            // is flagged at write time. `set_vendored_prefixes` is on a
            // OnceLock — write order matters: must precede `upsert_chunks_batch`.
            store.set_vendored_prefixes(cqs::vendored::effective_prefixes(None));
            let chunks = vec![
                (
                    make_chunk_at(
                        "node_modules/lib.js:1:lib_fn",
                        "lib_fn",
                        "node_modules/lib.js",
                    ),
                    emb1,
                ),
                (
                    make_chunk_at("src/lib.rs:1:user_fn", "user_fn", "src/lib.rs"),
                    emb2,
                ),
            ];
            store.upsert_chunks_batch(&chunks, Some(0)).expect("upsert");
        }
        let ctx = create_test_context(&cqs_dir).expect("ctx");
        (dir, ctx)
    }

    #[test]
    fn dispatch_read_focus_emits_vendored_code_for_vendored_chunk() {
        let (_dir, ctx) = seed_with_vendored_chunk();
        let view = ctx.build_view(None);
        // `lib_fn` lives in `node_modules/lib.js` so it must be tagged vendored.
        let json = dispatch_read_focused(&view, "lib_fn").expect("dispatch_read_focused");
        assert_eq!(
            json["trust_level"], "vendored-code",
            "vendored chunk must surface trust_level=vendored-code, got: {json}"
        );
    }

    #[test]
    fn dispatch_read_focus_emits_user_code_for_normal_chunk() {
        let (_dir, ctx) = seed_with_vendored_chunk();
        let view = ctx.build_view(None);
        // `user_fn` lives in `src/lib.rs` so it must be tagged user-code.
        let json = dispatch_read_focused(&view, "user_fn").expect("dispatch_read_focused");
        assert_eq!(
            json["trust_level"], "user-code",
            "non-vendored chunk must surface trust_level=user-code, got: {json}"
        );
    }
}
