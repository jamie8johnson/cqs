//! Stats command for cqs
//!
//! Displays index statistics.
//!
//! Core JSON construction is in [`stats_to_json`] so batch mode can reuse it.

use std::collections::HashSet;

use anyhow::{Context as _, Result};

use cqs::{HnswIndex, Parser};

/// Build the core stats JSON shared between CLI and batch.
///
/// Contains: total_chunks, total_files, notes, call_graph, type_graph,
/// by_language, by_type, model, schema_version.
/// Callers add context-specific fields (stale_files, errors, etc.).
pub(crate) fn stats_to_json(store: &cqs::Store) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("stats_to_json").entered();
    let stats = store.stats().context("Failed to read index statistics")?;
    let note_count = store.note_count()?;
    let fc_stats = store.function_call_stats()?;
    let te_stats = store.type_edge_stats()?;

    Ok(serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "notes": note_count,
        "call_graph": {
            "total_calls": fc_stats.total_calls,
            "unique_callers": fc_stats.unique_callers,
            "unique_callees": fc_stats.unique_callees,
        },
        "type_graph": {
            "total_edges": te_stats.total_edges,
            "unique_types": te_stats.unique_types,
        },
        "by_language": stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect::<std::collections::HashMap<_, _>>(),
        "by_type": stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect::<std::collections::HashMap<_, _>>(),
        "model": stats.model_name,
        "schema_version": stats.schema_version,
    }))
}

/// Display index statistics (chunk counts, languages, types)
pub(crate) fn cmd_stats(ctx: &crate::cli::CommandContext, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_stats").entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;
    let stats = store.stats().context("Failed to read index statistics")?;

    // Check staleness by scanning filesystem
    let parser = Parser::new()?;
    let files = crate::cli::enumerate_files(root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();
    let (stale_count, missing_count) = store
        .count_stale_files(&file_set)
        .context("Failed to count stale files")?;

    // Use count_vectors to avoid loading full HNSW index just for stats
    let hnsw_vectors = HnswIndex::count_vectors(cqs_dir, "index");
    let note_count = store.note_count()?;
    let fc_stats = store.function_call_stats()?;
    let (call_count, caller_count, callee_count) = (
        fc_stats.total_calls,
        fc_stats.unique_callers,
        fc_stats.unique_callees,
    );
    let te_stats = store.type_edge_stats()?;

    if json || ctx.cli.json {
        let mut output = stats_to_json(store)?;
        // CLI-specific fields
        output["stale_files"] = serde_json::json!(stale_count);
        output["missing_files"] = serde_json::json!(missing_count);
        output["created_at"] = serde_json::json!(stats.created_at);
        output["hnsw_vectors"] = serde_json::json!(hnsw_vectors);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Index Statistics");
        println!("================");
        println!();
        println!("Total chunks: {}", stats.total_chunks);
        println!("Total files:  {}", stats.total_files);
        println!();
        println!("By language:");
        for (lang, count) in &stats.chunks_by_language {
            println!("  {}: {}", lang, count);
        }
        println!();
        println!("By type:");
        for (chunk_type, count) in &stats.chunks_by_type {
            println!("  {}: {}", chunk_type, count);
        }
        println!();
        println!("Model: {}", stats.model_name);
        println!("Schema: v{}", stats.schema_version);
        println!("Created: {}", stats.created_at);
        println!();
        println!("Notes: {}", note_count);
        println!(
            "Call graph: {} calls ({} callers, {} callees)",
            call_count, caller_count, callee_count
        );
        println!(
            "Type graph: {} edges ({} types)",
            te_stats.total_edges, te_stats.unique_types
        );

        // HNSW index status (use count_vectors to avoid loading full index)
        println!();
        match hnsw_vectors {
            Some(count) => {
                println!("HNSW index: {} vectors (O(log n) search)", count);
            }
            None => {
                println!("HNSW index: not built (using brute-force O(n) search)");
                if stats.total_chunks > 10_000 {
                    println!("  Tip: Run 'cqs index' to build HNSW for faster search");
                }
            }
        }

        // Staleness warning
        if stale_count > 0 || missing_count > 0 {
            eprintln!();
            if stale_count > 0 {
                eprintln!(
                    "Stale: {} file{} changed since last index",
                    stale_count,
                    if stale_count == 1 { "" } else { "s" }
                );
            }
            if missing_count > 0 {
                eprintln!(
                    "Missing: {} file{} deleted since last index",
                    missing_count,
                    if missing_count == 1 { "" } else { "s" }
                );
            }
            eprintln!("  Run 'cqs index' to update, or 'cqs gc' to clean up deleted files");
        }

        // Warning for very large indexes
        if stats.total_chunks > 50_000 {
            println!();
            println!(
                "Warning: {} chunks is a large index. Consider:",
                stats.total_chunks
            );
            println!("  - Using --path to limit search scope");
            println!("  - Splitting into multiple projects");
        }
    }

    Ok(())
}
