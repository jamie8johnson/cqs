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
            // On transient upsert failure, push `ready` back into `retained`
            // so the next flush attempt retries them. Discarding would be
            // silent permanent data loss.
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
            // Leave the buffer intact for retry rather than silently
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
    // Track every chunk id we upsert per file so we can prune phantom rows
    // (chunks at the same origin from prior runs whose ID format / hash
    // changed) after the loop completes. Per-batch pruning is unsafe because
    // a single file's chunks can split across batches when the file is large
    // — pruning mid-loop would delete chunks the next batch is about to
    // re-insert. The watch path passes per-file live_ids to
    // `upsert_chunks_calls_and_prune`; this keeps the full reindex pipeline
    // in line.
    let mut live_ids_per_file: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut batch_counter: usize = 0;
    let flush_interval = deferred_flush_interval();

    for batch in embed_rx {
        if check_interrupted() {
            break;
        }

        // Use pre-extracted chunk calls from the parse stage (rayon parallel)
        // instead of re-parsing each chunk sequentially here.
        // Defer chunk_calls — they reference caller_id with FK on chunks(id),
        // and chunks from later batches aren't in the DB yet.
        deferred_chunk_calls.extend(batch.relationships.chunk_calls);

        let batch_count = batch.chunk_embeddings.len();
        let no_calls: Vec<(String, cqs::parser::CallSite)> = Vec::new();

        // Upsert chunks WITHOUT calls (calls are deferred). Also accumulate
        // per-file live IDs for the post-loop prune pass.
        //
        // When `uncached_need_embedding` is set, the chunks past index
        // `cached_count` carry zero-vec sentinels (skip-first-pass path
        // under `--llm-summaries`). Cached chunks still carry real
        // embeddings (from the global cache). Slice the batch and route
        // each half to the correct upsert path so cached chunks land at
        // `needs_embedding=0` while sentinel chunks land at
        // `needs_embedding=1`.
        let cached_slice_end = batch.cached_count.min(batch.chunk_embeddings.len());
        let mut by_file_real: HashMap<PathBuf, Vec<(Chunk, Embedding)>> = HashMap::new();
        let mut by_file_sentinel: HashMap<PathBuf, Vec<Chunk>> = HashMap::new();
        for (i, (chunk, embedding)) in batch.chunk_embeddings.into_iter().enumerate() {
            live_ids_per_file
                .entry(chunk.file.clone())
                .or_default()
                .insert(chunk.id.clone());
            if i < cached_slice_end || !batch.uncached_need_embedding {
                by_file_real
                    .entry(chunk.file.clone())
                    .or_default()
                    .push((chunk, embedding));
            } else {
                // Past cached_count and skip-first-pass mode is on — chunk
                // carries a zero-vec sentinel; route to the unembedded upsert.
                by_file_sentinel
                    .entry(chunk.file.clone())
                    .or_default()
                    .push(chunk);
            }
        }

        for (file, pairs) in &by_file_real {
            let mtime = batch.file_mtimes.get(file.as_path()).copied();
            store.upsert_chunks_and_calls(pairs, mtime, &no_calls)?;
        }
        for (file, chunks) in &by_file_sentinel {
            let mtime = batch.file_mtimes.get(file.as_path()).copied();
            store.upsert_chunks_unembedded_batch(chunks, mtime)?;
        }

        // Store function calls extracted during parsing (for the
        // `function_calls` table). Defer-and-batch like type edges: a
        // per-file `upsert_function_calls` would open one transaction per
        // file (~2,500 BEGIN/COMMIT round-trips on a typical wire). Collect
        // every (file, calls) tuple first, then a single batched call writes
        // them all in one transaction.
        let mut function_call_entries: Vec<(PathBuf, Vec<cqs::parser::FunctionCalls>)> =
            Vec::with_capacity(batch.relationships.function_calls.len());
        for (file, function_calls) in batch.relationships.function_calls {
            for fc in &function_calls {
                total_calls += fc.calls.len();
            }
            function_call_entries.push((file, function_calls));
        }
        if !function_call_entries.is_empty() {
            if let Err(e) = store.upsert_function_calls_for_files(&function_call_entries) {
                tracing::warn!(
                    files = function_call_entries.len(),
                    error = %e,
                    "Failed to store batched function calls"
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

        // Periodic flush to bound deferred vec memory.
        batch_counter += 1;
        if batch_counter.is_multiple_of(flush_interval) {
            deferred_chunk_calls = flush_calls(store, std::mem::take(&mut deferred_chunk_calls));
            // Only clear the buffer on successful flush; on failure the
            // buffer is left intact so the next flush retries.
            if flush_type_edges(store, &deferred_type_edges) {
                deferred_type_edges.clear();
            }
        }
    }

    // Prune phantom chunks per file. Walks every origin we touched, deletes
    // rows whose ID isn't in the current live set. Catches old-format chunk
    // IDs from prior chunker versions (e.g. `:t3wN:` middle segments, `:wN`
    // window suffixes). Mirrors the watch path's per-file
    // `upsert_chunks_calls_and_prune(prune_file: Some(...))` so a
    // `cqs index --force` after a chunker bump doesn't accumulate orphans.
    // Runs before the deferred call/edge flushes so any FK-cascading delete
    // from `chunks` happens before fresh calls reference the new IDs.
    let mut total_orphans_pruned: u32 = 0;
    for (file, live_ids) in &live_ids_per_file {
        let live_ids_vec: Vec<&str> = live_ids.iter().map(|s| s.as_str()).collect();
        match store.delete_phantom_chunks(file.as_path(), &live_ids_vec) {
            Ok(deleted) => {
                total_orphans_pruned += deleted;
            }
            Err(e) => {
                tracing::warn!(
                    file = %file.display(),
                    error = %e,
                    "delete_phantom_chunks failed; orphan rows from prior chunker versions may persist for this file"
                );
            }
        }
    }
    if total_orphans_pruned > 0 {
        tracing::info!(
            count = total_orphans_pruned,
            "Pruned phantom chunks from prior chunker versions (#1283)"
        );
    }

    // Final flush: insert any remaining deferred items now that all chunks
    // are in the DB. Only credit `total_calls` on a successful insert — the
    // upsert is a single transaction, so one bad FK rolls back the whole
    // batch and an Err means *zero* rows landed. Counting the attempt
    // anyway would make the "Pipeline indexing complete total_calls=N" log
    // lie about graph completeness.
    if !deferred_chunk_calls.is_empty() {
        match store.upsert_calls_batch(&deferred_chunk_calls) {
            Ok(()) => {
                total_calls += deferred_chunk_calls.len();
            }
            Err(e) => {
                tracing::warn!(
                    count = deferred_chunk_calls.len(),
                    error = %e,
                    "Failed to store deferred chunk calls — call graph is incomplete by this many rows"
                );
            }
        }
    }

    // Single transaction for all remaining files instead of per-file transactions.
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
