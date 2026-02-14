//! Query command for cqs
//!
//! Executes semantic search queries.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use cqs::parser::ChunkType;
use cqs::store::{ParentContext, UnifiedResult};
use cqs::{reference, Embedder, Embedding, HnswIndex, Pattern, SearchFilter, Store};

use crate::cli::{display, signal, staleness, Cli};

/// Execute a semantic search query and display results
pub(crate) fn cmd_query(cli: &Cli, query: &str) -> Result<()> {
    let _span = tracing::info_span!("cmd_query", query_len = query.len()).entered();

    let (store, root, cqs_dir) = crate::cli::open_project_store()?;

    // Name-only mode: search by function/struct name, skip embedding entirely
    if cli.name_only {
        if let Some(ref ref_name) = cli.ref_name {
            return cmd_query_ref_name_only(cli, ref_name, query, &root);
        }
        return cmd_query_name_only(cli, &store, query, &root);
    }

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
                "Invalid chunk type. Valid: function, method, class, struct, enum, trait, interface, constant, section",
            )?)
        }
        None => None,
    };

    let filter = SearchFilter {
        languages,
        chunk_types,
        path_pattern: cli.path.clone(),
        name_boost: cli.name_boost,
        query_text: query.to_string(),
        enable_rrf: !cli.semantic_only, // RRF on by default, disable with --semantic-only
        note_weight: cli.note_weight,
        note_only: cli.note_only,
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

    // Load vector index for O(log n) search
    let index: Option<Box<dyn cqs::index::VectorIndex>> = {
        #[cfg(feature = "gpu-search")]
        {
            // Priority: CAGRA (GPU, large indexes) > HNSW (CPU) > brute-force
            //
            // CAGRA rebuilds index each CLI invocation (~1s for 474 vectors).
            // Only worth it when search time savings exceed rebuild cost.
            // Threshold: 5000 vectors (where CAGRA search is ~10x faster than HNSW)
            const CAGRA_THRESHOLD: u64 = 5000;
            let chunk_count = store.chunk_count().unwrap_or(0);
            if chunk_count >= CAGRA_THRESHOLD && cqs::cagra::CagraIndex::gpu_available() {
                match cqs::cagra::CagraIndex::build_from_store(&store) {
                    Ok(idx) => {
                        tracing::info!("Using CAGRA GPU index ({} vectors)", idx.len());
                        Some(Box::new(idx) as Box<dyn cqs::index::VectorIndex>)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to build CAGRA index, falling back to HNSW: {}", e);
                        HnswIndex::try_load(&cqs_dir)
                    }
                }
            } else {
                if chunk_count < CAGRA_THRESHOLD {
                    tracing::debug!(
                        "Index too small for CAGRA ({} < {}), using HNSW",
                        chunk_count,
                        CAGRA_THRESHOLD
                    );
                } else {
                    tracing::debug!("GPU not available, using HNSW");
                }
                HnswIndex::try_load(&cqs_dir)
            }
        }
        #[cfg(not(feature = "gpu-search"))]
        {
            HnswIndex::try_load(&cqs_dir)
        }
    };

    // Check audit mode for note exclusion
    let audit_mode = cqs::audit::load_audit_state(&cqs_dir);
    if audit_mode.is_active() && cli.note_only {
        bail!("--note-only is unavailable during audit mode");
    }

    // Use unified search, or code-only if audit mode active
    let results = if audit_mode.is_active() {
        // Audit mode: search code only, skip notes
        let code_results = store.search_filtered_with_index(
            &query_embedding,
            &filter,
            if cli.pattern.is_some() {
                cli.limit * 3
            } else {
                cli.limit
            },
            cli.threshold,
            index.as_deref(),
        )?;
        code_results.into_iter().map(UnifiedResult::Code).collect()
    } else {
        store.search_unified_with_index(
            &query_embedding,
            &filter,
            if cli.pattern.is_some() {
                cli.limit * 3
            } else {
                cli.limit
            },
            cli.threshold,
            index.as_deref(),
        )?
    };

    // Load references for multi-index search
    let config = cqs::config::Config::load(&root);
    let references = reference::load_references(&config.references);

    // Parse pattern filter if specified
    let pattern: Option<Pattern> = cli
        .pattern
        .as_ref()
        .map(|p| p.parse())
        .transpose()
        .context("Invalid pattern")?;

    // Apply structural pattern filter if specified
    let results = if let Some(ref pat) = pattern {
        let mut filtered: Vec<UnifiedResult> = results
            .into_iter()
            .filter(|r| match r {
                UnifiedResult::Code(sr) => {
                    pat.matches(&sr.chunk.content, &sr.chunk.name, Some(sr.chunk.language))
                }
                UnifiedResult::Note(_) => false, // Pattern filter only applies to code
            })
            .collect();
        filtered.truncate(cli.limit);
        filtered
    } else {
        results
    };

    // Token-budget packing for unified results (no-ref path)
    let json_overhead = if cli.json {
        super::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let (results, token_info) = if let Some(budget) = cli.tokens {
        token_pack_unified(results, budget, json_overhead, &embedder)
    } else {
        (results, None)
    };

    // Resolve parent context if --expand requested
    let parents = if cli.expand {
        resolve_parent_context(&results, &store, &root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand { Some(&parents) } else { None };

    // Proactive staleness warning (stderr, doesn't pollute JSON)
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
            staleness::warn_stale_results(&store, &origins, &root);
        }
    }

    // Fast path: no references configured
    if references.is_empty() {
        if results.is_empty() {
            if cli.json {
                println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
            } else {
                println!("No results found.");
            }
            std::process::exit(signal::ExitCode::NoResults as i32);
        }

        if cli.json {
            display::display_unified_results_json(&results, query, parents_ref, token_info)?;
        } else {
            display::display_unified_results(
                &results,
                &root,
                cli.no_content,
                cli.context,
                parents_ref,
            )?;
        }
        return Ok(());
    }

    // Multi-index search: search references in parallel
    use rayon::prelude::*;
    let ref_results: Vec<_> = references
        .par_iter()
        .filter_map(|ref_idx| {
            match reference::search_reference(
                ref_idx,
                &query_embedding,
                &filter,
                cli.limit,
                cli.threshold,
                true, // apply weight for multi-index merged search
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

    // Token-budget packing for tagged results (multi-ref path)
    let (tagged, token_info) = if let Some(budget) = cli.tokens {
        token_pack_tagged(tagged, budget, json_overhead, &embedder)
    } else {
        (tagged, token_info)
    };

    if tagged.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found.");
        }
        std::process::exit(signal::ExitCode::NoResults as i32);
    }

    if cli.json {
        display::display_tagged_results_json(&tagged, query, parents_ref, token_info)?;
    } else {
        display::display_tagged_results(&tagged, &root, cli.no_content, cli.context, parents_ref)?;
    }

    Ok(())
}

/// Token info for display: (used, budget)
type TokenInfo = Option<(usize, usize)>;

/// Pack unified results into a token budget, keeping highest-scoring results.
///
/// Results are already sorted by score from the search engine.
fn token_pack_unified(
    results: Vec<UnifiedResult>,
    budget: usize,
    json_overhead: usize,
    embedder: &Embedder,
) -> (Vec<UnifiedResult>, TokenInfo) {
    let _span = tracing::info_span!("token_pack_unified", budget).entered();

    let texts: Vec<&str> = results
        .iter()
        .map(|r| match r {
            UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
            UnifiedResult::Note(nr) => nr.note.text.as_str(),
        })
        .collect();
    let token_counts = super::count_tokens_batch(embedder, &texts);
    let (packed, used) =
        super::token_pack(results, &token_counts, budget, json_overhead, |r| match r {
            UnifiedResult::Code(sr) => sr.score,
            UnifiedResult::Note(nr) => nr.score,
        });
    tracing::info!(
        chunks = packed.len(),
        tokens = used,
        budget,
        "Token-budgeted query"
    );
    (packed, Some((used, budget)))
}

/// Pack tagged results into a token budget
fn token_pack_tagged(
    results: Vec<reference::TaggedResult>,
    budget: usize,
    json_overhead: usize,
    embedder: &Embedder,
) -> (Vec<reference::TaggedResult>, TokenInfo) {
    let _span = tracing::info_span!("token_pack_tagged", budget).entered();

    let texts: Vec<&str> = results
        .iter()
        .map(|r| match &r.result {
            UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
            UnifiedResult::Note(nr) => nr.note.text.as_str(),
        })
        .collect();
    let token_counts = super::count_tokens_batch(embedder, &texts);
    let (packed, used) =
        super::token_pack(
            results,
            &token_counts,
            budget,
            json_overhead,
            |r| match &r.result {
                UnifiedResult::Code(sr) => sr.score,
                UnifiedResult::Note(nr) => nr.score,
            },
        );
    tracing::info!(
        chunks = packed.len(),
        tokens = used,
        budget,
        "Token-budgeted query (tagged)"
    );
    (packed, Some((used, budget)))
}

/// Name-only search: find by function/struct name, no embedding needed
fn cmd_query_name_only(
    cli: &Cli,
    store: &Store,
    query: &str,
    root: &std::path::Path,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_query_name_only", query).entered();
    let results = store.search_by_name(query, cli.limit)?;

    if results.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found.");
        }
        std::process::exit(signal::ExitCode::NoResults as i32);
    }

    // Convert to UnifiedResult for display
    let unified: Vec<UnifiedResult> = results.into_iter().map(UnifiedResult::Code).collect();

    // Token-budget packing (lazy embedder — only created when --tokens is set)
    let json_overhead = if cli.json {
        super::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let (unified, token_info) = if let Some(budget) = cli.tokens {
        let embedder = Embedder::new()?;
        token_pack_unified(unified, budget, json_overhead, &embedder)
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

    let config = cqs::config::Config::load(root);
    let references = reference::load_references(&config.references);

    let ref_idx = references
        .iter()
        .find(|r| r.name == ref_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                ref_name
            )
        })?;

    let results = reference::search_reference(
        ref_idx,
        query_embedding,
        filter,
        cli.limit,
        cli.threshold,
        false, // no weight for --ref scoped search
    )?;

    let tagged: Vec<reference::TaggedResult> = results
        .into_iter()
        .map(|r| reference::TaggedResult {
            result: UnifiedResult::Code(r),
            source: Some(ref_name.to_string()),
        })
        .collect();

    // Token-budget packing
    let json_overhead = if cli.json {
        super::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let (tagged, token_info) = if let Some(budget) = cli.tokens {
        token_pack_tagged(tagged, budget, json_overhead, embedder)
    } else {
        (tagged, None)
    };

    if tagged.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found in reference '{}'.", ref_name);
        }
        std::process::exit(signal::ExitCode::NoResults as i32);
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

    let config = cqs::config::Config::load(root);
    let references = reference::load_references(&config.references);

    let ref_idx = references
        .iter()
        .find(|r| r.name == ref_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                ref_name
            )
        })?;

    let results =
        reference::search_reference_by_name(ref_idx, query, cli.limit, cli.threshold, false)?;

    let tagged: Vec<reference::TaggedResult> = results
        .into_iter()
        .map(|r| reference::TaggedResult {
            result: UnifiedResult::Code(r),
            source: Some(ref_name.to_string()),
        })
        .collect();

    // Token-budget packing (lazy embedder — only created when --tokens is set)
    let json_overhead = if cli.json {
        super::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let (tagged, token_info) = if let Some(budget) = cli.tokens {
        let embedder = Embedder::new()?;
        token_pack_tagged(tagged, budget, json_overhead, &embedder)
    } else {
        (tagged, None)
    };

    if tagged.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found in reference '{}'.", ref_name);
        }
        std::process::exit(signal::ExitCode::NoResults as i32);
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
