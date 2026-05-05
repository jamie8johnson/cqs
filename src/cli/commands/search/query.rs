//! Query command for cqs
//!
//! Executes semantic search queries.
//!
//! TODO(json-schema): Extract typed QueryOutput struct. Depends on display module
//! refactoring — search results use display::display_unified_results_json which
//! builds JSON inline. Blocked until display module has typed output structs.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use cqs::parser::ChunkType;
use cqs::store::{ParentContext, UnifiedResult};
use cqs::{reference, Embedder, Embedding, Pattern, SearchFilter, Store};

use crate::cli::{display, signal, staleness, Cli};

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
    let cqs_dir = &ctx.cqs_dir;


    // Name-only mode: search by function/struct name, skip embedding entirely
    if cli.name_only {
        if cli.rerank_active() {
            bail!("--rerank requires embedding search, incompatible with --name-only");
        }
        if let Some(ref ref_name) = cli.ref_name {
            return cmd_query_ref_name_only(cli, ref_name, query, root);
        }
        return cmd_query_name_only(cli, store, query, root);
    }

    // Adaptive routing: classify query BEFORE embedding to potentially skip it
    // --splade intentionally NOT here: it only controls SPLADE fusion,
    // not adaptive routing. --rrf/--rerank/--ref override the search strategy.
    let has_explicit_flags = cli.rrf || cli.rerank_active() || cli.ref_name.is_some();
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

    // NameOnly strategy: try FTS5 first, fall back to dense on 0 results
    if let Some(ref c) = classification {
        if c.strategy == cqs::search::router::SearchStrategy::NameOnly {
            let results = store.search_by_name(query, cli.limit)?;
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
                return cmd_query_name_only(cli, store, query, root);
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
    // P3 #100: pool sizing centralized in `cli/limits.rs::rerank_pool_size`,
    // honors CQS_RERANK_OVER_RETRIEVAL / CQS_RERANK_POOL_MAX.
    let effective_limit = if cli.rerank_active() {
        crate::cli::limits::rerank_pool_size(cli.limit)
    } else {
        cli.limit
    };

    let embedder = ctx.embedder()?;
    let query_embedding = embedder.embed_query(query)?;

    // Centroid reclassification: if embedding-space centroid matching is
    // confident, upgrade the rule-based category. Track whether category
    // changed so we can apply the alpha floor downstream.
    let pre_centroid_cat = classification.as_ref().map(|c| c.category);
    let classification = classification
        .map(|c| cqs::search::router::reclassify_with_centroid(c, query_embedding.as_slice()));
    let centroid_applied = classification.as_ref().map(|c| c.category) != pre_centroid_cat;

    let languages = match &cli.lang {
        Some(l) => Some(vec![l.parse().context(format!(
            "Invalid language. Valid: {}",
            cqs::parser::Language::valid_names_display()
        ))?]),
        None => None,
    };

    let include_types = match &cli.include_type {
        Some(types) => {
            let parsed: Result<Vec<ChunkType>, _> = types.iter().map(|t| t.parse()).collect();
            Some(parsed.with_context(|| {
                format!(
                    "Invalid chunk type. Valid: {}",
                    ChunkType::valid_names().join(", ")
                )
            })?)
        }
        None if cli.include_docs => None, // --include-docs: search everything
        None => {
            // Default: search code only (callable types + type definitions).
            // Excludes Section (markdown), Module (file-level).
            Some(ChunkType::code_types())
        }
    };

    let exclude_types = match &cli.exclude_type {
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

    // Type boost from adaptive routing (boost, not filter — won't exclude results)
    let type_boost_types = classification.as_ref().and_then(|c| c.type_hints.clone());

    // Resolve SPLADE alpha:
    //   --splade-alpha X  : explicit constant α for all queries (sweeps, debug)
    //   otherwise         : per-category router when classification succeeds
    //   --splade          : force SPLADE on even for Unknown category
    //
    // IMPORTANT: SPLADE is always enabled when a category is classified, even at
    // α=1.0 — the α knob controls scoring weight (α=1.0 = pure dense) but SPLADE
    // still contributes to the *candidate pool*. Skipping SPLADE at α=1.0 loses
    // ~10pp R@1 on categories where sparse surfaces candidates dense misses
    // (multi_step, negation, cross_language).
    //
    // Semantics fix: prior to this, `--splade` bypassed the router and used
    // `cli.splade_alpha`'s clap default (0.7) for every query. That was a
    // regression introduced when routing landed — the flag predates routing.
    // Now the router runs whenever classification succeeds, regardless of
    // `--splade`. Explicit `--splade-alpha` still overrides.
    //
    // OB-NEW-1: `resolve_splade_alpha` emits the structured "SPLADE routing"
    // log internally — no call-site log needed here.
    let (use_splade, mut splade_alpha) = match (cli.splade_alpha, classification.as_ref()) {
        // Explicit α override wins in all cases.
        (Some(alpha), _) => (true, alpha),
        // Classified query → per-category router.
        (None, Some(c)) => (true, cqs::search::router::resolve_splade_alpha(&c.category)),
        // Unknown category + --splade → force on at legacy α=0.7.
        (None, None) if cli.splade => (true, 0.7),
        // Unknown category, no flags → SPLADE off (dense-only).
        (None, None) => (false, 1.0),
    };
    // Centroid alpha floor: when centroid reclassified the query, clamp α
    // so misclassifications can't catastrophically zero SPLADE (Behavioral α=0.0).
    if centroid_applied {
        splade_alpha = splade_alpha.max(0.7);
    }

    // #1349: SearchFilter is `#[non_exhaustive]`, so external-crate construction
    // goes through `Default` + field assignment instead of a struct expression.
    // Adding a new field stays one-line on the struct definition; this site
    // doesn't need to know.
    let filter = {
        let mut f = SearchFilter::default();
        f.languages = languages;
        f.include_types = include_types;
        f.exclude_types = exclude_types;
        f.path_pattern = cli.path.clone();
        f.name_boost = cli.name_boost;
        f.query_text = query.to_string();
        f.enable_rrf = cli.rrf;
        f.enable_demotion = !cli.no_demote;
        f.enable_splade = use_splade;
        f.splade_alpha = splade_alpha;
        f.type_boost_types = type_boost_types;
        f
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    // Lazily obtain reranker from CommandContext (shared across ref + project paths)
    let reranker = if cli.rerank_active() {
        Some(ctx.reranker()?)
    } else {
        None
    };

    // --ref scoped search: skip project index, search only the named reference
    if let Some(ref ref_name) = cli.ref_name {
        return cmd_query_ref_only(
            &RefQueryContext {
                cli,
                query,
                query_embedding: &query_embedding,
                filter: &filter,
                root,
                embedder,
                reranker: reranker.as_deref(),
            },
            ref_name,
        );
    }

    // SPLADE sparse encoding (if enabled by --splade flag OR per-category routing)
    let splade_query = if use_splade {
        ctx.splade_encoder().and_then(|enc| {
            match enc.encode(query) {
                Ok(sv) => Some(sv),
                Err(e) => {
                    tracing::warn!(error = %e, "SPLADE query encoding failed, falling back to cosine-only");
                    None
                }
            }
        })
    } else {
        None
    };
    let splade_index = if use_splade { ctx.splade_index() } else { None };

    cmd_query_project(&QueryContext {
        cli,
        query,
        query_embedding: &query_embedding,
        filter: &filter,
        store,
        cqs_dir,
        root,
        embedder,
        effective_limit,
        reranker: reranker.as_deref(),
        splade_query,
        splade_index,
        routed_strategy: classification.as_ref().map(|c| c.strategy),
    })
}

/// Infrastructure context for project queries.
struct QueryContext<'a> {
    cli: &'a Cli,
    query: &'a str,
    query_embedding: &'a Embedding,
    filter: &'a SearchFilter,
    store: &'a Store<cqs::store::ReadOnly>,
    cqs_dir: &'a std::path::Path,
    root: &'a std::path::Path,
    embedder: &'a Embedder,
    effective_limit: usize,
    reranker: Option<&'a dyn cqs::Reranker>,
    splade_query: Option<cqs::splade::SparseVector>,
    splade_index: Option<&'a cqs::splade::index::SpladeIndex>,
    /// Phase 5: strategy picked by the classifier (if adaptive routing ran).
    /// Drives whether we load the enriched or base HNSW.
    routed_strategy: Option<cqs::search::router::SearchStrategy>,
}

/// Project search: search project index, optionally include references (--include-refs).
fn cmd_query_project(ctx: &QueryContext<'_>) -> Result<()> {
    let cli = ctx.cli;
    let query = ctx.query;
    let query_embedding = ctx.query_embedding;
    let filter = ctx.filter;
    let store = ctx.store;
    let cqs_dir = ctx.cqs_dir;
    let root = ctx.root;
    let embedder = ctx.embedder;
    let effective_limit = ctx.effective_limit;

    // Phase 5: when the classifier picked DenseBase, try loading the
    // base (non-enriched) HNSW. If it's absent or corrupt, silently fall
    // back to the enriched index so the query still works.
    let use_base = matches!(
        ctx.routed_strategy,
        Some(cqs::search::router::SearchStrategy::DenseBase)
    ) || std::env::var("CQS_FORCE_BASE_INDEX").as_deref() == Ok("1");
    let mut base_fallback = false;
    let index = if use_base {
        match crate::cli::build_base_vector_index(store, cqs_dir)? {
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
                crate::cli::build_vector_index(store, cqs_dir)?
            }
        }
    } else {
        crate::cli::build_vector_index(store, cqs_dir)?
    };

    // Phase 5 telemetry: record DenseBase routing outcome (including fallback).
    // Other strategies are logged elsewhere; this fires only for DenseBase to
    // avoid double-counting.
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

    let audit_mode = cqs::audit::load_audit_state(cqs_dir);

    let search_limit = if cli.pattern.is_some() {
        effective_limit * 3
    } else {
        effective_limit
    };
    // Build SPLADE argument for search_hybrid
    let splade_arg = ctx
        .splade_query
        .as_ref()
        .and_then(|sq| ctx.splade_index.map(|si| (si, sq)));

    let results = if audit_mode.is_active() {
        let code_results = store.search_hybrid(
            query_embedding,
            filter,
            search_limit,
            cli.threshold,
            index.as_deref(),
            splade_arg,
        )?;
        code_results.into_iter().map(UnifiedResult::Code).collect()
    } else {
        // search_unified doesn't support SPLADE yet — use hybrid for code, unified for rest
        if splade_arg.is_some() {
            let code_results = store.search_hybrid(
                query_embedding,
                filter,
                search_limit,
                cli.threshold,
                index.as_deref(),
                splade_arg,
            )?;
            code_results.into_iter().map(UnifiedResult::Code).collect()
        } else {
            store.search_code_results(
                query_embedding,
                filter,
                search_limit,
                cli.threshold,
                index.as_deref(),
            )?
        }
    };

    // Pattern filter
    let pattern: Option<Pattern> = cli
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
        filtered.truncate(cli.limit);
        filtered
    } else {
        results
    };

    // Cross-encoder re-ranking
    let results = if let Some(reranker) = ctx.reranker {
        rerank_unified(reranker, query, results, cli.limit)?
    } else {
        results
    };

    // Token-budget packing
    let json_overhead = json_overhead_for(cli);
    let (results, token_info) = if let Some(budget) = cli.tokens {
        token_pack_results(
            results,
            budget,
            json_overhead,
            embedder,
            unified_text,
            unified_score,
            "query",
        )
    } else {
        (results, None)
    };

    // Parent context
    let parents = if cli.expand_parent {
        resolve_parent_context(&results, store, root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand_parent {
        Some(&parents)
    } else {
        None
    };

    // Staleness warning
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

    // Load references only when --include-refs is set (default: project only)
    let references = if cli.include_refs {
        let config = cqs::config::Config::load(root);
        reference::load_references(&config.references)
    } else {
        Vec::new()
    };

    if references.is_empty() {
        if results.is_empty() {
            emit_empty_results(query, cli.json, None);
        }
        if cli.json {
            display::display_unified_results_json(&results, query, parents_ref, token_info)?;
        } else {
            display::display_unified_results(
                &results,
                root,
                cli.no_content,
                cli.context,
                parents_ref,
            )?;
        }
        return Ok(());
    }

    if cli.rerank_active() {
        tracing::warn!("--rerank is not supported with multi-index search, skipping re-ranking");
    }

    // Multi-index search
    use rayon::prelude::*;
    let ref_results: Vec<_> = references
        .par_iter()
        .filter_map(|ref_idx| {
            match reference::search_reference(
                ref_idx,
                query_embedding,
                filter,
                cli.limit,
                cli.threshold,
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

    let tagged = reference::merge_results(results, ref_results, cli.limit);

    let (tagged, token_info) = if let Some(budget) = cli.tokens {
        token_pack_results(
            tagged,
            budget,
            json_overhead,
            embedder,
            |r| unified_text(&r.result),
            |r| unified_score(&r.result),
            "tagged",
        )
    } else {
        (tagged, token_info)
    };

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

/// Name-only search: find by function/struct name, no embedding needed
fn cmd_query_name_only<Mode>(
    cli: &Cli,
    store: &Store<Mode>,
    query: &str,
    root: &std::path::Path,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_query_name_only", query).entered();
    let results = store
        .search_by_name(query, cli.limit)
        .context("Failed to search by name")?;

    if results.is_empty() {
        emit_empty_results(query, cli.json, None);
    }

    // Convert to UnifiedResult for display
    let unified: Vec<UnifiedResult> = results.into_iter().map(UnifiedResult::Code).collect();

    // Token-budget packing (lazy embedder — only created when --tokens is set)
    let json_overhead = json_overhead_for(cli);
    let (unified, token_info) = if let Some(budget) = cli.tokens {
        let embedder = Embedder::new(cli.try_model_config()?.clone())?;
        token_pack_results(
            unified,
            budget,
            json_overhead,
            &embedder,
            unified_text,
            unified_score,
            "name-only",
        )
    } else {
        (unified, None)
    };

    // Resolve parent context if --expand requested
    let parents = if cli.expand_parent {
        resolve_parent_context(&unified, store, root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand_parent {
        Some(&parents)
    } else {
        None
    };

    if cli.json {
        display::display_unified_results_json(&unified, query, parents_ref, token_info)?;
    } else {
        display::display_unified_results(&unified, root, cli.no_content, cli.context, parents_ref)?;
    }

    Ok(())
}

/// Context for ref-scoped search queries.
struct RefQueryContext<'a> {
    cli: &'a Cli,
    query: &'a str,
    query_embedding: &'a Embedding,
    filter: &'a SearchFilter,
    root: &'a std::path::Path,
    embedder: &'a Embedder,
    reranker: Option<&'a dyn cqs::Reranker>,
}

/// Ref-scoped semantic search: search only the named reference, no project index
fn cmd_query_ref_only(ctx: &RefQueryContext<'_>, ref_name: &str) -> Result<()> {
    let _span = tracing::info_span!("cmd_query_ref_only", ref_name).entered();

    let ref_idx = crate::cli::commands::resolve::find_reference(ctx.root, ref_name)?;

    // P3 #100: shared pool sizing.
    let ref_limit = if ctx.cli.rerank_active() {
        crate::cli::limits::rerank_pool_size(ctx.cli.limit)
    } else {
        ctx.cli.limit
    };
    let mut results = reference::search_reference(
        &ref_idx,
        ctx.query_embedding,
        ctx.filter,
        ref_limit,
        ctx.cli.threshold,
        false, // no weight for --ref scoped search
    )?;

    // Cross-encoder re-ranking for ref-only path
    if let Some(reranker) = ctx.reranker {
        if results.len() > 1 {
            reranker
                .rerank(ctx.query, &mut results, ctx.cli.limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }
    }

    let tagged: Vec<reference::TaggedResult> = results
        .into_iter()
        .map(|r| reference::TaggedResult {
            result: UnifiedResult::Code(r),
            source: Some(ref_name.to_string()),
        })
        .collect();

    // Token-budget packing
    let json_overhead = json_overhead_for(ctx.cli);
    let (tagged, token_info) = if let Some(budget) = ctx.cli.tokens {
        token_pack_results(
            tagged,
            budget,
            json_overhead,
            ctx.embedder,
            |r| unified_text(&r.result),
            |r| unified_score(&r.result),
            "ref-only",
        )
    } else {
        (tagged, None)
    };

    if tagged.is_empty() {
        emit_empty_results(ctx.query, ctx.cli.json, Some(ref_name));
    }

    if ctx.cli.json {
        display::display_tagged_results_json(&tagged, ctx.query, None, token_info)?;
    } else {
        display::display_tagged_results(
            &tagged,
            ctx.root,
            ctx.cli.no_content,
            ctx.cli.context,
            None,
        )?;
    }

    Ok(())
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
            // RB-V1.36-2: gate by file size before slurping for line-range slice.
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
