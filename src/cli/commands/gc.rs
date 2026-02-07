//! GC command for cqs
//!
//! Removes chunks for deleted/stale files, cleans orphan call graph entries,
//! and rebuilds the HNSW index.

use std::collections::HashSet;

use anyhow::{bail, Result};

use cqs::{Parser, Store};

use crate::cli::{acquire_index_lock, find_project_root};

use super::build_hnsw_index;

/// Run garbage collection on the index
pub(crate) fn cmd_gc(json: bool) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    // Acquire lock to prevent race with watch/index
    let _lock = acquire_index_lock(&cq_dir)?;

    let store = Store::open(&index_path)?;

    // Enumerate current files
    let parser = Parser::new()?;
    let files = cqs::enumerate_files(&root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    // Count what we'll clean before doing it
    let (stale_count, missing_count) = store.count_stale_files(&file_set).unwrap_or((0, 0));

    // Prune chunks for missing files
    let pruned_chunks = store.prune_missing(&file_set)?;

    // Prune orphan call graph entries
    let pruned_calls = store.prune_stale_calls()?;

    // Rebuild HNSW if we pruned anything
    let hnsw_vectors = if pruned_chunks > 0 {
        build_hnsw_index(&store, &cq_dir)?
    } else {
        None
    };

    if json {
        let result = serde_json::json!({
            "stale_files": stale_count,
            "missing_files": missing_count,
            "pruned_chunks": pruned_chunks,
            "pruned_calls": pruned_calls,
            "hnsw_rebuilt": pruned_chunks > 0,
            "hnsw_vectors": hnsw_vectors,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if pruned_chunks == 0 && pruned_calls == 0 {
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
            if let Some(vectors) = hnsw_vectors {
                println!("Rebuilt HNSW index: {} vectors", vectors);
            }
        }
        if stale_count > 0 {
            println!(
                "\nNote: {} file{} changed since last index. Run 'cqs index' to update.",
                stale_count,
                if stale_count == 1 { "" } else { "s" },
            );
        }
    }

    Ok(())
}
