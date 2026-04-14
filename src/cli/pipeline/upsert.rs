//! Stage 3: Write embedded chunks to SQLite with call graph, function calls, and type edges.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use crossbeam_channel::Receiver;
use indicatif::ProgressBar;

use cqs::{Chunk, Embedding, Store};

use super::types::EmbeddedBatch;
use crate::cli::check_interrupted;

/// How often (in batches) to flush deferred vecs.
/// Overridable via `CQS_DEFERRED_FLUSH_INTERVAL` env var.
fn deferred_flush_interval() -> usize {
    std::env::var("CQS_DEFERRED_FLUSH_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
}

/// Attempt to flush deferred chunk calls whose FK targets (caller_id) already
/// exist in the database. Returns calls that could NOT be flushed (missing FK).
fn flush_calls(
    store: &Store,
    calls: Vec<(String, cqs::parser::CallSite)>,
) -> Vec<(String, cqs::parser::CallSite)> {
    if calls.is_empty() {
        return Vec::new();
    }

    let unique_ids: HashSet<&str> = calls.iter().map(|(id, _)| id.as_str()).collect();
    let existing = match store.existing_chunk_ids(&unique_ids) {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check existing chunk IDs, retaining all deferred calls");
            return calls;
        }
    };

    let (ready, mut retained): (Vec<_>, Vec<_>) = calls
        .into_iter()
        .partition(|(id, _)| existing.contains(id.as_str()));

    if !ready.is_empty() {
        tracing::info!(
            flushed = ready.len(),
            retained = retained.len(),
            "Periodic flush: deferred chunk calls"
        );
        if let Err(e) = store.upsert_calls_batch(&ready) {
            // EH-10: on transient upsert failure, push `ready` back into
            // `retained` so the next flush attempt retries them. Discarding
            // was silent permanent data loss.
            tracing::warn!(
                count = ready.len(),
                error = %e,
                "Periodic flush of deferred calls failed, re-buffering for retry"
            );
            retained.extend(ready);
        }
    }

    retained
}

/// Attempt to flush deferred type edges. Type edge resolution already handles
/// missing chunks gracefully (warns and skips), so we flush everything.
///
/// Returns `true` if the flush succeeded (caller should clear the buffer),
/// `false` if it failed (caller must leave the buffer intact for retry).
#[must_use]
fn flush_type_edges(store: &Store, edges: &[(PathBuf, Vec<cqs::parser::ChunkTypeRefs>)]) -> bool {
    if edges.is_empty() {
        return true;
    }
    tracing::info!(files = edges.len(), "Periodic flush: deferred type edges");
    match store.upsert_type_edges_for_files(edges) {
        Ok(()) => true,
        Err(e) => {
            // EH-11: leave the buffer intact for retry rather than silently
            // dropping all deferred edges on transient failure.
            tracing::warn!(
                files = edges.len(),
                error = %e,
                "Periodic flush of deferred type edges failed, retaining for retry"
            );
            false
        }
    }
}

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
    let mut batch_counter: usize = 0;
    let flush_interval = deferred_flush_interval();

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

        // RM-9: Periodic flush to bound deferred vec memory.
        batch_counter += 1;
        if batch_counter.is_multiple_of(flush_interval) {
            deferred_chunk_calls = flush_calls(store, std::mem::take(&mut deferred_chunk_calls));
            // EH-11: only clear the buffer on successful flush; on failure
            // the buffer is left intact so the next flush retries.
            if flush_type_edges(store, &deferred_type_edges) {
                deferred_type_edges.clear();
            }
        }
    }

    // Final flush: insert any remaining deferred items now that all chunks are in the DB.
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

    // PERF-26: Single transaction for all remaining files instead of per-file transactions.
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
