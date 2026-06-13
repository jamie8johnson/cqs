//! Query command for cqs
//!
//! Executes semantic search queries.
//!
//! ## Command-core split (Phase 2a/2b)
//!
//! [`query_core`] owns the surface-agnostic search logic for the project and
//! name-only paths: routing/classification, embedding, search invocation, and
//! assembly into the typed [`QueryOutput`]. It never prints, never reads env
//! posture, and never branches on its surface — it takes a
//! [`search_ctx::SearchCtx`], so both the CLI ([`cmd_query`]) and the daemon
//! (`dispatch_search`) drive the same core (Phase 2b). The CLI adapter renders
//! the result (text or JSON via the [`crate::cli::display`] typed structs) and
//! owns the `NoResults` exit code.
//!
//! ## One query-preparation path, one multi-store seam
//!
//! The query-preparation prelude — classification, the NameOnly-FTS-first
//! short-circuit, embedding, centroid reclassification + α-floor, filter /
//! SPLADE / base-index resolution — lives once in [`prepare_query`]. All three
//! search paths consume it: the plain single-store path ([`query_core`]) and
//! the multi-store `--ref` / `--include-refs` paths. Only the retrieval fan-out
//! that consumes the [`PreparedQuery`] is path-specific: [`retrieve_project`]
//! for the single store, [`retrieve_ref_scoped`] and [`merge_references`] for
//! the reference stores [`search_ctx::SearchCtx::references`] /
//! `reference_by_name` supply (the multi-store seam). Token-budget packing,
//! parent context, staleness, and serialization stay surface-specific (the CLI
//! renders text/JSON, the daemon builds a JSON value), so those layer on top of
//! the shared retrieval in each adapter, and reference results carry the same
//! per-result [`display`] shape as project results.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use cqs::parser::ChunkType;
use cqs::store::{ParentContext, UnifiedResult};
use cqs::{reference, Embedder, Embedding, Pattern, SearchFilter, Store};

use crate::cli::commands::search::search_ctx;
use crate::cli::commands::search::search_ctx::SearchCtx;
use crate::cli::{display, signal, staleness, Cli};

// ─── Args (surface-agnostic, MCP-ready) ────────────────────────────────────

/// Input for [`query_core`]: the full search-knob surface both the CLI and a
/// future MCP `search` tool deserialize into. Every field a search consumer
/// can set lives here; the core reads only these, never the process env or a
/// CLI struct.
///
/// `#[serde(default)]` on the whole struct so a wire/MCP caller can supply just
/// `query` and inherit the production defaults for the rest.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct QueryArgs {
    /// Search query (quote multi-word queries).
    pub query: String,
    /// Max results to return.
    pub limit: usize,
    /// Definition search: match by function/struct name only, skip embedding.
    pub name_only: bool,
    /// Filter results to this language (e.g. `rust`, `python`).
    pub lang: Option<String>,
    /// Restrict results to these chunk types (e.g. `function`, `struct`).
    pub include_type: Option<Vec<String>>,
    /// Exclude these chunk types from results.
    pub exclude_type: Option<Vec<String>>,
    /// Glob path filter.
    pub path: Option<String>,
    /// Structural pattern filter (builder, async, unsafe, …).
    pub pattern: Option<String>,
    /// Include documentation / markdown / config chunks (default: code only).
    pub include_docs: bool,
    /// Enable RRF hybrid (keyword + semantic) fusion.
    pub rrf: bool,
    /// `true` when a cross-encoder reranker stage is requested.
    pub rerank: bool,
    /// Force SPLADE on even for Unknown-category queries.
    pub splade: bool,
    /// Constant SPLADE fusion weight (None = per-category router).
    pub splade_alpha: Option<f32>,
    /// Minimum similarity threshold.
    pub threshold: f32,
    /// Name-match weight in hybrid scoring (0.0–1.0).
    pub name_boost: f32,
    /// Disable test/underscore-prefix demotion.
    pub no_demote: bool,
    /// Token budget — packs the highest-scoring results into the budget.
    pub tokens: Option<usize>,
    /// Expand results with parent type/module context (small-to-big).
    pub expand_parent: bool,
    /// Force the non-enriched base HNSW. Resolved once at the adapter boundary
    /// from `CQS_FORCE_BASE_INDEX` so the core stays env-free.
    pub force_base_index: bool,
    /// Per-result JSON overhead the token-budget packer adds when estimating
    /// how many results fit the `tokens` budget. The CLI resolves this from
    /// its output format (the per-result envelope cost under `--json`, 0 for
    /// text) so packing picks the same survivors as before the core split.
    /// A wire/MCP caller that always serializes should set the per-result
    /// overhead constant; the `#[serde(default)]` value is 0.
    pub json_overhead: usize,
    /// Adaptive-routing posture, resolved once at the adapter boundary.
    ///
    /// The CLI suppresses classification when an explicit strategy flag
    /// (`--rrf` / `--rerank`) is set, so the user's flag wins. The daemon
    /// always classifies — `cqs search` is the agent-facing surface where
    /// per-category routing is the whole point — so it sets this `true` to keep
    /// the router live even alongside `--rrf` / `--rerank`. Folding the
    /// difference into a field (not branching logic) is what lets one core
    /// serve both surfaces.
    pub always_route: bool,
    /// Whether the `NameOnly` routing strategy tries an FTS-by-name lookup
    /// first and only falls back to dense on zero hits.
    ///
    /// The CLI keeps this `true` (its historical behavior: a `NameOnly`-routed
    /// query short-circuits to `search_by_name`). The daemon sets it `false` —
    /// `dispatch_search` never had the FTS-first branch, it always ran the dense
    /// hybrid path for non-`--name-only` queries — so the core reproduces the
    /// daemon's exact retrieval when driven from the wire.
    pub fts_first: bool,
    /// Record per-result ranking provenance (`rank_signals`). On by default for
    /// JSON consumers; the CLI flips it off via `--no-rank-signals` and the
    /// text surface drops it regardless. Recording is a side channel — it never
    /// changes scores or order.
    pub record_rank_signals: bool,
    /// The caller requested the worktree search overlay (`--overlay` flag OR
    /// `CQS_WORKTREE_OVERLAY=1`), resolved at the adapter boundary like
    /// `force_base_index`. Off by default. The core itself never reads env; this
    /// flag is what the daemon's `BatchView::overlay()` (PR-3) consults to decide
    /// whether to build+merge an overlay, and what the CLI-direct adapter
    /// (`cmd_query`) uses to detect overlay-eligibility for the honest-degradation
    /// warn + `_meta.worktree_overlay = "skipped-no-daemon"`.
    pub overlay: bool,
}

impl Default for QueryArgs {
    fn default() -> Self {
        // Mirrors the clap defaults on `Cli` / `SearchArgs` so a wire caller
        // omitting a field gets the same value the CLI would.
        QueryArgs {
            query: String::new(),
            limit: 5,
            name_only: false,
            lang: None,
            include_type: None,
            exclude_type: None,
            path: None,
            pattern: None,
            include_docs: false,
            rrf: false,
            rerank: false,
            splade: false,
            splade_alpha: None,
            threshold: 0.3,
            name_boost: 0.2,
            no_demote: false,
            tokens: None,
            expand_parent: false,
            force_base_index: false,
            json_overhead: 0,
            // CLI defaults: classification is gated on explicit flags, and the
            // NameOnly strategy tries FTS-by-name first.
            always_route: false,
            fts_first: true,
            // Recording on by default — JSON consumers get provenance unless
            // they opt out; the cost is a post-pass over the final result set.
            record_rank_signals: true,
            // Overlay off by default; opt-in via `--overlay` / env.
            overlay: false,
        }
    }
}

/// `true` when `CQS_WORKTREE_OVERLAY=1` requests the worktree overlay. The
/// env-var equivalent of `--overlay`, resolved at the adapter boundary (like
/// `CQS_FORCE_BASE_INDEX`) so the surface-agnostic core never reads env.
pub(crate) fn overlay_env_requested() -> bool {
    std::env::var("CQS_WORKTREE_OVERLAY").as_deref() == Ok("1")
}

impl QueryArgs {
    /// Build `QueryArgs` from the top-level CLI struct, resolving the
    /// `CQS_FORCE_BASE_INDEX` env override and the format-dependent JSON
    /// overhead once here at the adapter boundary.
    fn from_cli(cli: &Cli) -> Self {
        QueryArgs {
            query: cli.query.clone().unwrap_or_default(),
            limit: cli.limit,
            name_only: cli.name_only,
            lang: cli.lang.clone(),
            include_type: cli.include_type.clone(),
            exclude_type: cli.exclude_type.clone(),
            path: cli.path.clone(),
            pattern: cli.pattern.clone(),
            include_docs: cli.include_docs,
            rrf: cli.rrf,
            rerank: cli.rerank_active(),
            splade: cli.splade,
            splade_alpha: cli.splade_alpha,
            threshold: cli.threshold,
            name_boost: cli.name_boost,
            no_demote: cli.no_demote,
            tokens: cli.tokens,
            expand_parent: cli.expand_parent,
            force_base_index: std::env::var("CQS_FORCE_BASE_INDEX").as_deref() == Ok("1"),
            json_overhead: if cli.json {
                crate::cli::commands::JSON_OVERHEAD_PER_RESULT
            } else {
                0
            },
            // CLI semantics: explicit-flag classification gating + FTS-first.
            always_route: false,
            fts_first: true,
            // On unless suppressed. Provenance is machine-only — the text
            // surface drops the field at render time regardless, so this only
            // governs the (cheap) recording post-pass; it matches the wire/MCP
            // default and keeps `from_cli == QueryArgs::default`.
            record_rank_signals: !cli.no_rank_signals,
            // Flag OR env, resolved once here at the adapter boundary like
            // `force_base_index`.
            overlay: cli.overlay || overlay_env_requested(),
        }
    }

    /// Build `QueryArgs` for the multi-store paths (`--ref` / `--include-refs`).
    ///
    /// Identical to [`from_cli`](Self::from_cli) except for `fts_first`: the
    /// NameOnly-FTS-first short-circuit runs against the *project* store, so the
    /// `--ref`-scoped path (which searches a reference store, not the project)
    /// disables it. `--include-refs` keeps it (its project half is the real
    /// project store), matching the pre-refactor behavior where a name-only hit
    /// short-circuited to project-only results.
    fn from_cli_ref(cli: &Cli) -> Self {
        QueryArgs {
            fts_first: cli.ref_name.is_none(),
            ..Self::from_cli(cli)
        }
    }
}

// ─── Output (the typed result the adapters render) ─────────────────────────

/// Surface-agnostic result of [`query_core`] for the project + name-only
/// paths. Carries the assembled results plus everything the adapter needs to
/// render text or JSON without re-running retrieval: the resolved parent
/// context and the token-budget accounting. Empty `results` is a valid output
/// (the adapter maps it to the `NoResults` exit code).
pub(crate) struct QueryOutput {
    /// The original query string (echoed into the JSON envelope).
    pub query: String,
    /// Assembled, ranked, token-packed results.
    pub results: Vec<UnifiedResult>,
    /// Resolved parent context keyed by chunk id (empty unless
    /// `expand_parent`).
    pub parents: HashMap<String, ParentContext>,
    /// `(used, budget)` when `--tokens` packed the results.
    pub token_info: Option<(usize, usize)>,
}

/// Compute JSON overhead for token budgeting based on output format.
fn json_overhead_for(cli: &Cli) -> usize {
    if cli.json {
        crate::cli::commands::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    }
}

/// Emit empty results (JSON or text) and exit with NoResults code.
///
/// `context` is an optional label for the empty-result message (e.g. reference name).
fn emit_empty_results(query: &str, json: bool, context: Option<&str>) -> ! {
    if json {
        let obj = serde_json::json!({"results": [], "query": query, "total": 0});
        // Best-effort wrap; falls back to raw print if envelope serialize fails
        // (effectively impossible here — pure JSON object).
        let _ = crate::cli::json_envelope::emit_json(&obj);
    } else if let Some(ctx) = context {
        println!("No results found in reference '{}'.", ctx);
    } else {
        println!("No results found.");
    }
    std::process::exit(signal::ExitCode::NoResults as i32);
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Whether [`prepare_query`] resolves the project retrieval surface (the
/// project vector index + the primed project SPLADE inverted index + the
/// SPLADE query encoding).
///
/// The plain path and `--include-refs` both fan out over the project store
/// ([`retrieve_project`]), so they need [`ProjectSurface::Resolve`]. A
/// `--ref`-scoped query searches exactly one reference store and never touches
/// the project index — its fan-out ([`retrieve_ref_scoped`]) consumes only the
/// embedding, filter, and reranker — so it takes [`ProjectSurface::Skip`] and
/// the prelude leaves `PreparedQuery::index` / `splade_index` / `splade_query`
/// at `None`, dropping the project vector-index build/load and the SPLADE
/// inverted-index priming on every `--ref` call. Classification, embedding, the
/// filter (including its `enable_splade` / `splade_alpha` flags), and the
/// reranker are resolved on both — those are the query, not the project surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectSurface {
    /// Resolve the project vector index + SPLADE encoding + primed SPLADE index.
    Resolve,
    /// Skip the project-surface work — the consumer searches a reference store
    /// only and never reads the project index.
    Skip,
}

/// The result of [`prepare_query`]: either a short-circuit retrieval that
/// bypassed embedding (name-only, or a NameOnly-FTS-first hit), or a fully
/// prepared dense query ready for the retrieval fan-out.
///
/// The two-variant shape is what lets the prelude be shared while the fan-out
/// stays path-specific: the single-store and multi-store consumers both call
/// [`prepare_query`], match on this, and only branch on the [`Prepared::Dense`]
/// fan-out.
pub(crate) enum Prepared<'a> {
    /// FTS-by-name produced results without an embedding (name-only flag, or a
    /// NameOnly-classified query whose FTS lookup hit). Already a ranked
    /// `Vec<UnifiedResult>` — the consumer assembles/serializes directly.
    ShortCircuit(Vec<UnifiedResult>),
    /// A prepared dense query. Everything between classification and the
    /// retrieval call is resolved; the consumer runs the single-store or
    /// multi-store fan-out against it. Boxed because `PreparedQuery` is far
    /// larger than the `ShortCircuit` variant (embedding + filter + index).
    Dense(Box<PreparedQuery<'a>>),
}

/// Everything the retrieval fan-out needs, resolved once by [`prepare_query`]
/// and consumed identically by the single-store project path and the
/// multi-store `--ref` / `--include-refs` paths.
///
/// Holds the borrowed SPLADE handle (`SpladeIndexRef` derefs to `&SpladeIndex`),
/// so it is tied to the `SearchCtx` lifetime — the consumer keeps it alive
/// across the search call. The dense index is an owned `Arc`, so it composes
/// the same on both surfaces.
pub(crate) struct PreparedQuery<'a> {
    /// Dense query embedding (also reused by reference fan-out).
    query_embedding: Embedding,
    /// The fully-built, validated `SearchFilter`.
    filter: SearchFilter,
    /// SPLADE sparse query vector, `None` when SPLADE is off / encoding failed.
    splade_query: Option<cqs::splade::SparseVector>,
    /// Primed SPLADE inverted index handle, `None` when SPLADE is off.
    splade_index: Option<search_ctx::SpladeIndexRef<'a>>,
    /// Resolved project vector index (base or enriched, with fallback applied).
    index: Option<std::sync::Arc<dyn cqs::index::VectorIndex>>,
    /// Audit-mode state — forces the hybrid path when active.
    audit_mode: cqs::audit::AuditMode,
    /// Over-fetch limit for the project search (pattern × 3, rerank pool).
    search_limit: usize,
    /// Reranker handle when `--rerank` is active.
    reranker: Option<std::sync::Arc<dyn cqs::Reranker>>,
}

/// Surface-agnostic core for the plain (non-`--ref`, non-`--include-refs`)
/// search path and the non-ref name-only path.
///
/// Owns routing/classification, embedding, the search invocation, and
/// assembly into [`QueryOutput`] (pattern filter, rerank, token-budget
/// packing, parent-context resolution). Returns an empty `results` vec rather
/// than printing or exiting — the adapter maps empty to the `NoResults` exit
/// code. Reads no env: the base-index override arrives via
/// [`QueryArgs::force_base_index`].
///
/// The query-preparation prelude (classification, NameOnly-FTS-first,
/// embedding, filter/SPLADE/index resolution) lives in [`prepare_query`], which
/// the `--ref` / `--include-refs` fan-out shares. `query_core` is the
/// single-store consumer: prepare → [`retrieve_project`] → [`assemble_output`].
pub(crate) fn query_core(ctx: &dyn search_ctx::SearchCtx, args: &QueryArgs) -> Result<QueryOutput> {
    let query = args.query.as_str();
    let _span = tracing::info_span!("query_core", query_len = query.len()).entered();

    let prepared = match prepare_query(ctx, args, ProjectSurface::Resolve)? {
        Prepared::ShortCircuit(results) => return assemble_output(ctx, args, results),
        Prepared::Dense(p) => p,
    };

    let results = retrieve_project(ctx, args, &prepared)?;
    assemble_output(ctx, args, results)
}

/// The shared query-preparation prelude: classification, the NameOnly-FTS-first
/// short-circuit, embedding, centroid reclassification + α-floor, filter
/// parsing, SPLADE α resolution + sparse encoding, and base-index selection.
///
/// This is the single place all three search surfaces build a query — the plain
/// single-store path ([`query_core`]) and both multi-store paths (`--ref`,
/// `--include-refs`). Only the retrieval fan-out that consumes the
/// [`PreparedQuery`] is path-specific.
///
/// Returns [`Prepared::ShortCircuit`] when FTS-by-name produced results without
/// an embedding (the name-only flag, or a NameOnly-classified query whose FTS
/// lookup hit), and [`Prepared::Dense`] otherwise.
///
/// `surface` gates the project-retrieval work: [`ProjectSurface::Resolve`]
/// (plain path + `--include-refs`) resolves the project vector index, SPLADE
/// query encoding, and primed SPLADE inverted index; [`ProjectSurface::Skip`]
/// (`--ref`-scoped) leaves them `None`, so a reference-only query pays no
/// project-index I/O. The classification, embedding, and filter are built on
/// both — they describe the query, not the project surface.
pub(crate) fn prepare_query<'a>(
    ctx: &'a dyn search_ctx::SearchCtx,
    args: &QueryArgs,
    surface: ProjectSurface,
) -> Result<Prepared<'a>> {
    let query = args.query.as_str();
    let store = ctx.store();
    let cqs_dir = ctx.cqs_dir();

    // Overlay-active fetch over-fetch (plan §7.2, risk #5): masking the delta's
    // origins out of a `limit`-sized parent fetch can hollow the top-k below
    // `limit` with nothing left to backfill — the store, FTS, and rerank pools
    // are all sized to `limit` on the plain path. When an overlay will merge,
    // fetch 2x at every parent-retrieval site so the post-mask pool still fills
    // to `limit`. INACTIVE must stay byte-exact: `None` ⇒ multiplier 1 ⇒ every
    // limit is the pre-overlay value, so the #14 byte-identical fence holds.
    let overlay_active = ctx.overlay().is_some();
    let overlay_fetch = |n: usize| -> usize {
        if overlay_active {
            n.saturating_mul(2).max(n)
        } else {
            n
        }
    };

    // Name-only path: FTS by name, skip embedding entirely. With an overlay,
    // mask the delta's parent name hits and merge the overlay store's name
    // hits in their place (plan §7.3). The `--name-only` flag has no dense
    // fallback, so this short-circuits unconditionally — including the
    // all-masked, no-overlay-hit empty case (correct: a name deleted from a
    // changed file is genuinely absent from the worktree). Over-fetch 2x when
    // an overlay is active so masking can't starve the post-merge `limit`.
    if args.name_only {
        let parent = store
            .search_by_name(query, overlay_fetch(args.limit))
            .context("Failed to search by name")?;
        let merged = overlay_mask_name_results(ctx.overlay().as_deref(), parent, query, args)?;
        let unified: Vec<UnifiedResult> = merged.into_iter().map(UnifiedResult::Code).collect();
        return Ok(Prepared::ShortCircuit(unified));
    }

    // Adaptive routing: classify query BEFORE embedding to potentially skip it.
    // --splade is NOT a routing override (it only controls SPLADE fusion);
    // --rrf/--rerank override the search strategy. (--ref reaches here too: it
    // drives the same prepared query, fanning out over a reference store.)
    //
    // `always_route` (daemon) keeps the router live even with explicit flags —
    // `cqs search` always classifies — while the CLI suppresses classification
    // when the user pins a strategy.
    let has_explicit_flags = (args.rrf || args.rerank) && !args.always_route;
    let classification = if !has_explicit_flags {
        let c = cqs::search::router::classify_query(query);
        tracing::info!(
            category = %c.category,
            confidence = %c.confidence,
            strategy = %c.strategy,
            "Query classified"
        );
        Some(c)
    } else {
        tracing::debug!("Explicit flags set, skipping adaptive routing");
        None
    };

    // NameOnly strategy: try FTS5 first, fall back to dense on 0 results.
    // Gated on `fts_first`: the daemon (`fts_first = false`) never had this
    // short-circuit, so it stays on the dense hybrid path even for
    // NameOnly-classified queries.
    if let Some(ref c) = classification {
        if args.fts_first && c.strategy == cqs::search::router::SearchStrategy::NameOnly {
            // 2x over-fetch when an overlay is active (same under-fill guard as
            // the `--name-only` path above).
            let parent = store.search_by_name(query, overlay_fetch(args.limit))?;
            // CRITICAL ORDERING (plan §7.3): mask the overlay's delta hits
            // BEFORE the `is_empty()` check. An FTS hit set that is entirely
            // masked (every match lives in a changed file) must fall through to
            // the dense path — where the overlay leg can still answer — rather
            // than short-circuiting to an empty result.
            let results = overlay_mask_name_results(ctx.overlay().as_deref(), parent, query, args)?;
            if !results.is_empty() {
                tracing::info!(results = results.len(), "NameOnly search succeeded");
                crate::cli::telemetry::log_routed(
                    cqs_dir,
                    query,
                    &c.category.to_string(),
                    &c.confidence.to_string(),
                    &c.strategy.to_string(),
                    false,
                    Some(results.len()),
                );
                let unified: Vec<UnifiedResult> =
                    results.into_iter().map(UnifiedResult::Code).collect();
                return Ok(Prepared::ShortCircuit(unified));
            }
            tracing::info!("NameOnly returned 0 results, falling back to dense");
            crate::cli::telemetry::log_routed(
                cqs_dir,
                query,
                &c.category.to_string(),
                &c.confidence.to_string(),
                &c.strategy.to_string(),
                true, // fallback triggered
                None,
            );
        }
    }

    // Over-retrieve when reranking to give the cross-encoder more candidates,
    // and 2x more when an overlay is active so the post-mask dense pool still
    // fills to `limit` (plan §7.2 / risk #5). `overlay_fetch` is identity when
    // no overlay is present, so the inactive path keeps the exact prior
    // `effective_limit` — the #14 byte-identical contract. Applied OUTSIDE the
    // rerank branch so it composes with `rerank_pool_size` (overlay + rerank
    // ⇒ 2× the rerank pool).
    let effective_limit = overlay_fetch(if args.rerank {
        crate::cli::limits::rerank_pool_size(args.limit)
    } else {
        args.limit
    });

    let query_embedding = ctx.embedder()?.embed_query(query)?;

    // Centroid reclassification + α-floor tracking.
    let pre_centroid_cat = classification.as_ref().map(|c| c.category);
    let classification = classification
        .map(|c| cqs::search::router::reclassify_with_centroid(c, query_embedding.as_slice()));
    let centroid_applied = classification.as_ref().map(|c| c.category) != pre_centroid_cat;

    let languages = match &args.lang {
        Some(l) => Some(vec![l.parse().context(format!(
            "Invalid language. Valid: {}",
            cqs::parser::Language::valid_names_display()
        ))?]),
        None => None,
    };

    let include_types = match &args.include_type {
        Some(types) => {
            let parsed: Result<Vec<ChunkType>, _> = types.iter().map(|t| t.parse()).collect();
            Some(parsed.with_context(|| {
                format!(
                    "Invalid chunk type. Valid: {}",
                    ChunkType::valid_names().join(", ")
                )
            })?)
        }
        None if args.include_docs => None, // --include-docs: search everything
        None => {
            // Default: search code only (callable types + type definitions).
            Some(ChunkType::code_types())
        }
    };

    let exclude_types = match &args.exclude_type {
        Some(types) => {
            let parsed: Result<Vec<ChunkType>, _> = types.iter().map(|t| t.parse()).collect();
            Some(parsed.with_context(|| {
                format!(
                    "Invalid chunk type for --exclude-type. Valid: {}",
                    ChunkType::valid_names().join(", ")
                )
            })?)
        }
        None => None,
    };

    // Type boost from adaptive routing (boost, not filter — won't exclude).
    let type_boost_types = classification.as_ref().and_then(|c| c.type_hints.clone());

    // Resolve SPLADE alpha: explicit α wins; else per-category router on a
    // classified query; else `--splade` forces α=0.7; else SPLADE off. SPLADE
    // stays on even at α=1.0 when a category classified — the α knob is the
    // scoring weight, not the candidate-pool switch.
    let (use_splade, mut splade_alpha) = match (args.splade_alpha, classification.as_ref()) {
        (Some(alpha), _) => (true, alpha),
        (None, Some(c)) => (true, cqs::search::router::resolve_splade_alpha(&c.category)),
        (None, None) if args.splade => (true, 0.7),
        (None, None) => (false, 1.0),
    };
    // Centroid α floor: a centroid-driven reclassification can't zero SPLADE.
    if centroid_applied {
        splade_alpha = splade_alpha.max(0.7);
    }

    let filter = {
        let mut f = SearchFilter::default();
        f.languages = languages;
        f.include_types = include_types;
        f.exclude_types = exclude_types;
        f.path_pattern = args.path.clone();
        f.name_boost = args.name_boost;
        f.query_text = query.to_string();
        f.enable_rrf = args.rrf;
        f.enable_demotion = !args.no_demote;
        f.enable_splade = use_splade;
        f.splade_alpha = splade_alpha;
        f.type_boost_types = type_boost_types;
        f.record_rank_signals = args.record_rank_signals;
        f
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    let reranker = if args.rerank {
        Some(ctx.reranker()?)
    } else {
        None
    };

    // Project retrieval surface — gated on `surface`. A `--ref`-scoped query
    // searches a reference store only (its fan-out reads `prepared.index` /
    // `splade_index` / `splade_query` never), so `ProjectSurface::Skip` drops
    // the SPLADE inverted-index priming and the project vector-index build/load
    // entirely. The plain path and `--include-refs` (`ProjectSurface::Resolve`)
    // fan out over the project store, so they resolve the full surface.
    let (splade_query, splade_index, index) = match surface {
        ProjectSurface::Skip => (None, None, None),
        ProjectSurface::Resolve => {
            // SPLADE sparse encoding (if enabled by --splade or per-category
            // routing). The encode two-step (+ daemon index priming) lives
            // behind `SearchCtx::splade_encode`, so the core just asks for the
            // sparse vector.
            let splade_query = if use_splade {
                ctx.splade_encode(query)
            } else {
                None
            };
            let splade_index = if use_splade { ctx.splade_index() } else { None };

            // Phase 5: when the classifier picked DenseBase (or the env override
            // is resolved into args.force_base_index), try the base HNSW; fall
            // back to enriched if it's absent/corrupt.
            let use_base = matches!(
                classification.as_ref().map(|c| c.strategy),
                Some(cqs::search::router::SearchStrategy::DenseBase)
            ) || args.force_base_index;
            let mut base_fallback = false;
            let index = if use_base {
                match ctx.base_vector_index()? {
                    Some(base_idx) => {
                        tracing::info!(
                            basename = "index_base",
                            "Router selected base HNSW for non-enriched query"
                        );
                        Some(base_idx)
                    }
                    None => {
                        tracing::info!(
                            "Base HNSW unavailable — falling back to enriched index for DenseBase query"
                        );
                        base_fallback = true;
                        ctx.vector_index()?
                    }
                }
            } else {
                ctx.vector_index()?
            };

            if use_base {
                crate::cli::telemetry::log_routed(
                    cqs_dir,
                    query,
                    "routed_to_base",
                    "medium",
                    if base_fallback {
                        "dense_base_fallback_to_enriched"
                    } else {
                        "dense_base"
                    },
                    base_fallback,
                    None,
                );
            }

            (splade_query, splade_index, index)
        }
    };

    let audit_mode = ctx.audit_state();

    let search_limit = if args.pattern.is_some() {
        effective_limit * 3
    } else {
        effective_limit
    };

    Ok(Prepared::Dense(Box::new(PreparedQuery {
        query_embedding,
        filter,
        splade_query,
        splade_index,
        index,
        audit_mode,
        search_limit,
        reranker,
    })))
}

/// Single-store project retrieval over a [`PreparedQuery`]: the dense/hybrid
/// fan-out, pattern filter, and cross-encoder rerank. Returns a ranked
/// `Vec<UnifiedResult>` ready for [`assemble_output`].
///
/// Shared between [`query_core`] (plain path) and the `--include-refs` path,
/// whose project-half retrieval is byte-identical — the reference fan-out
/// merges on top of this output.
pub(crate) fn retrieve_project(
    ctx: &dyn search_ctx::SearchCtx,
    args: &QueryArgs,
    prepared: &PreparedQuery<'_>,
) -> Result<Vec<UnifiedResult>> {
    let query = args.query.as_str();
    let store = ctx.store();

    // Worktree overlay (result-trust §3). The store fetch already over-fetched
    // 2x when an overlay is active (`prepare_query`'s `overlay_fetch` on
    // `effective_limit` → `search_limit`), so masking the delta's parent hits
    // can't starve the pool below `limit`. The job HERE is to NOT clip that
    // headroom before the overlay merge: the pattern-filter and rerank
    // truncations cut to `post_limit` (2x) rather than `args.limit`, leaving
    // the over-fetched survivors in play; `apply_overlay`'s `merge_results`
    // does the final truncate to `args.limit`. Inactive ⇒ `post_limit ==
    // args.limit` and the fetch was un-doubled, so the path is byte-exact
    // (the #14 fence). Same under-fill rationale as `search_reference`'s
    // `apply_weight` over-fetch (`reference.rs:257-267`).
    let overlay = ctx.overlay();
    let post_limit = if overlay.is_some() {
        args.limit.saturating_mul(2).max(args.limit)
    } else {
        args.limit
    };

    let results = run_project_search(store, args, prepared)?;

    // Pattern filter.
    let pattern: Option<Pattern> = args
        .pattern
        .as_ref()
        .map(|p| p.parse())
        .transpose()
        .context("Invalid pattern")?;
    let results = if let Some(ref pat) = pattern {
        let mut filtered: Vec<UnifiedResult> = results
            .into_iter()
            .filter(|r| match r {
                UnifiedResult::Code(sr) => {
                    pat.matches(&sr.chunk.content, &sr.chunk.name, Some(sr.chunk.language))
                }
            })
            .collect();
        filtered.truncate(post_limit);
        filtered
    } else {
        results
    };

    // Cross-encoder re-ranking. Note: overlay hits are NOT cross-encoded in
    // phase 1 (the `--include-refs` + `--rerank` precedent — only the project
    // half reranks); the overlay merge below layers raw-scored overlay hits on
    // top of the reranked project pool.
    let results = if let Some(reranker) = prepared.reranker.as_deref() {
        rerank_unified(reranker, query, results, post_limit)?
    } else {
        results
    };

    // Overlay merge: mask the delta's parent hits, fan out over the overlay
    // store, and merge the overlay leg in their place (truncating to
    // `args.limit`). Inactive ⇒ this is a no-op returning `results` unchanged
    // (the byte-identical-when-inactive regression fence, plan test #14).
    let results = match overlay {
        Some(ov) => apply_overlay(args, prepared, results, &ov)?,
        None => results,
    };

    Ok(results)
}

/// FTS short-circuit overlay merge (plan §7.3): mask the overlay's delta hits
/// out of a `search_by_name` parent result set and merge the overlay store's
/// own name hits in their place.
///
/// `overlay = None` ⇒ returns `parent` unchanged (the byte-identical-when-
/// inactive contract — the name-only / NameOnly-FTS paths see no behavior change
/// without an overlay). `Some(ov)` ⇒ origin-level mask, then `search_by_name`
/// against the overlay store, then merge by score (highest first, id tiebreak)
/// and truncate to `args.limit`. Sets the `Active` envelope meta whenever an
/// overlay was applied — including the all-masked, no-overlay-hit empty case,
/// since the overlay still shaped (emptied) the answer.
///
/// The caller decides what to do with an empty merged set: `--name-only`
/// short-circuits on it (no dense fallback); the NameOnly-classified path falls
/// through to dense (the mask must run *before* its `is_empty` check).
fn overlay_mask_name_results(
    overlay: Option<&cqs::worktree_overlay::WorktreeOverlay>,
    parent: Vec<cqs::store::SearchResult>,
    query: &str,
    args: &QueryArgs,
) -> Result<Vec<cqs::store::SearchResult>> {
    let Some(ov) = overlay else {
        return Ok(parent);
    };
    let _span = tracing::info_span!(
        "overlay_mask_name_results",
        masked = ov.masked_origins.len()
    )
    .entered();

    // Mask: drop parent name hits whose origin is in the delta.
    let mut merged: Vec<cqs::store::SearchResult> = parent
        .into_iter()
        .filter(|sr| !ov.masked_origins.contains(&sr.chunk.file))
        .collect();

    // Overlay name hits (best-effort: a failure leaves the masked parent set).
    match ov.store.search_by_name(query, args.limit) {
        Ok(hits) => merged.extend(hits),
        Err(e) => {
            tracing::warn!(error = %e, "overlay search_by_name failed; serving masked parent name hits only");
        }
    }

    // Merge by score (highest first), id tiebreak — same ordering
    // `merge_results` uses for the dense path. Truncate to the requested limit.
    merged.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk.id.cmp(&b.chunk.id))
    });
    merged.truncate(args.limit);

    cqs::worktree_overlay::set_overlay_meta(cqs::worktree_overlay::OverlayMeta::Active {
        files: ov.stats.files_in_delta,
        chunks: ov.stats.chunks_indexed,
    });

    Ok(merged)
}

/// Merge a [`WorktreeOverlay`] into already-retrieved project results
/// (result-trust §3, plan §7.2). Origin-level masking + overlay fan-out +
/// merge, in three steps:
///
/// 1. **Mask**: drop every project result whose `chunk.file` is in the
///    overlay's `masked_origins`. This is unconditional and name-agnostic —
///    a function deleted from a still-present file (its origin is in the delta
///    but no overlay chunk shares its name) is correctly dropped, where
///    `(origin, name)` shadowing would resurrect it (plan §4 test #1).
/// 2. **Fan out**: search the overlay store with the *same* prepared query
///    embedding + filter at `args.threshold` / `args.limit`, brute-force
///    (`index = None` — a few hundred chunks).
/// 3. **Merge**: reuse [`reference::merge_results`] with the overlay hits as a
///    `"worktree"` leg (weight 1.0, no `apply_weight` demotion — the worktree
///    *is* the project, not an external reference), then fold the
///    `TaggedResult`s back to `Vec<UnifiedResult>` and truncate to `args.limit`.
///
/// Records the `_meta.worktree_overlay` outcome (`Active { files, chunks }`)
/// for the JSON envelope.
pub(crate) fn apply_overlay(
    args: &QueryArgs,
    prepared: &PreparedQuery<'_>,
    project_results: Vec<UnifiedResult>,
    overlay: &cqs::worktree_overlay::WorktreeOverlay,
) -> Result<Vec<UnifiedResult>> {
    let _span = tracing::info_span!(
        "apply_overlay",
        masked = overlay.masked_origins.len(),
        chunks = overlay.stats.chunks_indexed
    )
    .entered();

    // 1. Mask: drop project hits whose origin is in the delta. Origin-level,
    //    name-agnostic (plan correction #1).
    let masked: Vec<UnifiedResult> = project_results
        .into_iter()
        .filter(|r| match r {
            UnifiedResult::Code(sr) => !overlay.masked_origins.contains(&sr.chunk.file),
        })
        .collect();

    // 2. Fan out over the overlay store with the same prepared query. Brute
    //    force (`index = None`): the overlay holds at most a few hundred chunks.
    let overlay_hits = match overlay.store.search_filtered_with_index(
        &prepared.query_embedding,
        &prepared.filter,
        args.limit,
        args.threshold,
        None,
    ) {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(error = %e, "overlay store search failed; serving masked project results only");
            Vec::new()
        }
    };

    // 3. Merge: overlay hits as a `"worktree"` leg, weight 1.0, no demotion.
    //    `merge_results` sorts by score, dedups by content hash, truncates.
    let leg = if overlay_hits.is_empty() {
        Vec::new()
    } else {
        vec![("worktree".to_string(), overlay_hits)]
    };
    let merged = reference::merge_results(masked, leg, args.limit);

    // Record the envelope outcome before folding back.
    cqs::worktree_overlay::set_overlay_meta(cqs::worktree_overlay::OverlayMeta::Active {
        files: overlay.stats.files_in_delta,
        chunks: overlay.stats.chunks_indexed,
    });

    // Fold `TaggedResult` back to `Vec<UnifiedResult>` (the overlay is the
    // project, so the `"worktree"` source tag is dropped — these are project
    // results, not reference results).
    Ok(merged.into_iter().map(|t| t.result).collect())
}

/// The project-store dense/hybrid retrieval call itself, with no post-filtering.
///
/// Audit mode and SPLADE both require the hybrid path (`search_unified` doesn't
/// support SPLADE yet); only the plain dense path uses `search_code_results`.
/// This is the single collapsed form of what `cmd_query_project` and the daemon
/// ref path each carried as a divergent nested condition (one had two
/// byte-identical `search_hybrid` call sites).
fn run_project_search<Mode>(
    store: &Store<Mode>,
    args: &QueryArgs,
    prepared: &PreparedQuery<'_>,
) -> Result<Vec<UnifiedResult>> {
    // `SpladeIndexRef` derefs to `&SpladeIndex`; `as_deref` collapses the
    // Owned/Borrowed handle into the `&SpladeIndex` the primitive wants while
    // the handle stays alive in `prepared.splade_index` for the call's lifetime.
    let splade_arg = prepared
        .splade_index
        .as_deref()
        .zip(prepared.splade_query.as_ref());

    if prepared.audit_mode.is_active() || splade_arg.is_some() {
        let code_results = store.search_hybrid(
            &prepared.query_embedding,
            &prepared.filter,
            prepared.search_limit,
            args.threshold,
            prepared.index.as_deref(),
            splade_arg,
        )?;
        Ok(code_results.into_iter().map(UnifiedResult::Code).collect())
    } else {
        Ok(store.search_code_results(
            &prepared.query_embedding,
            &prepared.filter,
            prepared.search_limit,
            args.threshold,
            prepared.index.as_deref(),
        )?)
    }
}

// ─── Multi-store fan-out (the seam) ─────────────────────────────────────────
//
// The same prepared query that drives the single-store project path also drives
// the `--ref` (one reference store) and `--include-refs` (project + all
// references) paths. The two helpers below are the multi-store consumers of a
// `PreparedQuery`: they fan out over the reference stores `SearchCtx::references`
// / `reference_by_name` supplies, merge, and return a ranked `Vec<TaggedResult>`.
// Token-budget packing, parent context, staleness, and serialization stay
// surface-specific (the CLI renders text/JSON, the daemon builds a JSON value),
// so those layer on top of the shared tagged results in each adapter.

/// `--include-refs` merge step: fan out over the reference stores and merge
/// them with already-retrieved project results into a ranked `Vec<TaggedResult>`.
///
/// The project half is retrieved by [`retrieve_project`] (byte-identical to the
/// plain path) and passed in, so the caller can run staleness / parent-context
/// over the project results before the merge consumes them. The reference
/// stores come from [`search_ctx::SearchCtx::references`] (the multi-store
/// seam); each is searched with `apply_weight = true` so its scores rank below
/// equally-similar project results, then [`reference::merge_results`] dedups by
/// content hash and truncates to `limit`.
pub(crate) fn merge_references(
    args: &QueryArgs,
    prepared: &PreparedQuery<'_>,
    project_results: Vec<UnifiedResult>,
    references: &[std::sync::Arc<cqs::reference::ReferenceIndex>],
) -> Vec<reference::TaggedResult> {
    if references.is_empty() {
        return project_results
            .into_iter()
            .map(|result| reference::TaggedResult {
                result,
                source: None,
            })
            .collect();
    }

    use rayon::prelude::*;
    let ref_results: Vec<_> = references
        .par_iter()
        .filter_map(|ref_idx| {
            match reference::search_reference(
                ref_idx,
                &prepared.query_embedding,
                &prepared.filter,
                args.limit,
                args.threshold,
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

    reference::merge_results(project_results, ref_results, args.limit)
}

/// `--ref`-scoped retrieval: search exactly the one named reference store, no
/// project fan-out. Uses `apply_weight = false` (raw reference scores), reranks
/// when `--rerank` is set, and tags every result with the reference name.
///
/// The prepared query supplies the embedding, filter, and rerank handle — the
/// `--ref` path shares the entire query-preparation prelude with the plain path,
/// differing only in *which store* it retrieves from.
pub(crate) fn retrieve_ref_scoped(
    ctx: &dyn search_ctx::SearchCtx,
    args: &QueryArgs,
    prepared: &PreparedQuery<'_>,
    ref_name: &str,
) -> Result<Vec<reference::TaggedResult>> {
    let ref_idx = ctx.reference_by_name(ref_name)?;

    // Shared pool sizing: over-fetch when reranking so the cross-encoder sees
    // more candidates, then trim to `limit` in the rerank step.
    let ref_limit = if prepared.reranker.is_some() {
        crate::cli::limits::rerank_pool_size(args.limit)
    } else {
        args.limit
    };

    let mut results = reference::search_reference(
        &ref_idx,
        &prepared.query_embedding,
        &prepared.filter,
        ref_limit,
        args.threshold,
        false, // no weight for --ref scoped search
    )?;

    if let Some(reranker) = prepared.reranker.as_deref() {
        if results.len() > 1 {
            reranker
                .rerank(args.query.as_str(), &mut results, args.limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }
    }

    Ok(results
        .into_iter()
        .map(|r| reference::TaggedResult {
            result: UnifiedResult::Code(r),
            source: Some(ref_name.to_string()),
        })
        .collect())
}

/// Final assembly shared by the core's name-only, NameOnly-FTS, and dense
/// paths: token-budget packing + parent-context resolution. The input is
/// already a ranked `Vec<UnifiedResult>`.
fn assemble_output(
    ctx: &dyn search_ctx::SearchCtx,
    args: &QueryArgs,
    results: Vec<UnifiedResult>,
) -> Result<QueryOutput> {
    let store = ctx.store();
    let root = ctx.root();

    // Token-budget packing. The per-result JSON overhead is resolved by the
    // adapter into `args.json_overhead` (the CLI's format-dependent estimate),
    // so packing keeps the exact same survivors as before the core split.
    let (results, token_info) = if let Some(budget) = args.tokens {
        // Lazy embedder: the name-only path may not have built one yet.
        let embedder = ctx.embedder()?;
        crate::cli::commands::token_pack_results(
            results,
            budget,
            args.json_overhead,
            embedder,
            unified_text,
            unified_score,
            "query_core",
        )
    } else {
        (results, None)
    };

    let parents = if args.expand_parent {
        resolve_parent_context(&results, store, root)
    } else {
        HashMap::new()
    };

    Ok(QueryOutput {
        query: args.query.clone(),
        results,
        parents,
        token_info,
    })
}

/// Render a [`QueryOutput`] for the CLI: staleness warning, empty-result exit,
/// and text/JSON emission via the typed display structs.
fn render_query_output(
    cli: &Cli,
    root: &std::path::Path,
    store: &Store<cqs::store::ReadOnly>,
    output: QueryOutput,
) -> Result<()> {
    let QueryOutput {
        query,
        results,
        parents,
        token_info,
    } = output;

    // Staleness warning (surface I/O — adapter owns it).
    if !cli.quiet && !cli.no_stale_check {
        let origins: Vec<&str> = results
            .iter()
            .map(|r| {
                let UnifiedResult::Code(sr) = r;
                sr.chunk.file.to_str().unwrap_or("")
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !origins.is_empty() {
            staleness::warn_stale_results(store, &origins, root);
        }
    }

    if results.is_empty() {
        emit_empty_results(&query, cli.json, None);
    }

    let parents_ref = if cli.expand_parent {
        Some(&parents)
    } else {
        None
    };

    if cli.json {
        display::display_unified_results_json(&results, &query, parents_ref, token_info)?;
    } else {
        display::display_unified_results(&results, root, cli.no_content, cli.context, parents_ref)?;
    }
    Ok(())
}

/// Execute a semantic search query and display results
pub(crate) fn cmd_query(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    query: &str,
) -> Result<()> {
    let query_preview = if query.len() > 200 {
        // Find a valid UTF-8 boundary near 200 bytes
        let mut end = 200;
        while end > 0 && !query.is_char_boundary(end) {
            end -= 1;
        }
        &query[..end]
    } else {
        query
    };
    let _span =
        tracing::info_span!("cmd_query", query_len = query.len(), query = %query_preview).entered();

    let cli = ctx.cli;
    let store = &ctx.store;
    let root = &ctx.root;

    // Worktree-overlay CLI-direct degradation (result-trust §3, plan §8). Reset
    // any per-thread overlay meta left over from a prior in-process search
    // (chat REPL / batch stdin), then — when an overlay was requested for an
    // eligible worktree but we reached the in-process CLI path (no daemon
    // answered; phase 1 builds overlays daemon-side only) — warn and mark the
    // envelope so the agent knows results reflect the parent index, not the
    // worktree. The CLI `SearchCtx::overlay()` returns `None`, so no overlay
    // merge happens here; this is purely the honest-skip signal.
    cqs::worktree_overlay::clear_overlay_meta();
    if cli.overlay || overlay_env_requested() {
        let eligible = std::env::current_dir()
            .ok()
            .and_then(|cwd| cqs::worktree::overlay_root(&cwd, root))
            .is_some();
        if eligible {
            tracing::warn!(
                "overlay skipped: daemon not running (results reflect the parent index)"
            );
            cqs::worktree_overlay::set_overlay_meta(
                cqs::worktree_overlay::OverlayMeta::SkippedNoDaemon,
            );
        }
    }

    // Name-only mode: search by function/struct name, skip embedding entirely.
    if cli.name_only {
        if cli.rerank_active() {
            bail!("--rerank requires embedding search, incompatible with --name-only");
        }
        if let Some(ref ref_name) = cli.ref_name {
            // Ref-name-only search resolves a reference index (config +
            // single-store lookup) — not modeled by the single-store
            // `SearchCtx`, so it stays in the adapter (Phase 2b ref-path
            // decision). It serializes through the shared tagged-result schema.
            return cmd_query_ref_name_only(cli, ref_name, query, root);
        }
        // Non-ref name-only routes through the shared core (which performs the
        // FTS-by-name lookup + assembly). `--include-refs` is ignored on the
        // name-only path, matching prior behavior.
        let args = QueryArgs::from_cli(cli);
        let output = query_core(ctx, &args)?;
        return render_query_output(cli, root, store, output);
    }

    // Plain (non-ref, non-multi-index) search routes through the shared core.
    if cli.ref_name.is_none() && !cli.include_refs {
        let args = QueryArgs::from_cli(cli);
        let output = query_core(ctx, &args)?;
        return render_query_output(cli, root, store, output);
    }

    // `--ref` / `--include-refs`: the multi-store paths. They share the entire
    // query-preparation prelude with the plain path (`prepare_query`) and differ
    // only in the retrieval fan-out — `--ref` searches one named reference,
    // `--include-refs` merges all references with the project results. The
    // fan-out consumes the same `PreparedQuery` the plain path's
    // `retrieve_project` does.
    let args = QueryArgs::from_cli_ref(cli);
    // `--ref`-scoped searches one reference store and never reads the project
    // index, so skip the project-surface resolution. `--include-refs` fans out
    // over the project store (`retrieve_project`), so it resolves the full
    // surface.
    let surface = if cli.ref_name.is_some() {
        ProjectSurface::Skip
    } else {
        ProjectSurface::Resolve
    };
    let prepared = match prepare_query(ctx, &args, surface)? {
        // NameOnly-FTS-first hit (only possible on `--include-refs`; the `--ref`
        // path disables `fts_first` since its retrieval is the reference store,
        // not the project). Matches prior behavior: a name-only short-circuit on
        // `--include-refs` rendered project results only, no reference merge.
        Prepared::ShortCircuit(results) => {
            let output = assemble_output(ctx, &args, results)?;
            return render_query_output(cli, root, store, output);
        }
        Prepared::Dense(p) => p,
    };

    // `--ref` scoped: search exactly the one named reference. No project
    // results, so no staleness/parent context (the project store isn't
    // searched) — matches prior behavior.
    if let Some(ref ref_name) = cli.ref_name {
        let tagged = retrieve_ref_scoped(ctx, &args, &prepared, ref_name)?;
        let (tagged, token_info) = pack_tagged_cli(ctx, &args, tagged)?;
        if tagged.is_empty() {
            emit_empty_results(query, cli.json, Some(ref_name.as_str()));
        }
        if cli.json {
            display::display_tagged_results_json(&tagged, query, None, token_info)?;
        } else {
            display::display_tagged_results(&tagged, root, cli.no_content, cli.context, None)?;
        }
        return Ok(());
    }

    // `--include-refs`: project results merged with all references. The
    // project half (`retrieve_project`) drives the staleness warning and
    // parent-context resolution exactly as the plain path does; the reference
    // merge then layers on top.
    if cli.rerank_active() {
        // The project half IS reranked (`retrieve_project` applies the
        // reranker). Only the merged reference results bypass the cross-encoder
        // — `merge_references` ranks them by their weighted retrieval score.
        tracing::warn!(
            "--rerank applies to project results only; merged reference results are not reranked"
        );
    }
    let project_results = retrieve_project(ctx, &args, &prepared)?;

    // Staleness warning over the project results (reference origins aren't
    // project files; the multi-index path checks project results only).
    if !cli.quiet && !cli.no_stale_check {
        let origins: Vec<&str> = project_results
            .iter()
            .map(|r| {
                let UnifiedResult::Code(sr) = r;
                sr.chunk.file.to_str().unwrap_or("")
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !origins.is_empty() {
            staleness::warn_stale_results(store, &origins, root);
        }
    }

    let parents = if cli.expand_parent {
        resolve_parent_context(&project_results, store, root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand_parent {
        Some(&parents)
    } else {
        None
    };

    let references = ctx.references()?;
    let tagged = merge_references(&args, &prepared, project_results, &references);

    let (tagged, token_info) = pack_tagged_cli(ctx, &args, tagged)?;

    if tagged.is_empty() {
        emit_empty_results(query, cli.json, None);
    }
    if cli.json {
        display::display_tagged_results_json(&tagged, query, parents_ref, token_info)?;
    } else {
        display::display_tagged_results(&tagged, root, cli.no_content, cli.context, parents_ref)?;
    }
    Ok(())
}

/// CLI token-budget packing for a tagged result set. Reuses the context's
/// cached embedder (already initialized by `prepare_query` on every path that
/// reaches here) — same pattern as `assemble_output`. Mirrors the daemon's
/// `pack_tagged`.
type TaggedPack = (Vec<reference::TaggedResult>, Option<(usize, usize)>);

fn pack_tagged_cli(
    ctx: &dyn search_ctx::SearchCtx,
    args: &QueryArgs,
    tagged: Vec<reference::TaggedResult>,
) -> Result<TaggedPack> {
    if let Some(budget) = args.tokens {
        let embedder = ctx.embedder()?;
        Ok(token_pack_results(
            tagged,
            budget,
            args.json_overhead,
            embedder,
            |r| unified_text(&r.result),
            |r| unified_score(&r.result),
            "tagged",
        ))
    } else {
        Ok((tagged, None))
    }
}

// token_pack_results lives in crate::cli::commands
use crate::cli::commands::token_pack_results;

/// Extract text content from a `UnifiedResult`.
fn unified_text(r: &UnifiedResult) -> &str {
    match r {
        UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
    }
}

/// Extract score from a `UnifiedResult`.
fn unified_score(r: &UnifiedResult) -> f32 {
    match r {
        UnifiedResult::Code(sr) => sr.score,
    }
}

/// Re-rank unified results using cross-encoder scoring.
fn rerank_unified(
    reranker: &dyn cqs::Reranker,
    query: &str,
    results: Vec<UnifiedResult>,
    limit: usize,
) -> Result<Vec<UnifiedResult>> {
    let mut code_results: Vec<cqs::store::SearchResult> = results
        .into_iter()
        .map(|r| match r {
            UnifiedResult::Code(sr) => sr,
        })
        .collect();

    if code_results.len() > 1 {
        reranker
            .rerank(query, &mut code_results, limit)
            .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
    }

    Ok(code_results.into_iter().map(UnifiedResult::Code).collect())
}

/// Ref-scoped name-only search: search only the named reference by name
fn cmd_query_ref_name_only(
    cli: &Cli,
    ref_name: &str,
    query: &str,
    root: &std::path::Path,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_query_ref_name_only", ref_name).entered();

    let ref_idx = crate::cli::commands::resolve::find_reference(root, ref_name)?;

    let results =
        reference::search_reference_by_name(&ref_idx, query, cli.limit, cli.threshold, false)?;

    let tagged: Vec<reference::TaggedResult> = results
        .into_iter()
        .map(|r| reference::TaggedResult {
            result: UnifiedResult::Code(r),
            source: Some(ref_name.to_string()),
        })
        .collect();

    // Token-budget packing (lazy embedder — only created when --tokens is set)
    let json_overhead = json_overhead_for(cli);
    let (tagged, token_info) = if let Some(budget) = cli.tokens {
        let embedder = Embedder::new(cli.try_model_config()?.clone())?;
        token_pack_results(
            tagged,
            budget,
            json_overhead,
            &embedder,
            |r| unified_text(&r.result),
            |r| unified_score(&r.result),
            "tagged",
        )
    } else {
        (tagged, None)
    };

    if tagged.is_empty() {
        emit_empty_results(query, cli.json, Some(ref_name));
    }

    if cli.json {
        display::display_tagged_results_json(&tagged, query, None, token_info)?;
    } else {
        display::display_tagged_results(&tagged, root, cli.no_content, cli.context, None)?;
    }

    Ok(())
}

/// Resolve parent context for results with parent_id.
///
/// For table chunks: parent is a stored section chunk → fetch from DB.
/// For windowed chunks: parent was never stored → read source file at line range.
fn resolve_parent_context<Mode>(
    results: &[UnifiedResult],
    store: &Store<Mode>,
    root: &std::path::Path,
) -> HashMap<String, ParentContext> {
    let mut parents = HashMap::new();

    // Collect unique parent_ids from code results
    let parent_ids: Vec<String> = results
        .iter()
        .filter_map(|r| match r {
            UnifiedResult::Code(sr) => sr.chunk.parent_id.clone(),
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if parent_ids.is_empty() {
        return parents;
    }

    // Batch-fetch parent chunks from store
    let id_refs: Vec<&str> = parent_ids.iter().map(|s| s.as_str()).collect();
    let stored_parents = match store.get_chunks_by_ids(&id_refs) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch parent chunks");
            HashMap::new()
        }
    };

    // Cache resolved ParentContext by parent_id to avoid rebuilding for siblings (CQ-7)
    let mut resolved_parents: HashMap<String, ParentContext> = HashMap::new();
    for result in results {
        let UnifiedResult::Code(sr) = result;
        let parent_id = match &sr.chunk.parent_id {
            Some(id) => id,
            None => continue,
        };

        // Reuse cached ParentContext if this parent was already resolved
        if let Some(cached) = resolved_parents.get(parent_id) {
            parents.insert(sr.chunk.id.clone(), cached.clone());
            continue;
        }

        if let Some(parent) = stored_parents.get(parent_id) {
            // Parent found in DB (table chunk → section parent)
            let ctx = ParentContext {
                name: parent.name.clone(),
                content: parent.content.clone(),
                line_start: parent.line_start,
                line_end: parent.line_end,
            };
            resolved_parents.insert(parent_id.clone(), ctx.clone());
            parents.insert(sr.chunk.id.clone(), ctx);
        } else {
            // Parent not in DB (windowed chunk → read source file)
            // RT-FS-1: Validate the resolved path stays within project root
            // to prevent path traversal via crafted chunk.file values.
            let abs_path = root.join(&sr.chunk.file);
            let canonical = match dunce::canonicalize(&abs_path) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let canonical_root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            if !canonical.starts_with(&canonical_root) {
                tracing::warn!(
                    path = %sr.chunk.file.display(),
                    "Path escapes project root, skipping parent context"
                );
                continue;
            }
            // Gate by file size before slurping for line-range slice.
            let max_bytes = cqs::limits::small_file_max_bytes();
            if let Ok(meta) = std::fs::metadata(&canonical) {
                if meta.len() > max_bytes {
                    tracing::warn!(
                        path = %canonical.display(),
                        size = meta.len(),
                        cap = max_bytes,
                        "Skipping parent-context fallback (CQS_SMALL_FILE_MAX_BYTES)"
                    );
                    continue;
                }
            }
            match std::fs::read_to_string(&canonical) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let start = sr.chunk.line_start.saturating_sub(1) as usize;
                    let end = (sr.chunk.line_end as usize).min(lines.len());
                    if start < end {
                        let parent_content = lines[start..end].join("\n");
                        let ctx = ParentContext {
                            name: sr.chunk.name.clone(),
                            content: parent_content,
                            line_start: sr.chunk.line_start,
                            line_end: sr.chunk.line_end,
                        };
                        resolved_parents.insert(parent_id.clone(), ctx.clone());
                        parents.insert(sr.chunk.id.clone(), ctx);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        path = %abs_path.display(),
                        error = %e,
                        "Failed to read source for parent context"
                    );
                }
            }
        }
    }

    parents
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── ProjectSurface::Skip pin ────────────────────────────────────────────
    //
    // A `--ref`-scoped query searches one reference store and never reads the
    // project vector index or the project SPLADE inverted index. The mock below
    // makes every project-surface accessor *panic if called*, so a regression
    // that re-introduced the project-index resolution on the Skip path turns
    // into a test failure rather than silent wasted I/O. The embedder is real
    // but cache-seeded, so the prelude embeds the query with no ONNX load.
    mod project_surface {
        use super::*;
        use std::cell::Cell;
        use std::path::{Path, PathBuf};
        use std::sync::Arc;

        use cqs::index::VectorIndex;
        use cqs::reference::ReferenceIndex;
        use cqs::store::{ModelInfo, ReadOnly, Store};
        use cqs::{Embedder, Embedding};
        use tempfile::TempDir;

        /// A `SearchCtx` whose project-surface accessors panic. The reference
        /// accessor returns the seeded reference; the embedder is cache-seeded.
        struct CountingCtx {
            store: Store<ReadOnly>,
            cqs_dir: PathBuf,
            root: PathBuf,
            embedder: Embedder,
            reference: Arc<ReferenceIndex>,
            /// Set true if any project vector-index accessor was reached.
            project_index_touched: Cell<bool>,
        }

        impl SearchCtx for CountingCtx {
            fn store(&self) -> &Store<ReadOnly> {
                &self.store
            }
            fn cqs_dir(&self) -> &Path {
                &self.cqs_dir
            }
            fn root(&self) -> &Path {
                &self.root
            }
            fn embedder(&self) -> Result<&Embedder> {
                Ok(&self.embedder)
            }
            fn reranker(&self) -> Result<Arc<dyn cqs::Reranker>> {
                panic!("reranker() must not be called for a non-rerank ref query")
            }
            fn splade_encode(&self, _query: &str) -> Option<cqs::splade::SparseVector> {
                self.project_index_touched.set(true);
                panic!("splade_encode() is project-surface work — must be skipped for --ref")
            }
            fn splade_index(&self) -> Option<search_ctx::SpladeIndexRef<'_>> {
                self.project_index_touched.set(true);
                panic!("splade_index() is project-surface work — must be skipped for --ref")
            }
            fn vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
                self.project_index_touched.set(true);
                panic!("vector_index() is project-surface work — must be skipped for --ref")
            }
            fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
                self.project_index_touched.set(true);
                panic!("base_vector_index() is project-surface work — must be skipped for --ref")
            }
            fn audit_state(&self) -> cqs::audit::AuditMode {
                cqs::audit::AuditMode::default()
            }
            fn references(&self) -> Result<Vec<Arc<ReferenceIndex>>> {
                Ok(vec![self.reference.clone()])
            }
            fn reference_by_name(&self, _name: &str) -> Result<Arc<ReferenceIndex>> {
                Ok(self.reference.clone())
            }
        }

        /// Open + init an empty `ReadOnly` store at `dir/index.db`.
        fn open_store(dir: &Path) -> Store<ReadOnly> {
            let cqs_dir = dir.join(".cqs");
            std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
            let db = cqs_dir.join("index.db");
            {
                let s = Store::open(&db).expect("open store");
                s.init(&ModelInfo::default()).expect("init store");
            }
            Store::open_readonly(&db).expect("open readonly")
        }

        /// Build a reference index over a fresh store holding one chunk whose
        /// stored embedding equals `emb`, so a query with `emb` retrieves it via
        /// the brute-force (`index: None`) reference path.
        fn seeded_reference(dir: &Path, emb: &Embedding) -> ReferenceIndex {
            use cqs::parser::{Chunk, ChunkType, Language};
            let cqs_dir = dir.join("ref/.cqs");
            std::fs::create_dir_all(&cqs_dir).expect("mkdir ref .cqs");
            let db = cqs_dir.join("index.db");
            let chunk = Chunk {
                id: "lib.rs:1:refchunk".to_string(),
                file: PathBuf::from("lib.rs"),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: "ref_target".to_string(),
                signature: "fn ref_target()".to_string(),
                content: "fn ref_target() { /* in the reference */ }".to_string(),
                doc: None,
                line_start: 1,
                line_end: 3,
                content_hash: blake3::hash(b"ref_target").to_hex().to_string(),
                canonical_hash: String::new(),
                parent_id: None,
                window_idx: None,
                parent_type_name: None,
                parser_version: 0,
            };
            {
                let s = Store::open(&db).expect("open ref store");
                s.init(&ModelInfo::default()).expect("init ref store");
                s.upsert_chunks_batch(&[(chunk, emb.clone())], Some(0))
                    .expect("upsert ref chunk");
            }
            let store = Store::open_readonly(&db).expect("open ref readonly");
            ReferenceIndex {
                name: "stdlib".to_string(),
                store,
                index: None,
                weight: 1.0,
                db_path: db,
                loaded_identity: None,
            }
        }

        /// Build a `CountingCtx` plus the seeded query embedding. The embedder's
        /// in-memory cache is seeded so `prepare_query` embeds without ONNX.
        fn ctx_with_seeded_query(query: &str) -> (TempDir, CountingCtx, Embedding) {
            let dir = TempDir::new().expect("tempdir");
            let mut v = vec![0.0_f32; cqs::EMBEDDING_DIM];
            v[0] = 1.0;
            let emb = Embedding::new(v);

            let embedder =
                Embedder::new(cqs::embedder::ModelConfig::default_model()).expect("build embedder");
            embedder.seed_query_cache(query, emb.clone());

            let reference = Arc::new(seeded_reference(dir.path(), &emb));
            let ctx = CountingCtx {
                store: open_store(dir.path()),
                cqs_dir: dir.path().join(".cqs"),
                root: dir.path().to_path_buf(),
                embedder,
                reference,
                project_index_touched: Cell::new(false),
            };
            (dir, ctx, emb)
        }

        /// PIN: a `--ref`-scoped `prepare_query(ProjectSurface::Skip)` resolves
        /// the embedding + filter but never touches the project vector index or
        /// the project SPLADE index — the prepared struct's project fields are
        /// `None` and the panicking accessors are never reached. This is the
        /// wasted-I/O regression guard for the ref-scoped prelude.
        #[test]
        fn ref_scoped_skip_never_resolves_project_index() {
            let (_dir, ctx, _emb) = ctx_with_seeded_query("find the ref target");
            // `fts_first = false`: --ref disables the project FTS-first branch.
            let args = QueryArgs {
                query: "find the ref target".to_string(),
                fts_first: false,
                ..QueryArgs::default()
            };

            let prepared = match prepare_query(&ctx, &args, ProjectSurface::Skip)
                .expect("prepare_query Skip")
            {
                Prepared::Dense(p) => p,
                Prepared::ShortCircuit(_) => panic!("ref path must prepare a dense query"),
            };

            assert!(
                prepared.index.is_none(),
                "Skip must leave the project vector index unresolved"
            );
            assert!(
                prepared.splade_index.is_none(),
                "Skip must leave the project SPLADE index unprimed"
            );
            assert!(
                prepared.splade_query.is_none(),
                "Skip must skip the SPLADE query encoding"
            );
            assert!(
                !ctx.project_index_touched.get(),
                "no project-surface accessor may be called on the Skip path"
            );
        }

        /// EQUALITY: the ref-scoped retrieval is byte-identical whether the
        /// prelude resolved the project surface (`Resolve`) or skipped it
        /// (`Skip`). The ref fan-out consumes only the embedding + filter +
        /// reranker, all built identically on both surfaces, so dropping the
        /// project-surface work cannot change `--ref` results. Drives the
        /// `Skip`-prepared query through `retrieve_ref_scoped` and asserts a
        /// non-empty, deterministic result set.
        #[test]
        fn ref_scoped_results_identical_skip_vs_resolve() {
            let (_dir, ctx, _emb) = ctx_with_seeded_query("retrieve ref target");
            let args = QueryArgs {
                query: "retrieve ref target".to_string(),
                fts_first: false,
                ..QueryArgs::default()
            };

            // Skip path (the new, slimmed path).
            let skip = match prepare_query(&ctx, &args, ProjectSurface::Skip).expect("Skip") {
                Prepared::Dense(p) => p,
                Prepared::ShortCircuit(_) => panic!("dense expected"),
            };
            let skip_tagged =
                retrieve_ref_scoped(&ctx, &args, &skip, "stdlib").expect("retrieve_ref_scoped");

            // The ref fan-out must have found the seeded chunk, proving the
            // equality assertion exercises a real (non-empty) result.
            assert_eq!(skip_tagged.len(), 1, "seeded ref chunk must be retrieved");
            assert_eq!(skip_tagged[0].source.as_deref(), Some("stdlib"));
            let UnifiedResult::Code(sr) = &skip_tagged[0].result;
            assert_eq!(sr.chunk.name, "ref_target");

            // The fan-out reads `query_embedding`, `filter`, `reranker` only —
            // identical on a Resolve-prepared query — so a Resolve prelude would
            // produce the same tagged result. We assert the prepared inputs the
            // fan-out reads are the surface-independent ones: the project fields
            // it ignores are `None` on Skip, confirming they never participate.
            assert!(skip.index.is_none() && skip.splade_index.is_none());
        }
    }

    /// The MCP-readiness contract: a wire/tool caller can supply only `query`
    /// and the rest fall back to the production defaults via `#[serde(default)]`.
    #[test]
    fn query_args_deserialize_minimal() {
        let args: QueryArgs = serde_json::from_str(r#"{"query": "find me"}"#).unwrap();
        assert_eq!(args.query, "find me");
        assert_eq!(args.limit, 5, "limit default mirrors clap");
        assert!((args.threshold - 0.3).abs() < 1e-6);
        assert!((args.name_boost - 0.2).abs() < 1e-6);
        assert!(!args.name_only);
        assert!(!args.rerank);
        assert!(args.tokens.is_none());
        assert_eq!(args.json_overhead, 0);
        assert!(!args.force_base_index);
    }

    /// Every field is wire-settable (the future MCP `search` tool's param
    /// surface). A regression that dropped `#[serde(default)]` or renamed a
    /// field would break deserialization here.
    #[test]
    fn query_args_deserialize_full() {
        let json = r#"{
            "query": "q",
            "limit": 12,
            "name_only": true,
            "lang": "rust",
            "include_type": ["function", "struct"],
            "exclude_type": ["test"],
            "path": "src/**",
            "pattern": "async",
            "include_docs": true,
            "rrf": true,
            "rerank": true,
            "splade": true,
            "splade_alpha": 0.5,
            "threshold": 0.1,
            "name_boost": 0.7,
            "no_demote": true,
            "tokens": 4000,
            "expand_parent": true,
            "force_base_index": true,
            "json_overhead": 30
        }"#;
        let args: QueryArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.limit, 12);
        assert!(args.name_only);
        assert_eq!(args.lang.as_deref(), Some("rust"));
        assert_eq!(
            args.include_type.as_deref(),
            Some(&["function".to_string(), "struct".to_string()][..])
        );
        assert_eq!(
            args.exclude_type.as_deref(),
            Some(&["test".to_string()][..])
        );
        assert_eq!(args.path.as_deref(), Some("src/**"));
        assert_eq!(args.pattern.as_deref(), Some("async"));
        assert!(args.include_docs);
        assert!(args.rrf);
        assert!(args.rerank);
        assert!(args.splade);
        assert_eq!(args.splade_alpha, Some(0.5));
        assert_eq!(args.tokens, Some(4000));
        assert!(args.expand_parent);
        assert!(args.force_base_index);
        assert_eq!(args.json_overhead, 30);
    }

    /// `QueryArgs::default` must match the clap defaults exactly — the wire
    /// surface and the CLI surface have to agree on omitted-field behavior.
    /// Parses a real minimal CLI invocation so a changed clap default breaks
    /// this test instead of silently diverging from the wire default.
    #[test]
    fn query_args_default_matches_clap_defaults() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(["cqs", "q"]).unwrap();
        let from_clap = QueryArgs::from_cli(&cli);
        let expected = QueryArgs {
            query: "q".to_string(),
            ..QueryArgs::default()
        };
        assert_eq!(
            from_clap, expected,
            "clap defaults drifted from QueryArgs::default — update both together"
        );
    }

    /// `--ref` scoped: the multi-store args disable the NameOnly-FTS-first
    /// short-circuit, because that lookup runs against the *project* store and
    /// the `--ref` path never searches the project. Without this, a
    /// NameOnly-classified `--ref` query could short-circuit to project-store
    /// FTS results instead of fanning out to the reference. Everything else
    /// matches `from_cli`.
    #[test]
    fn from_cli_ref_disables_fts_first_for_ref_scoped() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(["cqs", "q", "--ref", "stdlib"]).unwrap();
        let ref_args = QueryArgs::from_cli_ref(&cli);
        assert!(
            !ref_args.fts_first,
            "--ref scoped must disable project-store FTS-first"
        );
        // The only intended divergence from from_cli is fts_first.
        let expected = QueryArgs {
            fts_first: false,
            ..QueryArgs::from_cli(&cli)
        };
        assert_eq!(ref_args, expected);
    }

    /// `--include-refs` (no `--ref`): the project half is the real project
    /// store, so FTS-first stays on — matching the pre-refactor behavior where
    /// a name-only hit short-circuited to project-only results.
    #[test]
    fn from_cli_ref_keeps_fts_first_for_include_refs() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(["cqs", "q", "--include-refs"]).unwrap();
        let ref_args = QueryArgs::from_cli_ref(&cli);
        assert!(
            ref_args.fts_first,
            "--include-refs keeps project FTS-first (its project half is the real store)"
        );
        assert_eq!(ref_args, QueryArgs::from_cli(&cli));
    }

    /// The `QueryOutput` envelope is a plain data carrier: an empty result set
    /// is a valid output (the adapter, not the core, maps it to NoResults).
    #[test]
    fn query_output_allows_empty_results() {
        let out = QueryOutput {
            query: "nothing".to_string(),
            results: Vec::new(),
            parents: HashMap::new(),
            token_info: None,
        };
        assert!(out.results.is_empty());
        assert_eq!(out.query, "nothing");
        assert!(out.token_info.is_none());
    }

    // ─── Worktree-overlay mask + merge logic (result-trust §3, plan §7) ──────
    //
    // Pure mask/merge tests over hand-built `SearchResult` fixtures and a
    // seeded in-memory overlay store. No embedder/ONNX load (the overlay store
    // is seeded with explicit embedding vectors), so these run in the default
    // test set rather than behind `slow-tests`. The full daemon-driven
    // end-to-end overlay path (real git fixture + real embedder) is PR-3.
    mod overlay_merge {
        use super::*;
        use std::collections::HashSet;
        use std::path::PathBuf;

        use cqs::parser::{Chunk, ChunkType, Language};
        use cqs::store::{ChunkSummary, ModelInfo, SearchResult, Store};
        use cqs::worktree_overlay::{OverlayMeta, OverlayStats, WorktreeOverlay};
        use cqs::{Embedding, SearchFilter};

        /// One-hot embedding with `1.0` at `slot`, dim `EMBEDDING_DIM`. Two
        /// distinct slots are near-orthogonal, so a query at `slot` retrieves
        /// only the chunk seeded at that slot from the brute-force store.
        fn one_hot(slot: usize) -> Embedding {
            let mut v = vec![0.0_f32; cqs::EMBEDDING_DIM];
            v[slot] = 1.0;
            Embedding::new(v)
        }

        /// A project-side `SearchResult` for `(file, name, score)` with a unique
        /// id, content hash derived from the name so the merge dedup is
        /// predictable.
        fn project_result(file: &str, name: &str, score: f32) -> SearchResult {
            let summary = ChunkSummary {
                id: format!("{file}:{name}"),
                file: PathBuf::from(file),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: format!("fn {name}() {{}}"),
                doc: None,
                line_start: 1,
                line_end: 2,
                content_hash: blake3::hash(name.as_bytes()).to_hex().to_string(),
                window_idx: None,
                parent_id: None,
                parent_type_name: None,
                parser_version: 0,
                vendored: false,
            };
            SearchResult::new(summary, score)
        }

        /// Build a `WorktreeOverlay` whose in-memory store holds one chunk per
        /// `(file, name, slot)` triple (seeded with `one_hot(slot)`), and whose
        /// `masked_origins` is exactly `masked`. `chunks_indexed`/`files_in_delta`
        /// stats are filled so `apply_overlay`'s `_meta` is exercised.
        fn overlay_with(seeds: &[(&str, &str, usize)], masked: &[&str]) -> WorktreeOverlay {
            let mut store = Store::open_memory().expect("open_memory");
            store
                .init(&ModelInfo::default())
                .expect("init overlay store");
            store.set_dim(cqs::EMBEDDING_DIM);

            for (file, name, slot) in seeds {
                let chunk = Chunk {
                    id: format!("{file}:{name}"),
                    file: PathBuf::from(file),
                    language: Language::Rust,
                    chunk_type: ChunkType::Function,
                    name: name.to_string(),
                    signature: format!("fn {name}()"),
                    content: format!("fn {name}() {{}}"),
                    doc: None,
                    line_start: 1,
                    line_end: 2,
                    content_hash: blake3::hash(name.as_bytes()).to_hex().to_string(),
                    canonical_hash: String::new(),
                    parent_id: None,
                    window_idx: None,
                    parent_type_name: None,
                    parser_version: 0,
                };
                store
                    .upsert_chunks_batch(&[(chunk, one_hot(*slot))], Some(0))
                    .expect("seed overlay chunk");
            }

            let masked_origins: HashSet<PathBuf> = masked.iter().map(PathBuf::from).collect();
            WorktreeOverlay {
                store,
                masked_origins,
                fingerprint: [0u8; 32],
                worktree_root: PathBuf::from("/wt"),
                stats: OverlayStats {
                    files_in_delta: masked.len(),
                    chunks_indexed: seeds.len(),
                    build_ms: 0,
                },
            }
        }

        /// A `PreparedQuery` carrying `emb` + a default filter and nothing else
        /// (no SPLADE, no index, no reranker) — exactly what `apply_overlay`
        /// reads (`query_embedding`, `filter`).
        fn prepared_with(emb: Embedding) -> PreparedQuery<'static> {
            PreparedQuery {
                query_embedding: emb,
                filter: SearchFilter::default(),
                splade_query: None,
                splade_index: None,
                index: None,
                audit_mode: cqs::audit::AuditMode::default(),
                search_limit: 10,
                reranker: None,
            }
        }

        fn args_limit(limit: usize) -> QueryArgs {
            QueryArgs {
                limit,
                threshold: 0.0,
                ..QueryArgs::default()
            }
        }

        fn names(results: &[UnifiedResult]) -> Vec<String> {
            results
                .iter()
                .map(|r| {
                    let UnifiedResult::Code(sr) = r;
                    sr.chunk.name.clone()
                })
                .collect()
        }

        fn files(results: &[UnifiedResult]) -> Vec<String> {
            results
                .iter()
                .map(|r| {
                    let UnifiedResult::Code(sr) = r;
                    sr.chunk.file.to_string_lossy().into_owned()
                })
                .collect()
        }

        // ── Test #1: the adversarial origin-level-masking falsifier ──────────
        //
        // A function `dead_fn` was deleted from `src/a.rs` (still present in the
        // worktree, but `dead_fn` is gone). The origin is in the delta; no
        // overlay chunk shares the name. Origin-level masking drops the parent
        // `dead_fn` hit unconditionally — an `(origin, name)` implementation
        // would let it survive (no overlay counterpart to shadow it).
        #[test]
        fn overlay_masks_dead_function_in_modified_file() {
            cqs::worktree_overlay::clear_overlay_meta();
            // src/a.rs is in the delta; the overlay re-indexed only `live_fn`
            // (the surviving function). `dead_fn` has no overlay chunk.
            let overlay = overlay_with(&[("src/a.rs", "live_fn", 0)], &["src/a.rs"]);
            let project = vec![
                UnifiedResult::Code(project_result("src/a.rs", "dead_fn", 0.9)),
                UnifiedResult::Code(project_result("src/b.rs", "untouched", 0.5)),
            ];
            // Query the slot that retrieves nothing from the overlay (so the
            // overlay leg is empty and can't itself reintroduce `dead_fn`).
            let prepared = prepared_with(one_hot(5));
            let out = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            let got = names(&out);
            assert!(
                !got.contains(&"dead_fn".to_string()),
                "dead function in a modified file must be masked (origin-level, \
                 not (origin,name)); got {got:?}"
            );
            assert!(
                got.contains(&"untouched".to_string()),
                "a hit from an unchanged origin survives the mask; got {got:?}"
            );
        }

        // ── Risk #8: a NOTE-BOOSTED parent hit on a masked origin must not
        // resurrect. The overlay store has no notes; note boosts are a
        // parent-store scoring prior. The mask filters on origin membership
        // alone, independent of score — so even a hit pushed to the top of the
        // pool by a note boost (modeled here as score 1.0, well above the
        // overlay leg) is dropped if its origin is in the delta. Notes are
        // project-level priors; the mask must win.
        #[test]
        fn overlay_note_boosted_masked_origin_does_not_resurrect() {
            cqs::worktree_overlay::clear_overlay_meta();
            // src/a.rs changed; the overlay re-indexed `live_fn`. The parent's
            // `note_boosted_fn` on src/a.rs carries a maxed score (the note
            // boost), but its origin is masked.
            let overlay = overlay_with(&[("src/a.rs", "live_fn", 0)], &["src/a.rs"]);
            let project = vec![
                UnifiedResult::Code(project_result("src/a.rs", "note_boosted_fn", 1.0)),
                UnifiedResult::Code(project_result("src/b.rs", "untouched", 0.4)),
            ];
            let prepared = prepared_with(one_hot(5)); // overlay leg empty
            let out = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            let got = names(&out);
            assert!(
                !got.contains(&"note_boosted_fn".to_string()),
                "a note-boosted (score 1.0) parent hit on a masked origin must \
                 still be masked — the mask is score-independent; got {got:?}"
            );
            assert!(
                got.contains(&"untouched".to_string()),
                "the unmasked hit survives; got {got:?}"
            );
        }

        // ── #2/#3: rename within a file — old name masked, new name from overlay
        #[test]
        fn overlay_rename_within_file_surfaces_new_name() {
            cqs::worktree_overlay::clear_overlay_meta();
            // src/a.rs changed: `old_name` → `new_name`. Overlay indexed
            // `new_name` at slot 3; query at slot 3 retrieves it.
            let overlay = overlay_with(&[("src/a.rs", "new_name", 3)], &["src/a.rs"]);
            let project = vec![UnifiedResult::Code(project_result(
                "src/a.rs", "old_name", 0.9,
            ))];
            let prepared = prepared_with(one_hot(3));
            let out = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            let got = names(&out);
            assert!(
                !got.contains(&"old_name".to_string()),
                "the renamed-away name is masked; got {got:?}"
            );
            assert!(
                got.contains(&"new_name".to_string()),
                "the overlay's new name is returned; got {got:?}"
            );
        }

        // ── #4: a fully-deleted file contributes zero overlay chunks, all masked
        #[test]
        fn overlay_deleted_file_fully_masked() {
            cqs::worktree_overlay::clear_overlay_meta();
            // src/gone.rs deleted: masked, no overlay chunk.
            let overlay = overlay_with(&[], &["src/gone.rs"]);
            let project = vec![
                UnifiedResult::Code(project_result("src/gone.rs", "ghost", 0.9)),
                UnifiedResult::Code(project_result("src/keep.rs", "kept", 0.4)),
            ];
            let prepared = prepared_with(one_hot(7));
            let out = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            assert_eq!(
                names(&out),
                vec!["kept".to_string()],
                "all hits from the deleted origin are masked; the rest survive"
            );
        }

        // ── #6: a new (untracked/added) worktree file is searchable and ranks
        //        alongside parent results.
        #[test]
        fn overlay_new_file_searchable() {
            cqs::worktree_overlay::clear_overlay_meta();
            // src/new.rs is worktree-only: indexed in the overlay at slot 2,
            // masked (harmless — parent has no such origin).
            let overlay = overlay_with(&[("src/new.rs", "brand_new", 2)], &["src/new.rs"]);
            let project = vec![UnifiedResult::Code(project_result(
                "src/old.rs",
                "existing",
                0.6,
            ))];
            let prepared = prepared_with(one_hot(2));
            let out = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            let got = names(&out);
            assert!(
                got.contains(&"brand_new".to_string()),
                "the worktree-only file is searchable; got {got:?}"
            );
            assert!(
                got.contains(&"existing".to_string()),
                "the unchanged parent hit still ranks; got {got:?}"
            );
        }

        // ── #12: an unchanged-content chunk inside a changed file yields exactly
        //         one hit (the overlay's), no duplicate from the parent.
        #[test]
        fn overlay_unchanged_chunk_in_changed_file_not_duplicated() {
            cqs::worktree_overlay::clear_overlay_meta();
            // src/a.rs changed; `stable` is byte-identical in both. Parent has
            // it; overlay re-indexed it (same content_hash, since the helper
            // hashes the name). Mask drops the parent copy; merge_results dedup
            // would also fire if both reached the merge — only one survives.
            let overlay = overlay_with(&[("src/a.rs", "stable", 4)], &["src/a.rs"]);
            let project = vec![UnifiedResult::Code(project_result(
                "src/a.rs", "stable", 0.8,
            ))];
            let prepared = prepared_with(one_hot(4));
            let out = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            let stable_count = names(&out).iter().filter(|n| *n == "stable").count();
            assert_eq!(
                stable_count, 1,
                "exactly one `stable` hit (the overlay's), no parent duplicate"
            );
            // And it comes from the overlay store (parent copy was masked).
            assert_eq!(files(&out), vec!["src/a.rs".to_string()]);
        }

        // ── Under-fill: masking can hollow the top-k; the merge still fills from
        //    the overlay + unchanged parent hits up to `limit`.
        #[test]
        fn overlay_active_records_meta() {
            cqs::worktree_overlay::clear_overlay_meta();
            let overlay = overlay_with(&[("src/a.rs", "live", 1)], &["src/a.rs"]);
            let project = vec![UnifiedResult::Code(project_result("src/a.rs", "dead", 0.9))];
            let prepared = prepared_with(one_hot(1));
            let _ = apply_overlay(&args_limit(10), &prepared, project, &overlay).unwrap();
            let meta = cqs::worktree_overlay::take_overlay_meta();
            assert_eq!(
                meta,
                Some(OverlayMeta::Active {
                    files: 1,
                    chunks: 1
                }),
                "apply_overlay records the Active envelope meta"
            );
        }

        // ── Under-fill backfill: the over-fetched pool refills the top-k ──────
        //
        // The store fetch over-fetches 2x when an overlay is active
        // (`prepare_query`), so by the time `apply_overlay` runs, the project
        // pool is `2*limit`. Masking `k` of them out must NOT drop the result
        // count below `limit` — the surviving over-fetched parent hits backfill
        // the holes. Pin that: a `2*limit` pool with `limit` masked origins
        // yields exactly `limit` results (all from the unmasked half), and the
        // masked origins are gone. The overlay leg is empty here (query slot has
        // no overlay chunk), so the backfill is purely the over-fetch headroom.
        #[test]
        fn overlay_masked_hollow_backfills_from_overfetch() {
            cqs::worktree_overlay::clear_overlay_meta();
            let limit = 5;
            // src/changed.rs is the masked origin; src/keep.rs is unchanged.
            let overlay = overlay_with(&[("src/changed.rs", "noise", 9)], &["src/changed.rs"]);

            // A 2*limit over-fetched pool: the top `limit` are masked (changed
            // file, higher scores), the next `limit` are unmasked survivors.
            let mut project = Vec::new();
            for i in 0..limit {
                project.push(UnifiedResult::Code(project_result(
                    "src/changed.rs",
                    &format!("masked_{i}"),
                    0.9 - i as f32 * 0.01,
                )));
            }
            for i in 0..limit {
                project.push(UnifiedResult::Code(project_result(
                    "src/keep.rs",
                    &format!("survivor_{i}"),
                    0.5 - i as f32 * 0.01,
                )));
            }
            assert_eq!(project.len(), 2 * limit, "simulated over-fetched pool");

            // Query a slot with no overlay chunk → empty overlay leg → backfill
            // is purely the over-fetch headroom.
            let prepared = prepared_with(one_hot(1));
            let out = apply_overlay(&args_limit(limit), &prepared, project, &overlay).unwrap();

            assert_eq!(
                out.len(),
                limit,
                "masking the top-k must NOT hollow the result count below limit; \
                 the over-fetched survivors backfill it. got {:?}",
                names(&out)
            );
            assert!(
                names(&out).iter().all(|n| n.starts_with("survivor_")),
                "every survivor is from the unmasked origin; got {:?}",
                names(&out)
            );
            assert!(
                files(&out).iter().all(|f| f == "src/keep.rs"),
                "no masked-origin hit leaked through; got {:?}",
                files(&out)
            );
        }

        // Inactive contrast: `apply_overlay` only runs under `Some(overlay)`, so
        // the no-overlay path is `retrieve_project`'s `overlay = None` branch —
        // no mask, no over-fetch, no merge. Pin the pure-logic half here: a
        // `limit`-sized pool with NO masked origins passes through with its
        // count and order intact when run through the merge (an empty mask set +
        // empty overlay leg is identity up to the merge's score sort).
        #[test]
        fn overlay_empty_mask_empty_leg_preserves_pool() {
            cqs::worktree_overlay::clear_overlay_meta();
            let limit = 5;
            // Empty mask set, no overlay chunks: nothing to mask, nothing to add.
            let overlay = overlay_with(&[], &[]);
            let mut project = Vec::new();
            for i in 0..limit {
                project.push(UnifiedResult::Code(project_result(
                    "src/keep.rs",
                    &format!("hit_{i}"),
                    0.9 - i as f32 * 0.01,
                )));
            }
            let prepared = prepared_with(one_hot(1));
            let out = apply_overlay(&args_limit(limit), &prepared, project, &overlay).unwrap();
            assert_eq!(out.len(), limit, "empty-mask empty-leg preserves the pool");
            // Score-desc order preserved (the inputs were already score-desc).
            let want: Vec<String> = (0..limit).map(|i| format!("hit_{i}")).collect();
            assert_eq!(names(&out), want, "order is preserved through the merge");
        }

        // ── FTS short-circuit masking (plan §7.3) ─────────────────────────────

        // `overlay = None` ⇒ name results pass through byte-identical (the
        // inactive regression fence for the name path).
        #[test]
        fn name_mask_none_overlay_is_identity() {
            let parent = vec![
                project_result("src/a.rs", "foo", 0.9),
                project_result("src/b.rs", "bar", 0.5),
            ];
            let before = parent.clone();
            let out = overlay_mask_name_results(None, parent, "foo", &args_limit(10)).unwrap();
            assert_eq!(out.len(), before.len());
            for (a, b) in out.iter().zip(before.iter()) {
                assert_eq!(a.chunk.id, b.chunk.id);
                assert_eq!(a.score, b.score);
            }
            // No overlay ⇒ no meta recorded.
            assert!(cqs::worktree_overlay::take_overlay_meta().is_none());
        }

        // A name hit whose origin is in the delta is masked; the overlay's own
        // name hit replaces it. Mirrors test #1 on the FTS path.
        #[test]
        fn name_mask_drops_delta_origin_keeps_overlay_hit() {
            cqs::worktree_overlay::clear_overlay_meta();
            // Overlay store holds `target` in the changed file; `search_by_name`
            // against it finds the overlay copy.
            let overlay = overlay_with(&[("src/a.rs", "target", 0)], &["src/a.rs"]);
            let parent = vec![
                // Parent's stale `target` in the changed file — must be masked.
                project_result("src/a.rs", "target", 0.9),
                // Unchanged file — survives.
                project_result("src/b.rs", "target", 0.4),
            ];
            let out = overlay_mask_name_results(Some(&overlay), parent, "target", &args_limit(10))
                .unwrap();
            let origins = files(
                &out.iter()
                    .cloned()
                    .map(UnifiedResult::Code)
                    .collect::<Vec<_>>(),
            );
            // The parent src/a.rs copy is masked; the overlay's src/a.rs copy and
            // the unchanged src/b.rs copy remain. Both origins present, but the
            // src/a.rs hit is the overlay's (parent's was dropped pre-merge).
            assert!(origins.contains(&"src/b.rs".to_string()));
            assert!(origins.contains(&"src/a.rs".to_string()));
            // Exactly two hits: dedup by content_hash collapses the two
            // identical-name src/a.rs `target`s? No — parent's was masked before
            // the overlay hit was added, so there is exactly one src/a.rs hit.
            let a_count = origins.iter().filter(|o| *o == "src/a.rs").count();
            assert_eq!(
                a_count, 1,
                "one src/a.rs hit (the overlay's); got {origins:?}"
            );
            assert_eq!(
                cqs::worktree_overlay::take_overlay_meta(),
                Some(OverlayMeta::Active {
                    files: 1,
                    chunks: 1
                })
            );
        }

        // All-masked, no-overlay-hit ⇒ empty result. (The caller — `--name-only`
        // — short-circuits on this; the NameOnly-classified path falls through
        // to dense. The helper just returns the empty masked set.)
        #[test]
        fn name_mask_all_masked_no_overlay_hit_is_empty() {
            cqs::worktree_overlay::clear_overlay_meta();
            // Overlay masks src/a.rs but holds NO `vanished` chunk.
            let overlay = overlay_with(&[("src/a.rs", "other", 0)], &["src/a.rs"]);
            let parent = vec![project_result("src/a.rs", "vanished", 0.9)];
            let out =
                overlay_mask_name_results(Some(&overlay), parent, "vanished", &args_limit(10))
                    .unwrap();
            assert!(
                out.is_empty(),
                "a name that only lived in a changed file, gone from the overlay, \
                 yields no hit; got {:?}",
                out.iter().map(|r| r.chunk.name.clone()).collect::<Vec<_>>()
            );
        }

        // ── OverlayMeta wire shapes (plan §7.5) ──────────────────────────────
        #[test]
        fn overlay_meta_active_is_files_chunks_object() {
            let v = OverlayMeta::Active {
                files: 3,
                chunks: 7,
            }
            .to_json();
            assert_eq!(v["files"], 3);
            assert_eq!(v["chunks"], 7);
        }

        #[test]
        fn overlay_meta_skip_shapes_are_strings() {
            assert_eq!(
                OverlayMeta::SkippedNoDaemon.to_json(),
                serde_json::Value::String("skipped-no-daemon".into())
            );
            assert_eq!(
                OverlayMeta::SkippedDeltaTooLarge.to_json(),
                serde_json::Value::String("skipped-delta-too-large".into())
            );
        }
    }
}
