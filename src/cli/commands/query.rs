//! Query command for cqs
//!
//! Executes semantic search queries.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use cqs::parser::ChunkType;
use cqs::store::{ParentContext, UnifiedResult};
use cqs::{reference, Embedder, Embedding, Pattern, SearchFilter, Store};

use crate::cli::{display, signal, staleness, Cli};

/// Compute JSON overhead for token budgeting based on output format.
fn json_overhead_for(cli: &Cli) -> usize {
    if cli.json {
        super::JSON_OVERHEAD_PER_RESULT
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
        println!("{}", obj);
    } else if let Some(ctx) = context {
        println!("No results found in reference '{}'.", ctx);
    } else {
        println!("No results found.");
    }
    std::process::exit(signal::ExitCode::NoResults as i32);
}

/// Execute a semantic search query and display results
pub(crate) fn cmd_query(cli: &Cli, query: &str) -> Result<()> {
    let _span = tracing::info_span!("cmd_query", query_len = query.len()).entered();

    let (store, root, cqs_dir) = crate::cli::open_project_store()?;

    // Name-only mode: search by function/struct name, skip embedding entirely
    if cli.name_only {
        if cli.rerank {
            bail!("--rerank requires embedding search, incompatible with --name-only");
        }
        if let Some(ref ref_name) = cli.ref_name {
            return cmd_query_ref_name_only(cli, ref_name, query, &root);
        }
        return cmd_query_name_only(cli, &store, query, &root);
    }

    // Over-retrieve when reranking to give the cross-encoder more candidates
    let effective_limit = if cli.rerank {
        (cli.limit * 4).min(100)
    } else {
        cli.limit
    };

    let embedder = Embedder::new()?;
    let query_embedding = embedder.embed_query(query)?;

    let languages = match &cli.lang {
        Some(l) => Some(vec![l.parse().context(format!(
            "Invalid language. Valid: {}",
            cqs::parser::Language::valid_names_display()
        ))?]),
        None => None,
    };

    let chunk_types = match &cli.chunk_type {
        Some(types) => {
            let parsed: Result<Vec<ChunkType>, _> = types.iter().map(|t| t.parse()).collect();
            Some(parsed.context(
                "Invalid chunk type. Valid: function, method, class, struct, enum, trait, interface, constant, section, property, delegate, event, module, macro, object, typealias",
            )?)
        }
        None => None,
    };

    #[allow(clippy::needless_update)]
    let filter = SearchFilter {
        languages,
        chunk_types,
        path_pattern: cli.path.clone(),
        name_boost: cli.name_boost,
        query_text: query.to_string(),
        enable_rrf: !cli.semantic_only, // RRF on by default, disable with --semantic-only
        note_weight: cli.note_weight,
        note_only: cli.note_only,
        enable_demotion: !cli.no_demote,
        ..Default::default()
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    // --ref scoped search: skip project index, search only the named reference
    if let Some(ref ref_name) = cli.ref_name {
        if cli.note_only {
            bail!("--note-only cannot be used with --ref (notes only exist in the project index)");
        }
        return cmd_query_ref_only(
            cli,
            ref_name,
            query,
            &query_embedding,
            &filter,
            &root,
            &embedder,
        );
    }

    cmd_query_project(
        cli,
        query,
        &query_embedding,
        &filter,
        &store,
        &cqs_dir,
        &root,
        &embedder,
        effective_limit,
    )
}

/// Project search: search project index (+ references if configured), post-process, display.
#[allow(clippy::too_many_arguments)]
fn cmd_query_project(
    cli: &Cli,
    query: &str,
    query_embedding: &Embedding,
    filter: &SearchFilter,
    store: &Store,
    cqs_dir: &std::path::Path,
    root: &std::path::Path,
    embedder: &Embedder,
    effective_limit: usize,
) -> Result<()> {
    let index = crate::cli::build_vector_index(store, cqs_dir)?;

    let audit_mode = cqs::audit::load_audit_state(cqs_dir);
    if audit_mode.is_active() && cli.note_only {
        bail!("--note-only is unavailable during audit mode");
    }

    let search_limit = if cli.pattern.is_some() {
        effective_limit * 3
    } else {
        effective_limit
    };
    let results = if audit_mode.is_active() {
        let code_results = store.search_filtered_with_index(
            query_embedding,
            filter,
            search_limit,
            cli.threshold,
            index.as_deref(),
        )?;
        code_results.into_iter().map(UnifiedResult::Code).collect()
    } else {
        store.search_unified_with_index(
            query_embedding,
            filter,
            search_limit,
            cli.threshold,
            index.as_deref(),
        )?
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
                UnifiedResult::Note(_) => false,
            })
            .collect();
        filtered.truncate(cli.limit);
        filtered
    } else {
        results
    };

    // Cross-encoder re-ranking
    let results = if cli.rerank {
        rerank_unified(query, results, cli.limit)?
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
    let parents = if cli.expand {
        resolve_parent_context(&results, store, root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand { Some(&parents) } else { None };

    // Staleness warning
    if !cli.quiet && !cli.no_stale_check {
        let origins: Vec<&str> = results
            .iter()
            .filter_map(|r| match r {
                UnifiedResult::Code(sr) => Some(sr.chunk.file.to_str().unwrap_or("")),
                UnifiedResult::Note(_) => None,
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !origins.is_empty() {
            staleness::warn_stale_results(store, &origins, root);
        }
    }

    // Load references for multi-index search
    let config = cqs::config::Config::load(root);
    let references = reference::load_references(&config.references);

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

    if cli.rerank {
        eprintln!(
            "Warning: --rerank is not supported with multi-index search. Skipping re-ranking."
        );
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

/// Token info for display: (used, budget)
type TokenInfo = Option<(usize, usize)>;

/// Pack results into a token budget, keeping highest-scoring results.
///
/// Generic over result type — works for both `UnifiedResult` and `TaggedResult`.
fn token_pack_results<T>(
    results: Vec<T>,
    budget: usize,
    json_overhead: usize,
    embedder: &Embedder,
    text_fn: impl Fn(&T) -> &str,
    score_fn: impl Fn(&T) -> f32,
    label: &str,
) -> (Vec<T>, TokenInfo) {
    let _span = tracing::info_span!("token_pack_results", budget, label).entered();

    let texts: Vec<&str> = results.iter().map(&text_fn).collect();
    let token_counts = super::count_tokens_batch(embedder, &texts);
    let (packed, used) = super::token_pack(results, &token_counts, budget, json_overhead, score_fn);
    tracing::info!(
        chunks = packed.len(),
        tokens = used,
        budget,
        label,
        "Token-budgeted query"
    );
    (packed, Some((used, budget)))
}

/// Extract text content from a `UnifiedResult`.
fn unified_text(r: &UnifiedResult) -> &str {
    match r {
        UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
        UnifiedResult::Note(nr) => nr.note.text.as_str(),
    }
}

/// Extract score from a `UnifiedResult`.
fn unified_score(r: &UnifiedResult) -> f32 {
    match r {
        UnifiedResult::Code(sr) => sr.score,
        UnifiedResult::Note(nr) => nr.score,
    }
}

/// Re-rank unified results using cross-encoder scoring
///
/// Extracts `Code` results, reranks them, preserves `Note` results at the end.
fn rerank_unified(
    query: &str,
    results: Vec<UnifiedResult>,
    limit: usize,
) -> Result<Vec<UnifiedResult>> {
    let mut code_results: Vec<cqs::store::SearchResult> = Vec::new();
    let mut note_results: Vec<UnifiedResult> = Vec::new();

    for r in results {
        match r {
            UnifiedResult::Code(sr) => code_results.push(sr),
            note @ UnifiedResult::Note(_) => note_results.push(note),
        }
    }

    if code_results.len() > 1 {
        let reranker =
            cqs::Reranker::new().map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
        reranker
            .rerank(query, &mut code_results, limit)
            .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
    }

    let mut out: Vec<UnifiedResult> = code_results.into_iter().map(UnifiedResult::Code).collect();
    out.extend(note_results);
    out.truncate(limit);
    Ok(out)
}

/// Name-only search: find by function/struct name, no embedding needed
fn cmd_query_name_only(
    cli: &Cli,
    store: &Store,
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
        let embedder = Embedder::new()?;
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
    let parents = if cli.expand {
        resolve_parent_context(&unified, store, root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand { Some(&parents) } else { None };

    if cli.json {
        display::display_unified_results_json(&unified, query, parents_ref, token_info)?;
    } else {
        display::display_unified_results(&unified, root, cli.no_content, cli.context, parents_ref)?;
    }

    Ok(())
}

/// Ref-scoped semantic search: search only the named reference, no project index
fn cmd_query_ref_only(
    cli: &Cli,
    ref_name: &str,
    query: &str,
    query_embedding: &Embedding,
    filter: &SearchFilter,
    root: &std::path::Path,
    embedder: &Embedder,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_query_ref_only", ref_name).entered();

    let ref_idx = super::resolve::find_reference(root, ref_name)?;

    let ref_limit = if cli.rerank {
        (cli.limit * 4).min(100)
    } else {
        cli.limit
    };
    let mut results = reference::search_reference(
        &ref_idx,
        query_embedding,
        filter,
        ref_limit,
        cli.threshold,
        false, // no weight for --ref scoped search
    )?;

    // Cross-encoder re-ranking for ref-only path
    if cli.rerank && results.len() > 1 {
        let reranker =
            cqs::Reranker::new().map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
        reranker
            .rerank(query, &mut results, cli.limit)
            .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
    }

    let tagged: Vec<reference::TaggedResult> = results
        .into_iter()
        .map(|r| reference::TaggedResult {
            result: UnifiedResult::Code(r),
            source: Some(ref_name.to_string()),
        })
        .collect();

    // Token-budget packing
    let json_overhead = json_overhead_for(cli);
    let (tagged, token_info) = if let Some(budget) = cli.tokens {
        token_pack_results(
            tagged,
            budget,
            json_overhead,
            embedder,
            |r| unified_text(&r.result),
            |r| unified_score(&r.result),
            "ref-only",
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

/// Ref-scoped name-only search: search only the named reference by name
fn cmd_query_ref_name_only(
    cli: &Cli,
    ref_name: &str,
    query: &str,
    root: &std::path::Path,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_query_ref_name_only", ref_name).entered();

    let ref_idx = super::resolve::find_reference(root, ref_name)?;

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
        let embedder = Embedder::new()?;
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
fn resolve_parent_context(
    results: &[UnifiedResult],
    store: &Store,
    root: &std::path::Path,
) -> HashMap<String, ParentContext> {
    let mut parents = HashMap::new();

    // Collect unique parent_ids from code results
    let parent_ids: Vec<String> = results
        .iter()
        .filter_map(|r| match r {
            UnifiedResult::Code(sr) => sr.chunk.parent_id.clone(),
            UnifiedResult::Note(_) => None,
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

    // For each result with parent_id, resolve the parent content
    for result in results {
        let sr = match result {
            UnifiedResult::Code(sr) => sr,
            UnifiedResult::Note(_) => continue,
        };
        let parent_id = match &sr.chunk.parent_id {
            Some(id) => id,
            None => continue,
        };

        // Skip if already resolved (multiple children share same parent)
        if parents.contains_key(&sr.chunk.id) {
            continue;
        }

        if let Some(parent) = stored_parents.get(parent_id) {
            // Parent found in DB (table chunk → section parent)
            parents.insert(
                sr.chunk.id.clone(),
                ParentContext {
                    name: parent.name.clone(),
                    content: parent.content.clone(),
                    line_start: parent.line_start,
                    line_end: parent.line_end,
                },
            );
        } else {
            // Parent not in DB (windowed chunk → read source file)
            let abs_path = root.join(&sr.chunk.file);
            match std::fs::read_to_string(&abs_path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let start = sr.chunk.line_start.saturating_sub(1) as usize;
                    let end = (sr.chunk.line_end as usize).min(lines.len());
                    if start < end {
                        let parent_content = lines[start..end].join("\n");
                        parents.insert(
                            sr.chunk.id.clone(),
                            ParentContext {
                                name: sr.chunk.name.clone(),
                                content: parent_content,
                                line_start: sr.chunk.line_start,
                                line_end: sr.chunk.line_end,
                            },
                        );
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
