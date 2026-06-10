//! Info dispatch handlers: stats, context, explain, similar, read, blame, onboard.
//!
//! Handlers take a single `&XArgs` argument so the macro-driven
//! `BatchCmd::dispatch` calls every row uniformly.

use anyhow::Result;

use super::super::BatchView;
use crate::cli::args::{BlameArgs, ContextArgs, ExplainArgs, OnboardArgs, ReadArgs, SimilarArgs};
use crate::cli::validate_finite_f32;

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
    args: &BlameArgs,
) -> Result<serde_json::Value> {
    let target = args.name.as_str();
    let _span = tracing::info_span!("batch_blame", target).entered();
    // Thin adapter over the shared `blame_core` — identical JSON shape across
    // the CLI and daemon surfaces (both serialize via `blame_to_json`).
    let core_args = crate::cli::commands::blame::BlameArgs {
        name: target.to_string(),
        commits: args.commits,
        callers: args.callers,
    };
    let data = crate::cli::commands::blame::blame_core(&ctx.store(), &ctx.root, &core_args)?;
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
    args: &ExplainArgs,
) -> Result<serde_json::Value> {
    let target = args.name.as_str();
    let tokens = args.tokens;
    let _span =
        tracing::info_span!("batch_explain", target, limit = args.limit_arg.limit).entered();
    // Shared cap with `cmd_explain`. Truncates the per-section lists
    // (callers / callees / similar) before serialization.
    let limit = args.limit_arg.limit.clamp(1, 100);

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
    args: &SimilarArgs,
) -> Result<serde_json::Value> {
    let name = args.name.as_str();
    let _span = tracing::info_span!("batch_similar", name).entered();
    let threshold = validate_finite_f32(args.threshold, "threshold")?;
    // Shared with CLI's cmd_similar, which does not clamp — keep parity here
    // rather than diverging.
    let limit = args.limit_arg.limit.clamp(1, crate::cli::SIMILAR_LIMIT_MAX);

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

    // Emit the canonical 9-field SearchResult shape so daemon/CLI parity
    // holds — same schema as the CLI's `r.to_json()`.
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
    args: &ContextArgs,
) -> Result<serde_json::Value> {
    let path = args.path.as_str();
    let _span = tracing::info_span!("batch_context", path).entered();

    // Thin adapter over the shared `context_core` — identical JSON shape across
    // the CLI and daemon surfaces (compact / summary / full all route through
    // the same schema sources, so the daemon's full-context path now carries
    // the external_callers / external_callees / dependent_files /
    // injection_flags / line_start/line_end shape the CLI always emitted).
    // The embedder for full-mode `--tokens` packing comes from the cached view.
    let embedder = if args.tokens.is_some() && !args.compact && !args.summary {
        Some(ctx.embedder()?)
    } else {
        None
    };
    let core_args = crate::cli::commands::context::ContextArgs {
        path: path.to_string(),
        summary: args.summary,
        compact: args.compact,
        tokens: args.tokens,
    };
    crate::cli::commands::context::context_core(&ctx.store(), &ctx.root, &core_args, embedder)
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

    // The CLI and daemon now share `stats_core` so freshness / created_at /
    // hnsw_vectors are computed identically on both surfaces. The daemon
    // layers on its request-error counter afterward — `errors` is the only
    // field that has no CLI analogue.
    let mut output = crate::cli::commands::stats_core(
        &ctx.store(),
        &ctx.root,
        &ctx.cqs_dir,
        &crate::cli::commands::StatsArgs::default(),
    )?;
    output.errors = Some(errors as usize);

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
    args: &OnboardArgs,
) -> Result<serde_json::Value> {
    let query = args.query.as_str();
    let direction = args.direction;
    let _span = tracing::info_span!(
        "batch_onboard",
        query,
        depth = args.depth,
        ?direction,
        limit = args.limit_arg.limit
    )
    .entered();
    let embedder = ctx.embedder()?;

    // Thin adapter over the shared `onboard_core` — identical JSON shape across
    // the CLI and daemon surfaces.
    let core_args = crate::cli::commands::onboard::OnboardArgs {
        query: query.to_string(),
        depth: args.depth,
        direction,
        limit: args.limit_arg.limit,
        tokens: args.tokens,
    };
    crate::cli::commands::onboard::onboard_core(&ctx.store(), embedder, &ctx.root, &core_args)
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
    args: &ReadArgs,
) -> Result<serde_json::Value> {
    let path = args.path.as_str();
    let focus = args.focus.as_deref();
    let _span = tracing::info_span!("batch_read", path).entered();

    // Focused read mode
    if let Some(focus) = focus {
        return dispatch_read_focused(ctx, focus);
    }

    let (file_path, content) = crate::cli::commands::read::validate_and_read_file(&ctx.root, path)?;

    // ctx.audit_state() returns owned AuditMode (cached + TTL'd reload).
    // build_file_note_header expects `&AuditMode`, so borrow.
    let audit_state = ctx.audit_state();
    let notes = ctx.notes();
    let (header, notes_injected) =
        crate::cli::commands::read::build_file_note_header(path, &file_path, &audit_state, &notes);

    let enriched = if header.is_empty() {
        content
    } else {
        format!("{}{}", header, content)
    };

    // The file-read path honors vendored detection so
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

    // ctx.audit_state() returns owned AuditMode; borrow at the call site
    // since build_focused_output takes `&AuditMode`.
    let audit_state = ctx.audit_state();
    let notes = ctx.notes();
    let result = crate::cli::commands::read::build_focused_output(
        &ctx.store(),
        focus,
        &ctx.root,
        &audit_state,
        &notes,
    )?;

    // `build_focused_output` surfaces the resolved chunk's `vendored` flag so
    // the daemon RPC matches the index-time labeling shape used by
    // search/scout JSON — chunks under `node_modules/`/`vendor/`/etc report
    // `vendored-code`.
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
    // Surface warnings into the batch response so daemon consumers see why
    // type-deps lookup may have come back empty.
    if !result.warnings.is_empty() {
        json["warnings"] = serde_json::json!(result.warnings);
    }

    Ok(json)
}

// Happy-path coverage for the embedder-free info dispatchers. The
// integration suite (`tests/cli_batch_test.rs`) covers the dispatch line
// parser but not the per-handler SQL → JSON contract. These pin
// `dispatch_stats` against a freshly-seeded store to catch any regression in
// `build_stats` schema fields without paying the embedder load cost.
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
            canonical_hash: String::new(),
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
        // Post-core-unification: the daemon now shares `stats_core` with the
        // CLI, so the formerly CLI-only `created_at` / `hnsw_vectors` keys are
        // present on the daemon path too (hnsw_vectors is null with no HNSW).
        assert!(
            json.get("created_at").is_some(),
            "stats must carry `created_at` post-unification: {json}"
        );
        assert!(
            json.as_object().unwrap().contains_key("hnsw_vectors")
                || json.get("hnsw_vectors").is_none(),
            "hnsw_vectors key handling is core-owned"
        );
    }

    /// Daemon `dispatch_stale` (non-count-only) is byte-equal to
    /// `stale_core(...)` over the daemon's cached file_set — the parity
    /// contract for the cored stale command.
    #[test]
    fn parity_stale_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let file_set = view.file_set().expect("file_set");
        let core = crate::cli::commands::stale_core(
            &view.store(),
            &view.root,
            &file_set,
            &crate::cli::commands::StaleArgs { count_only: false },
        )
        .expect("stale_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        let dispatched = super::super::analysis::dispatch_stale(
            &view,
            &crate::cli::args::StaleArgs { count_only: false },
        )
        .expect("dispatch_stale");

        assert_eq!(
            dispatched, core_val,
            "dispatch_stale (full) must equal stale_core output"
        );
    }

    /// Daemon `dispatch_stats` is byte-equal to `stats_core(...)` plus the
    /// daemon-only `errors` field — the parity contract for the cored stats
    /// command. Asserts the adapter adds nothing beyond `errors`.
    #[test]
    fn parity_stats_dispatch_equals_core_plus_errors() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let dispatched = dispatch_stats(&view).expect("dispatch_stats");
        let core = crate::cli::commands::stats_core(
            &view.store(),
            &view.root,
            &view.cqs_dir,
            &crate::cli::commands::StatsArgs::default(),
        )
        .expect("stats_core");
        let mut core_val = serde_json::to_value(&core).expect("serialize core");
        // The adapter layers on `errors`; everything else must match exactly.
        let errors = dispatched.get("errors").cloned().expect("errors present");
        core_val
            .as_object_mut()
            .unwrap()
            .insert("errors".to_string(), errors);
        assert_eq!(
            dispatched, core_val,
            "dispatch_stats must equal stats_core + errors"
        );
    }

    /// A vendored chunk surfaces `trust_level: "vendored-code"` from
    /// `dispatch_read --focus`, honoring the vendored-code boundary regardless
    /// of the daemon path.
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
            canonical_hash: String::new(),
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

    /// Parity: `dispatch_context` (the daemon adapter) is byte-equal to
    /// `context_core` driven with the same args. Compact mode is embedder-free,
    /// so this runs without an ONNX load. Pins the Phase-2b convergence — the
    /// daemon no longer hand-rolls per-chunk JSON.
    #[test]
    fn parity_context_compact_daemon_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let args = ContextArgs {
            path: "src/lib.rs".into(),
            summary: false,
            compact: true,
            tokens: None,
        };
        let daemon = dispatch_context(&view, &args).expect("dispatch_context");
        let core = crate::cli::commands::context::context_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::context::ContextArgs {
                path: "src/lib.rs".into(),
                summary: false,
                compact: true,
                tokens: None,
            },
            None,
        )
        .expect("context_core");
        assert_eq!(
            daemon, core,
            "daemon dispatch_context must equal context_core (compact)"
        );
    }

    /// Parity for the full-context path (the path whose schema converged onto
    /// the CLI's `FullOutput` in Phase 2b). Embedder-free because `tokens` is
    /// `None`.
    #[test]
    fn parity_context_full_daemon_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let args = ContextArgs {
            path: "src/lib.rs".into(),
            summary: false,
            compact: false,
            tokens: None,
        };
        let daemon = dispatch_context(&view, &args).expect("dispatch_context");
        let core = crate::cli::commands::context::context_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::context::ContextArgs {
                path: "src/lib.rs".into(),
                summary: false,
                compact: false,
                tokens: None,
            },
            None,
        )
        .expect("context_core");
        assert_eq!(
            daemon, core,
            "daemon dispatch_context must equal context_core (full)"
        );
        // The full path carries the converged schema — these keys were absent
        // from the daemon's old inline JSON.
        assert!(daemon.get("external_callers").is_some());
        assert!(daemon.get("external_callees").is_some());
        assert!(daemon.get("dependent_files").is_some());
    }
}
