//! GC command for cqs
//!
//! Removes chunks for deleted/stale files, cleans orphan call graph entries,
//! and rebuilds the HNSW index.
//!
//! Core struct is [`GcOutput`]; CLI builds inline, batch builds inline.

use std::collections::HashSet;

use anyhow::{Context as _, Result};

use cqs::Parser;

use crate::cli::acquire_index_lock;

use super::build_hnsw_index;

// ---------------------------------------------------------------------------
// Output struct
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct GcOutput {
    pub stale_files: usize,
    pub missing_files: usize,
    pub pruned_chunks: usize,
    pub pruned_calls: usize,
    pub pruned_type_edges: usize,
    pub pruned_summaries: usize,
    pub hnsw_rebuilt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hnsw_vectors: Option<usize>,
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

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

    // All prune operations in a single transaction so concurrent readers
    // never see chunks deleted but orphan call/type/summary entries remaining.
    let prune = store
        .prune_all(&file_set)
        .context("Failed to prune stale entries from index")?;
    let pruned_chunks = prune.pruned_chunks as usize;
    let pruned_calls = prune.pruned_calls as usize;
    let pruned_type_edges = prune.pruned_type_edges as usize;
    let pruned_summaries = prune.pruned_summaries;
    tracing::debug!(
        pruned_chunks,
        pruned_calls,
        pruned_type_edges,
        pruned_summaries,
        "GC prune complete"
    );

    // Rebuild HNSW if we pruned chunks. Delete the stale HNSW first so
    // concurrent searches fall back to brute-force during the rebuild window
    // rather than returning orphan IDs from the old index (RT-DATA-2).
    let hnsw_vectors = if pruned_chunks > 0 {
        if let Err(e) = store.set_hnsw_dirty(true) {
            tracing::warn!(error = %e, "Failed to mark HNSW dirty before rebuild");
        }
        let hnsw_path = cqs_dir.join("index.hnsw.graph");
        if hnsw_path.exists() {
            for file_name in cqs::hnsw::HNSW_ALL_EXTENSIONS
                .iter()
                .map(|ext| format!("index.{ext}"))
            {
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
        let result = build_hnsw_index(&store, &cqs_dir)?;
        if result.is_some() {
            if let Err(e) = store.set_hnsw_dirty(false) {
                tracing::warn!(error = %e, "Failed to clear HNSW dirty flag after rebuild");
            }
        }
        result
    } else {
        None
    };

    if json {
        let output = GcOutput {
            stale_files: stale_count as usize,
            missing_files: missing_count as usize,
            pruned_chunks,
            pruned_calls,
            pruned_type_edges,
            pruned_summaries,
            hnsw_rebuilt: pruned_chunks > 0,
            hnsw_vectors,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        if pruned_chunks == 0
            && pruned_calls == 0
            && pruned_type_edges == 0
            && pruned_summaries == 0
        {
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
            if pruned_summaries > 0 {
                println!(
                    "Removed {} orphan LLM summar{}",
                    pruned_summaries,
                    if pruned_summaries == 1 { "y" } else { "ies" },
                );
            }
            if let Some(vectors) = hnsw_vectors {
                println!("Rebuilt HNSW index: {vectors} vectors");
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_output_serialization() {
        let output = GcOutput {
            stale_files: 2,
            missing_files: 1,
            pruned_chunks: 15,
            pruned_calls: 30,
            pruned_type_edges: 5,
            pruned_summaries: 3,
            hnsw_rebuilt: true,
            hnsw_vectors: Some(500),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["pruned_chunks"], 15);
        assert_eq!(json["hnsw_rebuilt"], true);
        assert_eq!(json["hnsw_vectors"], 500);
    }

    #[test]
    fn test_gc_output_no_hnsw() {
        let output = GcOutput {
            stale_files: 0,
            missing_files: 0,
            pruned_chunks: 0,
            pruned_calls: 0,
            pruned_type_edges: 0,
            pruned_summaries: 0,
            hnsw_rebuilt: false,
            hnsw_vectors: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("hnsw_vectors").is_none());
    }
}
