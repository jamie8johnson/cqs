//! Search dispatch handler.

use anyhow::{bail, Result};

use super::super::BatchView;
use crate::cli::args::SearchArgs;
use crate::cli::commands::search::query::{
    merge_references, prepare_query, query_core, retrieve_project, retrieve_ref_scoped, Prepared,
    ProjectSurface, QueryArgs,
};
// Shared search `--limit` cap. The CLI dispatcher clamps `cli.limit` to the
// same constant (`cli::dispatch`), so daemon-up and daemon-down invocations
// agree on the bound — CLI==daemon parity is the requirement, not a
// daemon-only defense.
use crate::cli::limits::SEARCH_LIMIT_CAP;

/// Validate the textual filter args (`--lang`, `--include-type`,
/// `--exclude-type`) with flag-specific error messages, returning the parsed
/// `(languages, include_types, exclude_types)` for the ref path to reuse.
///
/// Run at the adapter boundary so a user typo fast-fails before any embedder
/// load — invalid filter flags are user input, not model state, and the daemon
/// contract is to name the offending flag rather than report an embedder error.
type ParsedFilters = (
    Option<Vec<cqs::parser::Language>>,
    Option<Vec<cqs::parser::ChunkType>>,
    Option<Vec<cqs::parser::ChunkType>>,
);

fn validate_filter_args(args: &SearchArgs) -> Result<ParsedFilters> {
    let languages = match &args.lang {
        Some(l) => Some(vec![l
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid language '{}'", l))?]),
        None => None,
    };
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
    Ok((languages, include_types, exclude_types))
}

/// Reset per-thread overlay state and (when requested) validate + stamp the
/// worktree overlay request for this dispatch.
///
/// Called at the TOP of `dispatch_search` (the single search entry — the
/// `--ref` / `--include-refs` path delegates to `dispatch_search_with_refs`
/// from inside it, so this runs exactly once per search), BEFORE the core
/// runs, on the daemon worker thread that will serve the query:
///
/// 1. **`clear_overlay_meta()` unconditionally.** A daemon worker thread is
///    reused across queries; an error path that bails before `write_json_line`
///    must not leak the previous query's `_meta.worktree_overlay` into the
///    next request on this thread. Set-and-take must straddle the same thread —
///    clearing here is the matched bookend to the envelope's `take` after a
///    successful dispatch. (The dispatch threading model: each connection runs
///    its handlers synchronously on one thread; `query_core` calls
///    `ctx.overlay()` on this same thread, which sets the meta, and the
///    envelope `take`s it on this same thread.)
/// 2. **Validate + stamp `--overlay-root`** when overlay was requested (flag
///    OR env) and a `--overlay-root` rode the wire. Validation is the security
///    seam ([`BatchView::set_validated_overlay_request`]); a rejection bubbles
///    up as a wire error rather than silently degrading. With no
///    `--overlay-root`, the request stays `None` (the overlay is a no-op for
///    this query — e.g. a daemon search from the project root itself).
fn prepare_overlay_request(ctx: &BatchView, args: &SearchArgs) -> Result<()> {
    cqs::worktree_overlay::clear_overlay_meta();
    // The default-on flip is decided CLIENT-side: the client applies the
    // tri-state resolution and forwards a `--overlay` flag (+ `--overlay-root`)
    // whenever it activates. The daemon has no client cwd, so it never does
    // default-on itself — `overlay_eligible = false` — but it still honors an
    // explicit `--no-overlay` / `=0` opt-out that rode the wire (opt-out wins).
    if !daemon_overlay_active(args) {
        return Ok(());
    }
    if let Some(root) = &args.overlay_root {
        // Reject (wire error) if the path is not a worktree of this project.
        ctx.set_validated_overlay_request(root)?;
    } else {
        tracing::debug!(
            "overlay requested but no --overlay-root on the wire — serving parent index"
        );
    }
    Ok(())
}

/// Translate the daemon's [`SearchArgs`] into the surface-agnostic
/// [`QueryArgs`] the shared core consumes.
///
/// This is where the daemon's documented semantic differences from the CLI
/// become *settings on the core* rather than separate logic:
/// - `always_route = true`: `cqs search` always classifies (per-category
///   routing is the point), even alongside `--rrf` / `--rerank`.
/// - `fts_first = false`: the daemon never had the NameOnly-FTS-first
///   short-circuit; it stays on the dense hybrid path for non-`--name-only`
///   queries.
/// - limit clamped to [`SEARCH_LIMIT_CAP`].
/// - `json_overhead` is the constant per-result envelope cost — the daemon
///   always serializes, so token-budget packing estimates with the JSON
///   overhead the CLI uses under `--json`.
///
/// `expand_parent` / `pattern` / `context` stay at their `QueryArgs`
/// defaults: the daemon JSON has never emitted parent context, run the
/// pattern filter, or line-context, and Phase 2b keeps that wire shape.
/// (`SearchArgs` carries those flags for CLI parity.) Staleness is not a
/// core concern — the adapter runs the per-origin check after the core
/// returns and attaches `_meta.stale_origins` (see
/// [`attach_stale_origins_meta`]), honoring `--no-stale-check`.
fn daemon_query_args(args: &SearchArgs) -> QueryArgs {
    QueryArgs {
        query: args.query.clone(),
        limit: args.limit_arg.limit.clamp(1, SEARCH_LIMIT_CAP),
        name_only: args.name_only,
        lang: args.lang.clone(),
        include_type: args.include_type.clone(),
        exclude_type: args.exclude_type.clone(),
        path: args.path.clone(),
        // Pattern filter is not part of the daemon wire path (it discarded
        // `args.pattern` before the refactor); leave it None so the core skips
        // the filter, preserving the daemon's retrieval shape.
        pattern: None,
        include_docs: args.include_docs,
        rrf: args.rrf,
        rerank: args.rerank_active(),
        splade: args.splade,
        splade_alpha: args.splade_alpha,
        threshold: args.threshold,
        name_boost: args.name_boost,
        no_demote: args.no_demote,
        tokens: args.tokens,
        // Parent expansion is not emitted on the daemon wire.
        expand_parent: false,
        force_base_index: std::env::var("CQS_FORCE_BASE_INDEX").as_deref() == Ok("1"),
        json_overhead: crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
        // Daemon semantics — see fn doc.
        always_route: true,
        fts_first: false,
        // The daemon surface is always JSON, so provenance is on unless the
        // caller suppresses it for a tight token budget.
        record_rank_signals: !args.no_rank_signals,
        // Overlay activation (daemon-side): opt-out wins, else opt-in (wire flag
        // OR the daemon's own env). Default-on is a client-side decision, so the
        // daemon passes `overlay_eligible = false` — it only ever sees an
        // overlay when the client forwarded `--overlay`. `BatchView::overlay()`
        // consults this to decide whether to resolve+build an overlay.
        overlay: daemon_overlay_active(args),
    }
}

/// Daemon-side overlay activation: the shared tri-state resolution with
/// `overlay_eligible = false` (the daemon never does default-on — that is a
/// client-side decision keyed on the client's cwd). Reduces to: explicit
/// `--no-overlay` / `CQS_WORKTREE_OVERLAY=0` opt-out wins, else explicit
/// `--overlay` / `CQS_WORKTREE_OVERLAY=1` opt-in, else off.
fn daemon_overlay_active(args: &SearchArgs) -> bool {
    crate::cli::commands::search::query::resolve_overlay_active(
        args.overlay,
        args.no_overlay,
        false,
    )
}

/// [`QueryArgs`] for the daemon's `--ref` / `--include-refs` fan-out.
///
/// Same as [`daemon_query_args`] but forces `name_only = false`: the daemon's
/// reference path has always done an embedding search (it predates and never
/// gained an FTS-by-name branch — `--ref --name-only` routes here and was
/// treated as an embedding query on the reference). Forcing it off keeps that
/// behavior and guarantees `prepare_query` returns a dense query (no project
/// FTS short-circuit the ref fan-out couldn't consume).
fn daemon_ref_query_args(args: &SearchArgs) -> QueryArgs {
    QueryArgs {
        name_only: false,
        ..daemon_query_args(args)
    }
}

/// Dispatches a search query and returns results as JSON.
///
/// A thin adapter over the shared search prelude. The plain and `--name-only`
/// paths build [`QueryArgs`] from the wire [`SearchArgs`], run [`query_core`]
/// through the [`BatchView`] `SearchCtx` impl, and serialize via the same
/// `display::build_unified_results_value` the CLI uses — so the daemon and CLI
/// now emit one schema (see CHANGELOG: the daemon dropped the `ChunkOutput`
/// shape). The `--ref` and `--include-refs` paths now share that same
/// [`prepare_query`] prelude (classification, embedding, filter / SPLADE /
/// index resolution); only the reference fan-out is ref-specific
/// ([`retrieve_ref_scoped`] / [`merge_references`]). They serialize through the
/// shared `display::build_tagged_results_value`, so reference results carry the
/// same per-result shape as project results.
///
/// # Errors
/// Returns an error if the embedder cannot be initialized, query embedding
/// fails, a filter argument (`--lang` / `--include-type` / `--exclude-type` /
/// `--pattern`) is invalid, or a store operation fails.
pub(in crate::cli::batch) fn dispatch_search(
    ctx: &BatchView,
    args: &SearchArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query = %args.query).entered();

    // Reset per-thread overlay meta and validate+stamp the overlay request
    // BEFORE any branch can return — including the `--include-refs` delegate
    // below, whose project half overlays too (plan §9). A foreign --overlay-root
    // is rejected here as a wire error.
    prepare_overlay_request(ctx, args)?;

    // Accepted for CLI parity; the batch JSON doesn't surface line-context or
    // parent expansion. Assigning to `_` documents the intentional drop and
    // keeps clippy quiet. (`no_stale_check` IS honored — it gates the
    // `_meta.stale_origins` attachment below.)
    let _ = (args.context, args.expand_parent);

    // `--ref` / `--include-refs` keep their reference-index retrieval in the
    // adapter (the core models only the single-store project path); they
    // serialize through the shared tagged-value builder below. The overlay
    // request is already stamped above, so `--include-refs`'s project leg
    // (which flows through `retrieve_project`) picks it up; the `--ref`-scoped
    // path never calls `retrieve_project`, so it stays parent-truth by
    // construction (plan §9).
    if args.ref_name.is_some() || args.include_refs {
        return dispatch_search_with_refs(ctx, args);
    }

    // Validate the textual filter args at the adapter boundary, BEFORE the
    // core touches the embedder. Invalid `--lang` / `--include-type` /
    // `--exclude-type` are user typos, not model state — the daemon's contract
    // is to fast-fail with the offending flag's name rather than surface
    // "embed query failed" when the embedder is uncached or contended. The core
    // re-parses these (it must stay surface-agnostic), but a pre-pass here keeps
    // the flag-specific error and the no-embedder-load guarantee.
    validate_filter_args(args)?;

    // Plain + name-only: one core, one schema. The `SearchCtx` impl on
    // `BatchView` supplies the store/embedder/splade/index/reranker the core
    // needs; `daemon_query_args` folds the daemon's always-route / no-FTS-first
    // / limit-clamp semantics into the Args.
    let qargs = daemon_query_args(args);
    let output = query_core(ctx, &qargs)?;

    let parents_ref = if output.parents.is_empty() {
        None
    } else {
        Some(&output.parents)
    };
    // No-content: strip the `content` field after the core built results so the
    // shared serializer (which always includes content) honours the daemon's
    // `--no-content`. The CLI handles this in its own display path; the wire
    // path mirrors it here at the adapter boundary.
    let mut value = crate::cli::display::build_unified_results_value(
        &output.results,
        &output.query,
        parents_ref,
        output.token_info,
    );
    if args.no_content {
        strip_content(&mut value);
    }

    let origins = result_origins(&output.results);
    attach_stale_origins_meta(ctx, args, &origins, &mut value);
    Ok(value)
}

/// Deduplicated result origins (chunk file paths) for the staleness check.
fn result_origins(results: &[cqs::store::UnifiedResult]) -> Vec<String> {
    results
        .iter()
        .map(|r| {
            let cqs::store::UnifiedResult::Code(sr) = r;
            sr.chunk.file.to_string_lossy().into_owned()
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

/// Run the cheap per-origin staleness check (mtime-stat over the result
/// origins — O(result_count), never a corpus scan) and attach the sorted
/// stale set as a reserved payload-level `_meta.stale_origins` key, which
/// `write_json_line` lifts onto the envelope `_meta` (sibling of `data`,
/// same wire position as `worktree_stale`). Skip-when-empty: a fresh index
/// emits no `_meta` key at all. `--no-stale-check` skips the check entirely.
///
/// PARITY: this is the daemon-surface counterpart of the CLI render path's
/// `warn_stale_results` call (`render_query_output` in
/// `commands::search::query`). The CLI client reads this meta off the daemon
/// envelope and prints the same stderr warning via
/// `staleness::print_stale_warning`, so daemon-up and daemon-down warn
/// identically for the same stale state. Change one side only with a reason.
///
/// Errors are logged and swallowed — the staleness check must never break a
/// query (same contract as `warn_stale_results`).
fn attach_stale_origins_meta(
    ctx: &BatchView,
    args: &SearchArgs,
    origins: &[String],
    value: &mut serde_json::Value,
) {
    if args.no_stale_check || origins.is_empty() {
        return;
    }
    let origin_refs: Vec<&str> = origins.iter().map(String::as_str).collect();
    let stale = match ctx.store().check_origins_stale(&origin_refs, &ctx.root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check staleness");
            return;
        }
    };
    if stale.is_empty() {
        return;
    }
    // Sorted for a deterministic wire shape (HashSet order is arbitrary).
    let mut stale: Vec<String> = stale.into_iter().collect();
    stale.sort();
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "_meta".to_string(),
            serde_json::json!({ "stale_origins": stale }),
        );
    }
}

/// Reference-scoped (`--ref`) and reference-merged (`--include-refs`) search.
///
/// Routes through the same `prepare_query` prelude the plain path uses, so the
/// daemon ref path no longer reimplements classification, centroid α-floor,
/// filter construction, SPLADE resolution, or base-index selection. Only the
/// retrieval fan-out is ref-specific: `retrieve_ref_scoped` for `--ref`,
/// `retrieve_project` + `merge_references` for `--include-refs`. Serialization
/// converges on the shared `SearchResultOutput` schema via
/// `build_tagged_results_value`.
fn dispatch_search_with_refs(ctx: &BatchView, args: &SearchArgs) -> Result<serde_json::Value> {
    // Textual filter validation runs BEFORE embedder load (shared with the
    // plain path) — invalid `--lang` / `--include-type` / `--exclude-type` are
    // user typos that must fast-fail with the offending flag's name. The core
    // re-parses these inside `prepare_query`; this pre-pass keeps the
    // flag-specific error and the no-embedder-load guarantee.
    validate_filter_args(args)?;

    // The daemon's always-route / no-FTS-first / name-only-off / limit-clamp
    // semantics fold into the same `QueryArgs` the plain path builds; the
    // multi-store fan-out then consumes the prepared query.
    let qargs = daemon_ref_query_args(args);
    // `--ref`-scoped searches one reference store and never reads the project
    // index, so skip the project-surface resolution (project vector index +
    // SPLADE encode + primed SPLADE index). `--include-refs` fans out over the
    // project store (`retrieve_project`), so it resolves the full surface.
    let surface = if args.ref_name.is_some() {
        ProjectSurface::Skip
    } else {
        ProjectSurface::Resolve
    };
    let prepared = match prepare_query(ctx, &qargs, surface)? {
        // `name_only = false` + `fts_first = false` on the daemon ref path → the
        // project FTS short-circuit never fires, so it always prepares a dense
        // query. A request handler must never panic the daemon, so a future
        // default change surfaces as a wire error instead.
        Prepared::ShortCircuit(_) => {
            bail!(
                "BUG: daemon ref path got an FTS short-circuit despite \
                 name_only = false and fts_first = false — report this"
            )
        }
        Prepared::Dense(p) => p,
    };

    // --ref scoped search: search only the named reference.
    if let Some(ref ref_name) = args.ref_name {
        let tagged = retrieve_ref_scoped(ctx, &qargs, &prepared, ref_name)?;
        let (tagged, token_info) = pack_tagged(ctx, args, tagged)?;
        let mut value = crate::cli::display::build_tagged_results_value(
            &tagged,
            &args.query,
            None,
            token_info,
            Some(ref_name.clone()),
        );
        if args.no_content {
            strip_content(&mut value);
        }
        return Ok(value);
    }

    // --include-refs: project search + merged reference results. The project
    // half (`retrieve_project`) is byte-identical to the plain path.
    let project_results = retrieve_project(ctx, &qargs, &prepared)?;

    // Project-result origins for the staleness meta, captured before the
    // reference merge consumes the project results. Reference origins are not
    // project files — parity with the CLI multi-index path, which checks
    // project results only.
    let project_origins = result_origins(&project_results);

    let references = ctx.get_all_refs()?;
    let tagged = merge_references(&qargs, &prepared, project_results, &references);

    let (tagged, token_info) = pack_tagged(ctx, args, tagged)?;
    let mut value = crate::cli::display::build_tagged_results_value(
        &tagged,
        &args.query,
        None,
        token_info,
        None,
    );
    if args.no_content {
        strip_content(&mut value);
    }
    attach_stale_origins_meta(ctx, args, &project_origins, &mut value);
    Ok(value)
}

/// Token-budget packing for the tagged (`--ref` / `--include-refs`) path.
/// Returns the packed results and the `(used, budget)` token info, or the input
/// unchanged when `--tokens` isn't set.
type TaggedPack = (Vec<cqs::reference::TaggedResult>, Option<(usize, usize)>);

fn pack_tagged(
    ctx: &BatchView,
    args: &SearchArgs,
    tagged: Vec<cqs::reference::TaggedResult>,
) -> Result<TaggedPack> {
    if let Some(budget) = args.tokens {
        let embedder = ctx.embedder()?;
        Ok(crate::cli::commands::token_pack_results(
            tagged,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            embedder,
            |t| match &t.result {
                cqs::store::UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
            },
            |t| match &t.result {
                cqs::store::UnifiedResult::Code(sr) => sr.score,
            },
            "batch_search_tagged",
        ))
    } else {
        Ok((tagged, None))
    }
}

/// Remove the `content` field from every result object, honoring the daemon's
/// `--no-content`. The shared `display` serializer always emits content (it's
/// the CLI default); the daemon's `--no-content` is applied here at the adapter
/// boundary so both surfaces keep one schema builder.
fn strip_content(value: &mut serde_json::Value) {
    if let Some(results) = value.get_mut("results").and_then(|r| r.as_array_mut()) {
        for r in results {
            if let Some(obj) = r.as_object_mut() {
                obj.remove("content");
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

// Content-asserting tests for `dispatch_search`.
//
// The batch `tests/cli_batch_test.rs` integration tests only assert schema
// (field names, non-empty arrays), so a regression that returned zeros or the
// wrong chunk would slip through. These tests exercise `dispatch_search`
// directly against a fixture store and assert the returned chunk content.
//
// The tests live inside the crate (not `tests/`) because `dispatch_search` is
// `pub(in crate::cli::batch)`, not reachable from an external integration
// test. Staying in-process is critical: `tests/cli_batch_test.rs` is gated
// behind `slow-tests` because it shells out to `cqs` and cold-loads the ONNX
// stack (~2 hours in CI). These tests use the in-process fixture pattern
// (build `Store` + `BatchContext`, call the handler directly) and each runs
// in well under a second because they exercise the `--name-only` branch that
// skips embedder init entirely.
//
// Coverage:
// - `--name-only` branch (dispatch_search:41-60): exact, prefix, substring,
//   no-match, and per-language content assertions.
// - `--include-type` parsing (dispatch_search:82-97): invalid type name path
//   returns an error (exercised before the embedder is touched, so still fast).
// - `--exclude-type` parsing (dispatch_search:90-97): same.
//
// The semantic search branch (post-line-62) requires a real ONNX embedder and
// is covered by the eval suite (`tests/eval_test.rs`, `#[ignore]`), not here.
#[cfg(test)]
mod tests {
    use super::*;
    // Inside `mod tests` in handlers/search.rs the super chain is:
    //   super              -> search module (this file)
    //   super::super       -> handlers module (handlers/mod.rs)
    //   super::super::super -> batch module (batch/mod.rs)
    // `commands` is a private sibling of `handlers`, unreachable via
    // `crate::cli::batch::commands`, but the super chain does reach it.
    use super::super::super::commands::{BatchCmd, BatchInput};
    use super::super::super::{create_test_context, BatchContext};
    use clap::Parser;
    use cqs::embedder::Embedding;
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::{ModelInfo, Store};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build a test Chunk with the usual test defaults.
    fn make_chunk(
        id: &str,
        file: &str,
        language: Language,
        chunk_type: ChunkType,
        name: &str,
        signature: &str,
        content: &str,
    ) -> Chunk {
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from(file),
            language,
            chunk_type,
            name: name.to_string(),
            signature: signature.to_string(),
            content: content.to_string(),
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

    /// Build a test BatchContext pre-populated with the given chunks.
    ///
    /// Opens the Store ONCE per test, inits, batch-inserts all chunks in a
    /// single `upsert_chunks_batch` call, then drops — so the transaction
    /// overhead amortizes across all chunks.
    ///
    /// Runtime note: on WSL `/mnt/c` (NTFS over 9P) a single
    /// `upsert_chunks_batch` with a non-empty batch takes ~20s. This is a
    /// pre-existing environmental slowness that affects every Store-write
    /// test in the crate — e.g. `store::chunks::crud::tests::
    /// test_upsert_chunks_batch_insert_and_fetch` has the same profile. On
    /// Linux ext4 (CI) the same test completes in <100ms. Don't refactor
    /// this helper to "fix" the runtime — the fix belongs in the SQLite/
    /// sqlx/WSL layer, not in each test.
    fn ctx_with_chunks(chunks: Vec<Chunk>) -> (TempDir, BatchContext) {
        let dir = TempDir::new().expect("Failed to create temp dir");
        ctx_with_chunks_in(dir, chunks, Some(0))
    }

    /// Like [`ctx_with_chunks`] but with a caller-supplied project dir (so
    /// tests can put real files on disk first) and a caller-supplied
    /// `source_mtime` (so staleness tests can pin the stored fingerprint
    /// against the disk one).
    fn ctx_with_chunks_in(
        dir: TempDir,
        chunks: Vec<Chunk>,
        source_mtime: Option<i64>,
    ) -> (TempDir, BatchContext) {
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("Failed to create .cqs dir");
        let index_path = cqs_dir.join("index.db");

        // Unit embedding: `upsert_chunk` validates dimension against
        // `ModelInfo::default()`. Content of the embedding vector doesn't
        // matter for the name-only branch under test — only the chunks
        // table + FTS index are consulted.
        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("Failed to open test store");
            store
                .init(&ModelInfo::default())
                .expect("Failed to init test store");
            // Batch all inserts in one call so we pay the transaction setup
            // cost once, not per chunk.
            if !chunks.is_empty() {
                let pairs: Vec<(Chunk, Embedding)> = chunks
                    .iter()
                    .map(|c| (c.clone(), embedding.clone()))
                    .collect();
                store
                    .upsert_chunks_batch(&pairs, source_mtime)
                    .expect("upsert_chunks_batch failed");
            }
        } // drop store to flush WAL

        let ctx = create_test_context(&cqs_dir).expect("Failed to create test context");
        (dir, ctx)
    }

    /// Build a BatchContext with a single chunk. Thin wrapper around
    /// `ctx_with_chunks` for tests that only need one.
    fn ctx_with_chunk(
        id: &str,
        file: &str,
        language: Language,
        chunk_type: ChunkType,
        name: &str,
        signature: &str,
        content: &str,
    ) -> (TempDir, BatchContext) {
        ctx_with_chunks(vec![make_chunk(
            id, file, language, chunk_type, name, signature, content,
        )])
    }

    /// Build a BatchContext with zero chunks. For tests that only exercise
    /// the error-path parsing branches of `dispatch_search`.
    fn empty_ctx() -> (TempDir, BatchContext) {
        ctx_with_chunks(vec![])
    }

    /// Parse a `SearchArgs` by running the same clap pipeline as the daemon.
    /// This guarantees we hit exactly the defaults the production code sees,
    /// instead of hardcoding field values that could drift from the `Args`
    /// attributes.
    fn parse_search_args(cli_args: &[&str]) -> crate::cli::args::SearchArgs {
        let mut full = vec!["search"];
        full.extend_from_slice(cli_args);
        let input = BatchInput::try_parse_from(&full).expect("clap parse failed");
        match input.cmd {
            BatchCmd::Search { args, .. } => args,
            other => panic!("Expected Search, got {:?}", other),
        }
    }

    /// Exact-name `--name-only` query returns the matching chunk as the *top*
    /// result with a deterministic score of 1.0.
    ///
    /// Adversarial contract: the test fails if a different chunk sorts first
    /// — catches a sort/scoring regression in `search_by_name`.
    #[test]
    fn test_dispatch_search_name_only_exact_match_top_result() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/lib.rs:1:aaaa0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "process_data",
                "fn process_data(input: &str) -> String",
                "fn process_data(input: &str) -> String { input.to_uppercase() }",
            ),
            make_chunk(
                "src/lib.rs:7:aaaa0002",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "unrelated_helper",
                "fn unrelated_helper()",
                "fn unrelated_helper() { println!(\"noop\"); }",
            ),
        ]);

        let args = parse_search_args(&["process_data", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        assert_eq!(json["query"], "process_data");
        assert_eq!(json["total"], 1, "Expected exactly 1 matching chunk");
        let results = json["results"].as_array().expect("results is array");
        assert_eq!(results.len(), 1, "results.len() must match total");
        assert_eq!(
            results[0]["name"], "process_data",
            "Top result must be the exact-name match, not '{}'",
            results[0]["name"]
        );
        let score = results[0]["score"]
            .as_f64()
            .expect("score is finite number");
        assert!(
            (score - 1.0).abs() < 1e-6,
            "Exact-name match should score 1.0, got {score}. A regression in \
             score_name_match_pre_lower or the sort in search_by_name would \
             break this."
        );
        assert_eq!(results[0]["chunk_type"], "function");
        assert_eq!(results[0]["language"], "rust");
    }

    /// Prefix-match `--name-only` query returns the prefixed chunk with
    /// score 0.9 (from `score_name_match_pre_lower`).
    /// Ensures the FTS5 prefix-match (`name:"parse"*`) path actually fires.
    #[test]
    fn test_dispatch_search_name_only_prefix_match_ranks_first() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/parse.rs:1:bbbb0001",
                "src/parse.rs",
                Language::Rust,
                ChunkType::Function,
                "parse_config",
                "fn parse_config() -> Config",
                "fn parse_config() -> Config { Config::default() }",
            ),
            make_chunk(
                "src/lib.rs:1:bbbb0002",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "do_parse_config",
                "fn do_parse_config()",
                "fn do_parse_config() { parse_config(); }",
            ),
        ]);

        // "parse" is a prefix of parse_config (score 0.9) and a substring of
        // do_parse_config (score 0.7). The prefix-match must rank first.
        let args = parse_search_args(&["parse", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        let results = json["results"].as_array().expect("results is array");
        assert!(
            !results.is_empty(),
            "Expected at least one match for 'parse' prefix, got {}",
            results.len()
        );
        assert_eq!(
            results[0]["name"], "parse_config",
            "Prefix match (0.9) must outrank substring match (0.7); got '{}' first",
            results[0]["name"]
        );
        let top_score = results[0]["score"].as_f64().unwrap();
        assert!(
            (top_score - 0.9).abs() < 1e-6,
            "Prefix match should score 0.9, got {top_score}"
        );

        // If do_parse_config is also in the results, it must rank below.
        if results.len() > 1 {
            assert_eq!(
                results[1]["name"], "do_parse_config",
                "Second result should be the substring match"
            );
            let second = results[1]["score"].as_f64().unwrap();
            assert!(
                second < top_score,
                "Substring (score={second}) must rank below prefix (score={top_score})"
            );
        }
    }

    /// `--name-only --limit N` honours the limit *and* clamps out-of-range
    /// values via `limit.clamp(1, SEARCH_LIMIT_CAP)`. A regression
    /// that passed the raw limit through would return unlimited rows.
    #[test]
    fn test_dispatch_search_name_only_limit_clamp() {
        let chunks: Vec<Chunk> = (0..10)
            .map(|i| {
                make_chunk(
                    &format!("src/lib.rs:{i}:cccc{i:04}"),
                    "src/lib.rs",
                    Language::Rust,
                    ChunkType::Function,
                    &format!("handler_{i}"),
                    &format!("fn handler_{i}()"),
                    &format!("fn handler_{i}() {{}}"),
                )
            })
            .collect();
        let (_dir, ctx) = ctx_with_chunks(chunks);

        // Default limit is 5 (per SearchArgs `#[arg(default_value = "5")]`).
        let default = parse_search_args(&["handler", "--name-only"]);
        let json =
            dispatch_search(&ctx.build_view(None), &default).expect("dispatch_search failed");
        let results = json["results"].as_array().unwrap();
        assert_eq!(
            results.len(),
            5,
            "Default limit=5 must bound results; got {} with total={}",
            results.len(),
            json["total"]
        );
        assert_eq!(json["total"], 5, "total must equal results.len()");
        for r in results {
            let name = r["name"].as_str().unwrap();
            assert!(
                name.starts_with("handler_"),
                "All results must be handler_* prefix matches, got '{name}'"
            );
        }

        // Explicit limit=3 narrows further.
        let three = parse_search_args(&["handler", "--name-only", "--limit", "3"]);
        let json = dispatch_search(&ctx.build_view(None), &three).expect("dispatch_search failed");
        assert_eq!(json["total"], 3);
        assert_eq!(json["results"].as_array().unwrap().len(), 3);
    }

    /// No-match `--name-only` query returns empty results with `total: 0`.
    /// The schema must still be present — callers
    /// rely on `results[]` / `total` keys existing regardless of row count.
    ///
    /// This is the adversarial case the issue requires: a silent regression
    /// that returned the wrong chunk for a no-match query would violate this.
    #[test]
    fn test_dispatch_search_name_only_no_match_returns_empty() {
        let (_dir, ctx) = ctx_with_chunk(
            "src/lib.rs:1:dddd0001",
            "src/lib.rs",
            Language::Rust,
            ChunkType::Function,
            "alpha",
            "fn alpha()",
            "fn alpha() {}",
        );

        let args = parse_search_args(&["zxyvwu_no_such_name", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        assert_eq!(json["query"], "zxyvwu_no_such_name");
        assert_eq!(json["total"], 0, "No-match query must return total=0");
        assert_eq!(
            json["results"].as_array().unwrap().len(),
            0,
            "Empty results array must be present (callers depend on schema)"
        );
    }

    /// Name-only returns chunks from every inserted language. Covers the shared
    /// `SearchResultOutput` / `to_json_with_origin` rendering for non-Rust
    /// languages — a refactor that special-cased Rust or dropped the `language`
    /// field would break this.
    #[test]
    fn test_dispatch_search_name_only_cross_language_content() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/lib.rs:1:eeee0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "validate_input",
                "fn validate_input()",
                "fn validate_input() {}",
            ),
            make_chunk(
                "src/app.py:1:eeee0002",
                "src/app.py",
                Language::Python,
                ChunkType::Function,
                "validate_input",
                "def validate_input()",
                "def validate_input():\n    pass",
            ),
        ]);

        let args = parse_search_args(&["validate_input", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        let results = json["results"].as_array().unwrap();
        assert_eq!(
            json["total"], 2,
            "Expected both Rust and Python 'validate_input' chunks"
        );
        assert_eq!(results.len(), 2);

        // Both must have name=validate_input, score=1.0 (exact match).
        let languages: std::collections::HashSet<&str> = results
            .iter()
            .map(|r| r["language"].as_str().unwrap())
            .collect();
        assert!(
            languages.contains("rust"),
            "Rust result missing from {languages:?}"
        );
        assert!(
            languages.contains("python"),
            "Python result missing from {languages:?}"
        );
        for r in results {
            assert_eq!(r["name"], "validate_input");
            let score = r["score"].as_f64().unwrap();
            assert!(
                (score - 1.0).abs() < 1e-6,
                "Exact match on {} should score 1.0, got {score}",
                r["language"]
            );
        }
    }

    /// `--include-type` with an invalid chunk type name returns an error at
    /// the parsing boundary (dispatch_search:82-87),
    /// *before* the embedder is touched. This guards the
    /// `ChunkType::from_str` pipeline that refactors have historically
    /// broken by regressing the `FromStr` impl or the alias handling.
    ///
    /// We intentionally don't assert embedder output — the embedder path is
    /// not reached. The test is still content-asserting: it asserts the
    /// error message contains the offending input.
    #[test]
    fn test_dispatch_search_invalid_include_type_errors_fast() {
        let (_dir, ctx) = empty_ctx();
        let args = parse_search_args(&["anything", "--include-type", "not_a_real_type"]);
        let err = dispatch_search(&ctx.build_view(None), &args)
            .expect_err("Invalid --include-type must error, not silently return all types");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Invalid --include-type"),
            "Error message must reference --include-type flag, got: {msg}"
        );
        assert!(
            msg.contains("not_a_real_type"),
            "Error must surface the offending input, got: {msg}"
        );
    }

    /// `--exclude-type` with an invalid chunk type name errors symmetrically
    /// with `--include-type`. The exclude path
    /// is a common forgotten mirror in refactors that rename the include
    /// branch.
    #[test]
    fn test_dispatch_search_invalid_exclude_type_errors_fast() {
        let (_dir, ctx) = empty_ctx();
        let args = parse_search_args(&["anything", "--exclude-type", "bogusbogus"]);
        let err = dispatch_search(&ctx.build_view(None), &args)
            .expect_err("Invalid --exclude-type must error, not silently accept");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Invalid --exclude-type"),
            "Error message must reference --exclude-type flag, got: {msg}"
        );
        assert!(
            msg.contains("bogusbogus"),
            "Error must surface the offending input, got: {msg}"
        );
    }

    /// CLI-vs-daemon parity for the name-only search path (Phase 2a).
    ///
    /// The CLI core (`query_core`, name-only branch) and the daemon
    /// (`dispatch_search`, name-only branch) both retrieve via the same
    /// `store.search_by_name` primitive. This test pins that contract: the
    /// chunk names + scores the daemon emits match exactly what the shared
    /// primitive returns (which is what the CLI core wraps into
    /// `UnifiedResult`). A future edit that gave one surface a different
    /// name-only retrieval (a sort tweak, a different clamp) would break here.
    ///
    /// Name-only is the cheap parity surface: it skips the embedder entirely,
    /// so the test runs in well under a second without the ONNX stack.
    #[test]
    fn parity_name_only_daemon_matches_shared_primitive() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/lib.rs:1:ffff0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "parse_config",
                "fn parse_config() -> Config",
                "fn parse_config() -> Config { Config::default() }",
            ),
            make_chunk(
                "src/lib.rs:7:ffff0002",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "do_parse_config",
                "fn do_parse_config()",
                "fn do_parse_config() { parse_config(); }",
            ),
        ]);
        let view = ctx.build_view(None);

        // The shared retrieval primitive both surfaces call for name-only.
        let limit = 5usize;
        let primitive = view.store().search_by_name("parse", limit).unwrap();

        // Daemon name-only path.
        let args = parse_search_args(&["parse", "--name-only"]);
        let daemon = dispatch_search(&view, &args).expect("dispatch_search");
        let daemon_results = daemon["results"].as_array().expect("results array");

        // Same count and same ordered (name, score) pairs — the daemon JSON is
        // a thin projection of the primitive, identical to what the CLI core
        // wraps into UnifiedResult.
        assert_eq!(
            daemon_results.len(),
            primitive.len(),
            "daemon and shared primitive must return the same number of name-only hits"
        );
        for (i, sr) in primitive.iter().enumerate() {
            assert_eq!(
                daemon_results[i]["name"], sr.chunk.name,
                "name mismatch at rank {i}"
            );
            let dscore = daemon_results[i]["score"].as_f64().unwrap() as f32;
            assert!(
                (dscore - sr.score).abs() < 1e-6,
                "score mismatch at rank {i}: daemon={dscore} primitive={}",
                sr.score
            );
        }
    }

    /// Phase 2b parity: the daemon `dispatch_search` is byte-equal to driving
    /// `query_core` through the shared `SearchCtx` and serializing with the
    /// shared `build_unified_results_value`. This is the load-bearing invariant
    /// of the 2b convergence — the handler is a thin adapter, so its output
    /// `Value` must equal `serialize(core(view, daemon_args))` exactly.
    ///
    /// Covers the name-only surface (embedder-free, sub-second) across:
    /// - **happy**: a non-empty result set,
    /// - **empty**: a no-match query (the `{results:[], total:0}` envelope),
    /// - **trust-labeled**: the converged per-result schema carries the store
    ///   serializer's `type: "code"` + skip-when-default trust fields, identical
    ///   on both the adapter and the direct-core path.
    #[test]
    fn parity_daemon_dispatch_equals_core_plus_serializer() {
        use crate::cli::commands::search::query::query_core;

        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/lib.rs:1:9aaa0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "parse_config",
                "fn parse_config() -> Config",
                "fn parse_config() -> Config { Config::default() }",
            ),
            make_chunk(
                "src/lib.rs:7:9aaa0002",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "do_parse_config",
                "fn do_parse_config()",
                "fn do_parse_config() { parse_config(); }",
            ),
        ]);
        let view = ctx.build_view(None);

        // Re-derive the QueryArgs the adapter builds, then drive the core +
        // shared serializer directly. The adapter's output must equal this.
        let assert_parity = |cli_args: &[&str]| {
            let args = parse_search_args(cli_args);

            let daemon = dispatch_search(&view, &args).expect("dispatch_search");

            let qargs = daemon_query_args(&args);
            let output = query_core(&view, &qargs).expect("query_core");
            let parents_ref = if output.parents.is_empty() {
                None
            } else {
                Some(&output.parents)
            };
            let mut expected = crate::cli::display::build_unified_results_value(
                &output.results,
                &output.query,
                parents_ref,
                output.token_info,
            );
            if args.no_content {
                strip_content(&mut expected);
            }

            assert_eq!(
                daemon, expected,
                "daemon dispatch must be byte-equal to core+serializer for args {cli_args:?}"
            );
        };

        // Happy: a matching name-only query. `--no-stale-check` keeps the
        // byte-equality contract pure: the staleness `_meta` is adapter
        // surface I/O layered on top of the core output (the CLI does the
        // same at its render layer), covered by the dedicated
        // `*_stale_origins_*` tests below.
        assert_parity(&["parse", "--name-only", "--no-stale-check"]);
        // Empty: a no-match query yields the bare envelope on both paths.
        assert_parity(&["zzz_no_such_symbol", "--name-only", "--no-stale-check"]);

        // Trust-labeled / converged schema: the daemon result objects carry the
        // store serializer's `type: "code"` field (a field the old ChunkOutput
        // shape never emitted). Confirms the convergence onto the CLI schema.
        let happy = dispatch_search(
            &view,
            &parse_search_args(&["parse", "--name-only", "--no-stale-check"]),
        )
        .expect("dispatch_search");
        for r in happy["results"].as_array().expect("results array") {
            assert_eq!(
                r["type"], "code",
                "converged daemon schema must carry the store serializer's type tag"
            );
            // `trust_level` is skip-when-`user-code` by default; if present it
            // must be a string, never the old always-emitted shape leaking a
            // non-string.
            if let Some(tl) = r.get("trust_level") {
                assert!(tl.is_string(), "trust_level must serialize as a string");
            }
        }
    }

    // ─── Staleness meta (`_meta.stale_origins`) ─────────────────────────────

    /// Build a context whose single chunk has a REAL file on disk at
    /// `src/lib.rs`. `mtime_offset_ms` shifts the *stored* fingerprint
    /// relative to the disk mtime: 0 → fresh (fingerprints match),
    /// non-zero → stale (divergence in either direction counts under
    /// `FileFingerprint::matches`).
    fn ctx_with_file_on_disk(mtime_offset_ms: i64) -> (TempDir, BatchContext) {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(file_path.parent().expect("parent")).expect("mkdir src");
        // Disk content differs from the chunk content so a future
        // content-hash tiebreak can never mask the mtime divergence.
        std::fs::write(&file_path, "fn process_data() { /* edited */ }").expect("write file");
        let disk_mtime = cqs::duration_to_mtime_millis(
            file_path
                .metadata()
                .expect("metadata")
                .modified()
                .expect("modified")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("duration"),
        );

        let chunk = make_chunk(
            "src/lib.rs:1:5a1e0001",
            "src/lib.rs",
            Language::Rust,
            ChunkType::Function,
            "process_data",
            "fn process_data()",
            "fn process_data() {}",
        );
        ctx_with_chunks_in(dir, vec![chunk], Some(disk_mtime + mtime_offset_ms))
    }

    /// A stale origin (stored mtime diverges from disk) surfaces as
    /// `_meta.stale_origins` on the dispatch payload — the machine-readable
    /// signal `write_json_line` lifts onto the envelope and the CLI client
    /// turns into the stderr warning.
    #[test]
    fn test_dispatch_search_stale_origin_lands_in_meta() {
        let (_dir, ctx) = ctx_with_file_on_disk(-60_000);
        let args = parse_search_args(&["process_data", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search");

        assert_eq!(json["total"], 1, "sanity: the chunk must match");
        assert_eq!(
            json["_meta"]["stale_origins"],
            serde_json::json!(["src/lib.rs"]),
            "stale origin must surface in _meta.stale_origins; got: {json}"
        );
    }

    /// `--no-stale-check` skips the check entirely — no `_meta` key, matching
    /// the CLI-direct flag semantics.
    #[test]
    fn test_dispatch_search_no_stale_check_omits_meta() {
        let (_dir, ctx) = ctx_with_file_on_disk(-60_000);
        let args = parse_search_args(&["process_data", "--name-only", "--no-stale-check"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search");

        assert_eq!(json["total"], 1, "sanity: the chunk must match");
        assert!(
            json.get("_meta").is_none(),
            "--no-stale-check must skip the staleness meta; got: {json}"
        );
    }

    /// Fresh index (stored fingerprint matches disk) emits NO `_meta` key —
    /// the skip-when-empty convention `worktree_stale` established.
    #[test]
    fn test_dispatch_search_fresh_index_omits_meta() {
        let (_dir, ctx) = ctx_with_file_on_disk(0);
        let args = parse_search_args(&["process_data", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search");

        assert_eq!(json["total"], 1, "sanity: the chunk must match");
        assert!(
            json.get("_meta").is_none(),
            "fresh index must emit no _meta key (skip-when-empty); got: {json}"
        );
    }

    // ─── Worktree overlay (result-trust §3, PR-3 daemon path) ────────────────

    use std::process::Command as StdCommand;

    /// Run `git` in `dir`, panicking with stderr on failure.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Build a `BatchContext` whose project root is a real git repo with a
    /// committed corpus, plus a linked worktree. Returns
    /// `(holder, ctx, parent_root, worktree_root)`. The index has one chunk so
    /// `create_test_context` opens cleanly; the corpus content is irrelevant to
    /// the validation tests, which only exercise the overlay-root path check.
    fn overlay_ctx() -> (TempDir, BatchContext, PathBuf, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        // Parent repo lives at <tmp>/parent so the worktree can be a sibling.
        let parent = dir.path().join("parent");
        std::fs::create_dir_all(parent.join("src")).expect("mkdir parent/src");
        std::fs::write(parent.join("src/lib.rs"), "pub fn alpha() -> i32 { 1 }\n")
            .expect("write lib.rs");
        git(&parent, &["init", "-q", "-b", "main"]);
        git(&parent, &["config", "user.email", "t@e.com"]);
        git(&parent, &["config", "user.name", "T"]);
        git(&parent, &["add", "-A"]);
        git(&parent, &["commit", "-q", "-m", "init"]);

        // Linked worktree.
        let wt = dir.path().join("wt");
        git(
            &parent,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );

        // Build the index under parent/.cqs so create_test_context's
        // root == parent.
        let cqs_dir = parent.join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join("index.db");
        {
            let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
            emb_vec[0] = 1.0;
            let embedding = Embedding::new(emb_vec);
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init store");
            let chunk = make_chunk(
                "src/lib.rs:1:aaaa0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "alpha",
                "fn alpha() -> i32",
                "fn alpha() -> i32 { 1 }",
            );
            store
                .upsert_chunks_batch(&[(chunk, embedding)], Some(0))
                .expect("upsert");
        }

        let ctx = create_test_context(&cqs_dir).expect("create_test_context");
        let parent = dunce::canonicalize(&parent).unwrap_or(parent);
        let wt = dunce::canonicalize(&wt).unwrap_or(wt);
        (dir, ctx, parent, wt)
    }

    /// #16 (security pin) — a `--overlay-root` that is NOT a worktree of the
    /// served project is rejected with a wire error before any file is read.
    /// An unvalidated path would be an arbitrary-directory read+embed primitive
    /// over the socket.
    #[test]
    fn overlay_daemon_rejects_foreign_root() {
        let (holder, ctx, _parent, _wt) = overlay_ctx();
        let view = ctx.build_view(None);

        // A directory that exists but is NOT a worktree of this project (a
        // bare tempdir sibling). Rejection must be loud.
        let foreign = holder.path().join("foreign");
        std::fs::create_dir_all(&foreign).expect("mkdir foreign");
        let err = view
            .set_validated_overlay_request(&foreign)
            .expect_err("foreign overlay-root must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not a worktree of the served project"),
            "expected a foreign-root rejection; got: {msg}"
        );
        // The request must NOT have been stamped.
        assert!(
            view.overlay_request.borrow().is_none(),
            "a rejected overlay-root must leave overlay_request unset"
        );

        // A non-existent path is rejected at canonicalize (also not a read
        // primitive).
        let missing = holder.path().join("does-not-exist");
        assert!(
            view.set_validated_overlay_request(&missing).is_err(),
            "a non-existent overlay-root must be rejected"
        );
    }

    /// The genuine worktree of the served project passes validation and is
    /// stamped onto the per-dispatch request.
    #[test]
    fn overlay_daemon_accepts_own_worktree() {
        let (_holder, ctx, _parent, wt) = overlay_ctx();
        let view = ctx.build_view(None);
        view.set_validated_overlay_request(&wt)
            .expect("own worktree must validate");
        assert_eq!(
            view.overlay_request.borrow().as_deref(),
            Some(wt.as_path()),
            "validated worktree must be stamped onto overlay_request"
        );
    }

    /// `prepare_overlay_request` clears any leftover per-thread overlay meta at
    /// the top of dispatch — an error path on a prior query must not leak its
    /// `_meta.worktree_overlay` into the next request on a reused worker thread.
    #[test]
    fn overlay_prepare_clears_stale_meta() {
        let (_holder, ctx, _parent, _wt) = overlay_ctx();
        let view = ctx.build_view(None);

        // Simulate a prior query having left meta on this thread.
        cqs::worktree_overlay::set_overlay_meta(
            cqs::worktree_overlay::OverlayMeta::SkippedDeltaTooLarge,
        );
        // A no-overlay search must clear it (flag off → early return after clear).
        let args = parse_search_args(&["alpha", "--name-only"]);
        prepare_overlay_request(&view, &args).expect("prepare");
        assert!(
            cqs::worktree_overlay::take_overlay_meta().is_none(),
            "prepare_overlay_request must clear leftover overlay meta"
        );
    }

    /// `prepare_overlay_request` rejects a foreign `--overlay-root` as a wire
    /// error (the dispatch-path counterpart of `overlay_daemon_rejects_foreign_root`).
    #[test]
    fn overlay_prepare_rejects_foreign_root() {
        let (holder, ctx, _parent, _wt) = overlay_ctx();
        let view = ctx.build_view(None);
        let foreign = holder.path().join("foreign2");
        std::fs::create_dir_all(&foreign).expect("mkdir");
        let args = parse_search_args(&[
            "alpha",
            "--name-only",
            "--overlay",
            "--overlay-root",
            foreign.to_str().unwrap(),
        ]);
        let err = prepare_overlay_request(&view, &args)
            .expect_err("foreign overlay-root must bubble up as a wire error");
        assert!(
            format!("{err:#}").contains("not a worktree of the served project"),
            "expected foreign-root rejection from the dispatch path"
        );
    }

    /// #13 — repeat-query cache hit + fingerprint-invalidates-on-edit, driven
    /// through the real `BatchView::overlay()` path with a real embedder.
    /// Gated behind `slow-tests` (cold-loads the embedder via the daemon
    /// embedder slot). Asserts:
    ///   - first `overlay()` builds (stats.build_ms recorded, chunks indexed);
    ///   - second `overlay()` within the debounce reuses the SAME build (same
    ///     fingerprint, returned Arc points at the cached store) without
    ///     re-running git;
    ///   - editing the worktree file changes the fingerprint, so a forced
    ///     re-validation (debounce 0) rebuilds with a different fingerprint.
    #[cfg(feature = "slow-tests")]
    #[test]
    fn overlay_repeat_query_cache_hit_and_invalidates_on_edit() {
        use crate::cli::commands::search::search_ctx::SearchCtx;
        let (_holder, ctx, _parent, wt) = overlay_ctx();

        // Install a real embedder into the context's embedder slot so
        // BatchView::overlay() can build.
        let embedder = match cqs::Embedder::new_cpu(cqs::embedder::ModelConfig::resolve(None, None))
        {
            Ok(e) => std::sync::Arc::new(e),
            Err(e) => {
                eprintln!("skipping overlay cache-hit e2e: embedder init failed: {e}");
                return;
            }
        };
        assert!(ctx.adopt_embedder(embedder), "embedder slot must be empty");

        // Dirty the worktree (lane-new file).
        std::fs::write(
            wt.join("src/feature.rs"),
            "pub fn brand_new_overlay_symbol() -> i32 { 7 }\n",
        )
        .expect("write feature.rs");

        let view = ctx.build_view(None);
        view.set_validated_overlay_request(&wt)
            .expect("validate wt");

        // First resolve: builds.
        let ov1 = view.overlay().expect("first overlay build");
        let fp1 = ov1.fingerprint;
        assert!(ov1.stats.chunks_indexed > 0, "overlay indexed the new file");

        // Second resolve within the debounce window: same cached build (the
        // returned Arc points at the same `WorktreeOverlay` instance — no
        // rebuild, no second git run).
        let ov2 = view.overlay().expect("second overlay (cache hit)");
        assert_eq!(
            ov2.fingerprint, fp1,
            "cache hit reuses the same fingerprint"
        );
        assert!(
            std::sync::Arc::ptr_eq(&ov1, &ov2),
            "cache hit must return the same cached overlay instance"
        );

        // Edit the worktree file, then force re-validation (debounce 0) via a
        // direct LRU call: the fingerprint must change and trigger a rebuild.
        std::fs::write(
            wt.join("src/feature.rs"),
            "pub fn brand_new_overlay_symbol() -> i32 { 999 }\n",
        )
        .expect("edit feature.rs");
        let parser = cqs::parser::Parser::new().expect("parser");
        let embedder_ref = view.embedder().expect("embedder");
        let rebuilt = super::super::super::view::get_overlay_via_lru(
            &view.overlays,
            &wt,
            &view.root,
            &parser,
            embedder_ref,
            None,
            std::time::Duration::from_millis(0),
        )
        .expect("rebuild after edit")
        .expect("non-clean worktree");
        assert_ne!(
            rebuilt.fingerprint, fp1,
            "editing the worktree file must move the overlay fingerprint"
        );
    }

    /// Carried pin 5b — the masked_origins↔chunk.file path-representation e2e
    /// against REAL producer output. A real git delta in a real worktree, the
    /// parent index built by the SAME pipeline (`overlay_reindex_files` =
    /// `reindex_files`, real embedder), and the overlay built daemon-side via
    /// `build_overlay`, must mask the parent's ACTUAL indexed origins. The unit
    /// tests hand-build both sides; only this end-to-end run catches a
    /// representation mismatch between git's repo-relative paths and the store's
    /// `normalize_path`'d origins.
    ///
    /// Scenario: parent `src/lib.rs` defines `alpha`; the worktree DELETES
    /// `alpha` from `src/lib.rs` (function-deleted-from-modified-file, the
    /// origin-level mask falsifier, plan #3). After the overlay merge, a search
    /// for `alpha` must NOT return the parent's now-dead `src/lib.rs:alpha` hit.
    #[cfg(feature = "slow-tests")]
    #[test]
    fn overlay_masks_real_parent_origin_for_deleted_function() {
        let dir = TempDir::new().expect("tempdir");
        let parent = dir.path().join("parent");
        std::fs::create_dir_all(parent.join("src")).expect("mkdir");
        // Two functions in the parent so masking `src/lib.rs` is observable
        // (we assert `alpha` disappears while the overlay leg can still answer).
        std::fs::write(
            parent.join("src/lib.rs"),
            "pub fn alpha_unique_token() -> i32 { 1 }\npub fn beta_unique_token() -> i32 { 2 }\n",
        )
        .expect("write lib.rs");
        git(&parent, &["init", "-q", "-b", "main"]);
        git(&parent, &["config", "user.email", "t@e.com"]);
        git(&parent, &["config", "user.name", "T"]);
        git(&parent, &["add", "-A"]);
        git(&parent, &["commit", "-q", "-m", "init"]);

        let embedder = match cqs::Embedder::new_cpu(cqs::embedder::ModelConfig::resolve(None, None))
        {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skipping overlay masking e2e: embedder init failed: {e}");
                return;
            }
        };
        let parser = cqs::parser::Parser::new().expect("parser");

        // Build the parent index with the SAME pipeline the overlay uses (real
        // embedder), so both sides of the mask share path representation.
        let cqs_dir = parent.join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join("index.db");
        {
            let store = Store::open(&index_path).expect("open store");
            let model_info =
                ModelInfo::new(&embedder.model_config().repo, embedder.embedding_dim());
            store.init(&model_info).expect("init store");
            // set_dim must follow init (init's metadata write doesn't update the
            // already-read dim field).
            let mut store = store;
            store.set_dim(model_info.dimensions);
            crate::cli::watch::overlay_reindex_files(
                &parent,
                &store,
                &[PathBuf::from("src/lib.rs")],
                &parser,
                &embedder,
                None,
                true,
            )
            .expect("index parent");
        }

        // Linked worktree; DELETE `alpha` from src/lib.rs (modify, not remove).
        let wt = dir.path().join("wt");
        git(
            &parent,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );
        std::fs::write(
            wt.join("src/lib.rs"),
            "pub fn beta_unique_token() -> i32 { 2 }\n",
        )
        .expect("edit wt lib.rs (delete alpha)");

        let wt = dunce::canonicalize(&wt).unwrap_or(wt);

        let ctx = create_test_context(&cqs_dir).expect("create_test_context");
        assert!(
            ctx.adopt_embedder(std::sync::Arc::new(embedder)),
            "embedder slot empty"
        );
        let view = ctx.build_view(None);

        // Sanity: WITHOUT the overlay, the parent's dead `alpha` is retrievable.
        let baseline = dispatch_search(
            &view,
            &parse_search_args(&["alpha_unique_token", "--name-only"]),
        )
        .expect("baseline search");
        let baseline_has_alpha = baseline["results"]
            .as_array()
            .map(|rs| {
                rs.iter()
                    .any(|r| r["name"] == "alpha_unique_token" && r["file"] == "src/lib.rs")
            })
            .unwrap_or(false);
        assert!(
            baseline_has_alpha,
            "sanity: parent index must contain src/lib.rs:alpha_unique_token; got {baseline}"
        );

        // WITH the overlay: stamp the worktree, then the dead parent hit must
        // be masked (origin src/lib.rs is in the delta).
        view.set_validated_overlay_request(&wt)
            .expect("validate wt");
        let overlaid = dispatch_search(
            &view,
            &parse_search_args(&[
                "alpha_unique_token",
                "--name-only",
                "--overlay",
                "--overlay-root",
                wt.to_str().unwrap(),
            ]),
        )
        .expect("overlaid search");
        let overlaid_has_dead_alpha = overlaid["results"]
            .as_array()
            .map(|rs| {
                rs.iter()
                    .any(|r| r["name"] == "alpha_unique_token" && r["file"] == "src/lib.rs")
            })
            .unwrap_or(false);
        assert!(
            !overlaid_has_dead_alpha,
            "the overlay must mask the parent's dead src/lib.rs:alpha_unique_token \
             (origin-level mask over REAL producer paths); got {overlaid}"
        );
    }
}
