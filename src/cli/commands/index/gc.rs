//! GC command for cqs
//!
//! Removes chunks for deleted/stale files, cleans orphan call graph entries,
//! and rebuilds the HNSW index.
//!
//! Core struct is [`GcOutput`]; CLI builds inline, batch builds inline.

use std::collections::HashSet;

use anyhow::{Context as _, Result};

use cqs::{HnswKind, Parser};

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
// Args + core (surface-agnostic)
// ---------------------------------------------------------------------------

/// Input for [`gc_core`]. gc takes no positional or flag input today — the
/// struct exists for schema/clap-pin uniformity and a future `--dry-run`
/// (count-without-prune) lands here as a field.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct GcArgs {}

/// Surface-agnostic core for `cqs gc`.
///
/// Prunes chunks/calls/type-edges/summaries for deleted files in a single
/// transaction, drops orphan sparse vectors, and rebuilds the enriched HNSW
/// when chunks were removed. Mutating — takes a `ReadWrite` store. The caller
/// (CLI) owns the index lock and store open; gc has no daemon path (the
/// daemon's `dispatch_gc` bails by design — a writable store can't be shared
/// with the serving snapshot), so this core has a single production caller.
///
/// HNSW dirty-flag handling: marking dirty before the rebuild is load-bearing
/// (concurrent searches must fall back to brute-force, not return orphan IDs
/// from the stale graph), so a `set_hnsw_dirty` failure aborts rather than
/// proceeding with an un-marked rebuild.
pub(crate) fn gc_core(
    store: &cqs::Store<cqs::store::ReadWrite>,
    root: &std::path::Path,
    cqs_dir: &std::path::Path,
    _args: &GcArgs,
) -> Result<GcOutput> {
    let _span = tracing::info_span!("gc_core").entered();

    // Enumerate current files
    let parser = Parser::new()?;
    let exts = parser.supported_extensions();
    let files = cqs::enumerate_files(root, &exts, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    // Count what we'll clean before doing it
    let (stale_count, missing_count) = match store.count_stale_files(&file_set, root) {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count stale files");
            (0, 0)
        }
    };

    // All prune operations in a single transaction so concurrent readers
    // never see chunks deleted but orphan call/type/summary entries remaining.
    let prune = store
        .prune_all(&file_set, root)
        .context("Failed to prune stale entries from index")?;
    let pruned_chunks = prune.pruned_chunks as usize;
    let pruned_calls = prune.pruned_calls as usize;
    let pruned_type_edges = prune.pruned_type_edges as usize;
    let pruned_summaries = prune.pruned_summaries;
    // Prune orphaned sparse vectors
    let pruned_sparse = match store.prune_orphan_sparse_vectors() {
        Ok(n) => {
            if n > 0 {
                tracing::debug!(pruned_sparse = n, "Pruned orphan sparse vectors");
            }
            n
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to prune orphan sparse vectors");
            0
        }
    };
    tracing::debug!(
        pruned_chunks,
        pruned_calls,
        pruned_type_edges,
        pruned_summaries,
        pruned_sparse,
        "GC prune complete"
    );

    // Rebuild HNSW if we pruned chunks. Delete the stale HNSW first so
    // concurrent searches fall back to brute-force during the rebuild window
    // rather than returning orphan IDs from the old index.
    let hnsw_vectors = if pruned_chunks > 0 {
        // Failing to mark HNSW dirty means concurrent searches could return
        // stale results from the old index during rebuild. Abort.
        // GC only rebuilds enriched; base is left alone for the next full
        // `cqs index` to catch up.
        store
            .set_hnsw_dirty(HnswKind::Enriched, true)
            .context("Failed to mark enriched HNSW dirty before GC rebuild")?;
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
        match build_hnsw_index(store, cqs_dir)? {
            Some((total, cqs::hnsw::SaveOutcome::Saved)) => {
                if let Err(e) = store.set_hnsw_dirty(HnswKind::Enriched, false) {
                    tracing::warn!(error = %e, "Failed to clear enriched HNSW dirty flag after rebuild");
                }
                Some(total)
            }
            Some((total, cqs::hnsw::SaveOutcome::DiscardedStale)) => {
                // A concurrent writer moved the store past the GC build
                // snapshot; the on-disk index was left untouched and the
                // dirty flag stays set so search brute-forces until rebuild.
                tracing::warn!("GC HNSW save discarded (concurrent writer); dirty flag stays set");
                Some(total)
            }
            None => None,
        }
    } else {
        None
    };

    Ok(GcOutput {
        stale_files: stale_count as usize,
        missing_files: missing_count as usize,
        pruned_chunks,
        pruned_calls,
        pruned_type_edges,
        pruned_summaries,
        hnsw_rebuilt: pruned_chunks > 0,
        hnsw_vectors,
    })
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

/// Run garbage collection on the index
pub(crate) fn cmd_gc(cli: &crate::cli::definitions::Cli, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_gc").entered();

    let ctx = crate::cli::CommandContext::open_readwrite(cli)?;
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    // Acquire lock to prevent race with watch/index
    let _lock = acquire_index_lock(cqs_dir)?;

    let output = gc_core(store, root, cqs_dir, &GcArgs::default())?;

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        render_gc_text(&output);
    }

    Ok(())
}

/// Plain-text renderer for `cqs gc`. Reads the typed [`GcOutput`] so text and
/// JSON can never drift.
fn render_gc_text(output: &GcOutput) {
    let stale_count = output.stale_files;
    let missing_count = output.missing_files;
    let pruned_chunks = output.pruned_chunks;
    let pruned_calls = output.pruned_calls;
    let pruned_type_edges = output.pruned_type_edges;
    let pruned_summaries = output.pruned_summaries;
    let hnsw_vectors = output.hnsw_vectors;
    {
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
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `cqs gc` takes no positional/flag input, so `GcArgs` is empty and an
    /// MCP no-params call (`{}`) deserializes cleanly to it. Pins that the
    /// Args surface stays parameter-free until a field is deliberately added.
    #[test]
    fn gc_args_deserialize_empty() {
        let _: GcArgs = serde_json::from_str("{}").unwrap();
    }

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

    // Assert ALL 8 fields of GcOutput are correctly named in JSON
    #[test]
    fn test_gc_output_all_fields() {
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

        // Assert every field name and value
        assert_eq!(json["stale_files"], 2);
        assert_eq!(json["missing_files"], 1);
        assert_eq!(json["pruned_chunks"], 15);
        assert_eq!(json["pruned_calls"], 30);
        assert_eq!(json["pruned_type_edges"], 5);
        assert_eq!(json["pruned_summaries"], 3);
        assert_eq!(json["hnsw_rebuilt"], true);
        assert_eq!(json["hnsw_vectors"], 500);

        // Verify exact field count (no extra fields)
        let obj = json.as_object().unwrap();
        assert_eq!(
            obj.len(),
            8,
            "GcOutput should serialize to exactly 8 fields, got: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }
}
