//! Stats command for cqs
//!
//! Displays index statistics.

use std::collections::HashSet;

use anyhow::Result;

use cqs::{HnswIndex, Parser};

use crate::cli::Cli;

/// Display index statistics (chunk counts, languages, types)
pub(crate) fn cmd_stats(cli: &Cli) -> Result<()> {
    let _span = tracing::info_span!("cmd_stats").entered();
    let (store, root, cqs_dir) = crate::cli::open_project_store()?;
    let stats = store.stats()?;

    // Check staleness by scanning filesystem
    let parser = Parser::new()?;
    let files = crate::cli::enumerate_files(&root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();
    let (stale_count, missing_count) = store.count_stale_files(&file_set)?;

    // Use count_vectors to avoid loading full HNSW index just for stats
    let hnsw_vectors = HnswIndex::count_vectors(&cqs_dir, "index");
    let note_count = store.note_count()?;
    let fc_stats = store.function_call_stats()?;
    let (call_count, caller_count, callee_count) = (
        fc_stats.total_calls,
        fc_stats.unique_callers,
        fc_stats.unique_callees,
    );

    if cli.json {
        let json = serde_json::json!({
            "total_chunks": stats.total_chunks,
            "total_files": stats.total_files,
            "stale_files": stale_count,
            "missing_files": missing_count,
            "notes": note_count,
            "call_graph": {
                "total_calls": call_count,
                "unique_callers": caller_count,
                "unique_callees": callee_count,
            },
            "by_language": stats.chunks_by_language.iter()
                .map(|(l, c)| (l.to_string(), c))
                .collect::<std::collections::HashMap<_, _>>(),
            "by_type": stats.chunks_by_type.iter()
                .map(|(t, c)| (t.to_string(), c))
                .collect::<std::collections::HashMap<_, _>>(),
            "model": stats.model_name,
            "schema_version": stats.schema_version,
            "created_at": stats.created_at,
            "hnsw_vectors": hnsw_vectors,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
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
            println!();
            if stale_count > 0 {
                println!(
                    "Stale: {} file{} changed since last index",
                    stale_count,
                    if stale_count == 1 { "" } else { "s" }
                );
            }
            if missing_count > 0 {
                println!(
                    "Missing: {} file{} deleted since last index",
                    missing_count,
                    if missing_count == 1 { "" } else { "s" }
                );
            }
            println!("  Run 'cqs index' to update, or 'cqs gc' to clean up deleted files");
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
