//! Stage 3: Write embedded chunks to SQLite with call graph, function calls, and type edges.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use crossbeam_channel::Receiver;
use indicatif::ProgressBar;

use cqs::{Chunk, Embedding, Store};

use super::types::EmbeddedBatch;
use crate::cli::check_interrupted;

/// Stage 3: Write embedded chunks to SQLite with call graph, function calls, and type edges.
///
/// Returns `(total_embedded, total_cached, total_type_edges, total_calls)` counts.
pub(super) fn store_stage(
    embed_rx: Receiver<EmbeddedBatch>,
    store: &Store,
    parsed_count: &AtomicUsize,
    embedded_count: &AtomicUsize,
    progress: &ProgressBar,
) -> Result<(usize, usize, usize, usize)> {
    let _span = tracing::info_span!("store_stage").entered();
    let mut total_embedded = 0;
    let mut total_cached = 0;
    let mut total_type_edges = 0;
    let mut total_calls = 0;
    let mut deferred_type_edges: Vec<(PathBuf, Vec<cqs::parser::ChunkTypeRefs>)> = Vec::new();
    let mut deferred_chunk_calls: Vec<(String, cqs::parser::CallSite)> = Vec::new();

    for batch in embed_rx {
        if check_interrupted() {
            break;
        }

        // PERF-28: Use pre-extracted chunk calls from the parse stage (rayon parallel)
        // instead of re-parsing each chunk sequentially here.
        // Defer chunk_calls — they reference caller_id with FK on chunks(id),
        // and chunks from later batches aren't in the DB yet.
        deferred_chunk_calls.extend(batch.relationships.chunk_calls);

        let batch_count = batch.chunk_embeddings.len();
        let no_calls: Vec<(String, cqs::parser::CallSite)> = Vec::new();

        // Upsert chunks WITHOUT calls (calls are deferred)
        if batch.file_mtimes.len() <= 1 {
            // Fast path: single file or no mtimes
            let mtime = batch.file_mtimes.values().next().copied();
            store.upsert_chunks_and_calls(&batch.chunk_embeddings, mtime, &no_calls)?;
        } else {
            // Multi-file batch: group by file and upsert with correct per-file mtime.
            let mut by_file: HashMap<PathBuf, Vec<(Chunk, Embedding)>> = HashMap::new();
            for (chunk, embedding) in batch.chunk_embeddings {
                by_file
                    .entry(chunk.file.clone())
                    .or_default()
                    .push((chunk, embedding));
            }

            for (file, pairs) in &by_file {
                let mtime = batch.file_mtimes.get(file.as_path()).copied();
                store.upsert_chunks_and_calls(pairs, mtime, &no_calls)?;
            }
        }

        // Store function calls extracted during parsing (for the `function_calls` table)
        for (file, function_calls) in &batch.relationships.function_calls {
            for fc in function_calls {
                total_calls += fc.calls.len();
            }
            if let Err(e) = store.upsert_function_calls(file, function_calls) {
                tracing::warn!(
                    file = %file.display(),
                    error = %e,
                    "Failed to store function calls"
                );
            }
        }

        // Defer type edge insertion — collect for later.
        // Type edges reference chunk IDs that may be in later batches,
        // so we insert them after all chunks are committed.
        for (file, chunk_type_refs) in batch.relationships.type_refs {
            for ctr in &chunk_type_refs {
                total_type_edges += ctr.type_refs.len();
            }
            deferred_type_edges.push((file, chunk_type_refs));
        }

        total_embedded += batch_count;
        total_cached += batch.cached_count;

        let parsed = parsed_count.load(Ordering::Relaxed);
        let embedded = embedded_count.load(Ordering::Relaxed);
        progress.set_position(parsed as u64);
        progress.set_message(format!(
            "parsed:{} embedded:{} written:{}",
            parsed, embedded, total_embedded
        ));
    }

    // Insert deferred chunk calls now that all chunks are in the DB.
    // chunk_calls reference caller_id with FK on chunks(id), so they
    // must be inserted after all chunks across all batches are committed.
    if !deferred_chunk_calls.is_empty() {
        if let Err(e) = store.upsert_calls_batch(&deferred_chunk_calls) {
            tracing::warn!(
                count = deferred_chunk_calls.len(),
                error = %e,
                "Failed to store deferred chunk calls"
            );
        }
        total_calls += deferred_chunk_calls.len();
    }

    // Insert deferred type edges now that all chunks are in the DB.
    // Type edges reference source_chunk_id with a FK constraint, so they
    // must be inserted after all chunks across all batches are committed.
    // PERF-26: Single transaction for all files instead of per-file transactions.
    if !deferred_type_edges.is_empty() {
        if let Err(e) = store.upsert_type_edges_for_files(&deferred_type_edges) {
            tracing::warn!(
                files = deferred_type_edges.len(),
                error = %e,
                "Failed to store deferred type edges"
            );
        }
    }

    Ok((total_embedded, total_cached, total_type_edges, total_calls))
}
