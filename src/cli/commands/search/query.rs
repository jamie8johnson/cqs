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
        }
    }
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

    let prepared = match prepare_query(ctx, args)? {
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
pub(crate) fn prepare_query<'a>(
    ctx: &'a dyn search_ctx::SearchCtx,
    args: &QueryArgs,
) -> Result<Prepared<'a>> {
    let query = args.query.as_str();
    let store = ctx.store();
    let cqs_dir = ctx.cqs_dir();

    // Name-only path: FTS by name, skip embedding entirely.
    if args.name_only {
        let results = store
            .search_by_name(query, args.limit)
            .context("Failed to search by name")?;
        let unified: Vec<UnifiedResult> = results.into_iter().map(UnifiedResult::Code).collect();
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
            let results = store.search_by_name(query, args.limit)?;
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

    // Over-retrieve when reranking to give the cross-encoder more candidates.
    let effective_limit = if args.rerank {
        crate::cli::limits::rerank_pool_size(args.limit)
    } else {
        args.limit
    };

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
        f
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    let reranker = if args.rerank {
        Some(ctx.reranker()?)
    } else {
        None
    };

    // SPLADE sparse encoding (if enabled by --splade or per-category routing).
    // The encode two-step (+ daemon index priming) lives behind
    // `SearchCtx::splade_encode`, so the core just asks for the sparse vector.
    let splade_query = if use_splade {
        ctx.splade_encode(query)
    } else {
        None
    };
    let splade_index = if use_splade { ctx.splade_index() } else { None };

    // Phase 5: when the classifier picked DenseBase (or the env override is
    // resolved into args.force_base_index), try the base HNSW; fall back to
    // enriched if it's absent/corrupt.
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
        filtered.truncate(args.limit);
        filtered
    } else {
        results
    };

    // Cross-encoder re-ranking.
    let results = if let Some(reranker) = prepared.reranker.as_deref() {
        rerank_unified(reranker, query, results, args.limit)?
    } else {
        results
    };

    Ok(results)
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
    let prepared = match prepare_query(ctx, &args)? {
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
        tracing::warn!("--rerank is not supported with multi-index search, skipping re-ranking");
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
}
