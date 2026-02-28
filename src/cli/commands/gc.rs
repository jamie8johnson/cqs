//! GC command for cqs
//!
//! Removes chunks for deleted/stale files, cleans orphan call graph entries,
//! and rebuilds the HNSW index.

use std::collections::HashSet;

use anyhow::{Context as _, Result};

use cqs::Parser;

use crate::cli::acquire_index_lock;

use super::build_hnsw_index;

/// Run garbage collection on the index
pub(crate) fn cmd_gc(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_gc").entered();

    let (store, root, cqs_dir) = crate::cli::open_project_store()?;

    // Acquire lock to prevent race with watch/index
    let _lock = acquire_index_lock(&cqs_dir)?;

    // Enumerate current files
    let parser = Parser::new()?;
    let exts = parser.supported_extensions();
    let files = cqs::enumerate_files(&root, &exts, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    // Count what we'll clean before doing it
    let (stale_count, missing_count) = match store.count_stale_files(&file_set) {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count stale files");
            (0, 0)
        }
    };

    // Prune in order: chunks first, then orphan references.
    // Each operation is individually transactional. If a crash interrupts between
    // operations, orphan call/type edges remain harmless and are cleaned on next GC.
    // prune_stale_calls/prune_stale_type_edges are idempotent (DELETE WHERE NOT IN chunks).
    let pruned_chunks = store
        .prune_missing(&file_set)
        .context("Failed to prune deleted files from index")?;
    tracing::debug!(pruned_chunks, "Chunks pruned");

    // Prune orphan call graph entries (references chunks that no longer exist)
    let pruned_calls = store
        .prune_stale_calls()
        .context("Failed to prune orphan call graph entries")?;
    tracing::debug!(pruned_calls, "Calls pruned");

    // Prune orphan type edges (references chunks that no longer exist)
    let pruned_type_edges = store
        .prune_stale_type_edges()
        .context("Failed to prune orphan type edges")?;
    tracing::debug!(pruned_type_edges, "Type edges pruned");

    // Rebuild HNSW if we pruned chunks. Delete the stale HNSW first so
    // concurrent searches fall back to brute-force during the rebuild window
    // rather than returning orphan IDs from the old index (RT-DATA-2).
    let hnsw_vectors = if pruned_chunks > 0 {
        let hnsw_path = cqs_dir.join("index.hnsw.graph");
        if hnsw_path.exists() {
            for file_name in &[
                "index.hnsw.graph",
                "index.hnsw.data",
                "index.hnsw.checksum",
                "index.hnsw.id_map.json",
            ] {
                let path = cqs_dir.join(file_name);
                if let Err(e) = std::fs::remove_file(&path) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to delete stale HNSW file during GC"
                        );
                    }
                }
            }
            tracing::debug!("Deleted stale HNSW before rebuild");
        }
        build_hnsw_index(&store, &cqs_dir)?
    } else {
        None
    };

    if json {
        let result = serde_json::json!({
            "stale_files": stale_count,
            "missing_files": missing_count,
            "pruned_chunks": pruned_chunks,
            "pruned_calls": pruned_calls,
            "pruned_type_edges": pruned_type_edges,
            "hnsw_rebuilt": pruned_chunks > 0,
            "hnsw_vectors": hnsw_vectors,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if pruned_chunks == 0 && pruned_calls == 0 && pruned_type_edges == 0 {
            println!("Index is clean. Nothing to do.");
        } else {
            if pruned_chunks > 0 {
                println!(
                    "Removed {} chunk{} from {} missing file{}",
                    pruned_chunks,
                    if pruned_chunks == 1 { "" } else { "s" },
                    missing_count,
                    if missing_count == 1 { "" } else { "s" },
                );
            }
            if pruned_calls > 0 {
                println!(
                    "Removed {} orphan call graph entr{}",
                    pruned_calls,
                    if pruned_calls == 1 { "y" } else { "ies" },
                );
            }
            if pruned_type_edges > 0 {
                println!(
                    "Removed {} orphan type edge{}",
                    pruned_type_edges,
                    if pruned_type_edges == 1 { "" } else { "s" },
                );
            }
            if let Some(vectors) = hnsw_vectors {
                println!("Rebuilt HNSW index: {} vectors", vectors);
            }
        }
        if stale_count > 0 {
            eprintln!(
                "\nNote: {} file{} changed since last index. Run 'cqs index' to update.",
                stale_count,
                if stale_count == 1 { "" } else { "s" },
            );
        }
    }

    Ok(())
}
