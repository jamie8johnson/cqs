//! Query command for cqs
//!
//! Executes semantic search queries.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use cqs::parser::ChunkType;
use cqs::store::{ParentContext, UnifiedResult};
use cqs::{reference, Embedder, HnswIndex, Pattern, SearchFilter, Store};

use crate::cli::{display, find_project_root, signal, staleness, Cli};

/// Execute a semantic search query and display results
pub(crate) fn cmd_query(cli: &Cli, query: &str) -> Result<()> {
    let _span = tracing::info_span!("cmd_query", query_len = query.len()).entered();

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

    // Name-only mode: search by function/struct name, skip embedding entirely
    if cli.name_only {
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

    // Resolve parent context if --expand requested
    let parents = if cli.expand {
        resolve_parent_context(&results, &store, &root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand { Some(&parents) } else { None };

    // Proactive staleness warning (stderr, doesn't pollute JSON)
    if !cli.quiet {
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
            staleness::warn_stale_results(&store, &origins);
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
            display::display_unified_results_json(&results, query, parents_ref)?;
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

    if tagged.is_empty() {
        if cli.json {
            println!(r#"{{"results":[],"query":"{}","total":0}}"#, query);
        } else {
            println!("No results found.");
        }
        std::process::exit(signal::ExitCode::NoResults as i32);
    }

    if cli.json {
        display::display_tagged_results_json(&tagged, query, parents_ref)?;
    } else {
        display::display_tagged_results(&tagged, &root, cli.no_content, cli.context, parents_ref)?;
    }

    Ok(())
}

/// Name-only search: find by function/struct name, no embedding needed
fn cmd_query_name_only(
    cli: &Cli,
    store: &Store,
    query: &str,
    root: &std::path::Path,
) -> Result<()> {
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

    // Resolve parent context if --expand requested
    let parents = if cli.expand {
        resolve_parent_context(&unified, store, root)
    } else {
        HashMap::new()
    };
    let parents_ref = if cli.expand { Some(&parents) } else { None };

    if cli.json {
        display::display_unified_results_json(&unified, query, parents_ref)?;
    } else {
        display::display_unified_results(&unified, root, cli.no_content, cli.context, parents_ref)?;
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
    let stored_parents = store.get_chunks_by_ids(&id_refs).unwrap_or_default();

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
            if let Ok(content) = std::fs::read_to_string(&abs_path) {
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
        }
    }

    parents
}
