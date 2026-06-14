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

/// Attempt to flush leftover per-chunk calls whose FK targets (caller_id)
/// already exist in the database. Returns calls that could NOT be flushed
/// (missing FK). Used only for the end-of-run stragglers — a file's own calls
/// ride its fused write.
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

/// Per-file accumulator: buffers a file's embedded chunks (and its file-level
/// `function_calls`) as embed batches arrive, until the file is COMPLETE, then
/// flushes the whole file in ONE fused transaction (`upsert_file_fused`).
///
/// Completion signal: a file's reconcile fingerprint rides the embed batch
/// carrying its LAST chunk (the parser stamps it on the last-chunk window; the
/// GPU-failure split holds it with the requeued half). So a fingerprint arriving
/// for an origin means every one of the file's chunks has now arrived across this
/// and prior batches — even when a file straddles batches or a GPU split scatters
/// its chunks. A zero-chunk file's completion signal is its
/// `empty_file_fingerprints` entry.
///
/// Memory bound: only MULTI-batch files (>embed_batch_size chunks, default 64)
/// are held mid-accumulation; single-batch files flush on arrival. Peak hold is
/// the in-flight straddling files' embedded chunks (~dim*4 bytes + content per
/// chunk); a pathological 10k-chunk file is ~30 MB. Files flush incrementally as
/// they complete, so progress is preserved and the buffer drains continuously.
#[derive(Default)]
struct FileAccum {
    real: Vec<(Chunk, Embedding)>,
    sentinel: Vec<Chunk>,
    function_calls: Vec<cqs::parser::FunctionCalls>,
    type_refs: Vec<cqs::parser::ChunkTypeRefs>,
}

/// State retained for a file AFTER its first flush this run, so a late
/// (out-of-order) batch can ADDITIVELY re-flush without losing anything.
///
/// `live_ids` is the cumulative set of chunk ids committed so far — the additive
/// re-flush unions it into the prune set so the earlier flush's chunks survive.
///
/// `function_calls` / `type_refs` are the cumulative relationships seen for the
/// file across ALL its batches. They are CARRIED FORWARD here because the
/// file-level call-graph write (`write_function_calls_in_tx`) is a wholesale
/// DELETE-then-INSERT of the supplied set: an additive re-flush that supplied an
/// EMPTY set would wipe the call edges the first flush wrote and insert nothing
/// (silently deleting the file's whole call graph). Supplying the cumulative set
/// on EVERY flush makes the last write reconstruct the file's complete call
/// graph + type edges, robust to relationships arriving in any batch (early with
/// the fingerprint, or late with a straddling tail).
#[derive(Default)]
struct FlushedFile {
    fp: cqs::store::FileFingerprint,
    live_ids: HashSet<String>,
    function_calls: Vec<cqs::parser::FunctionCalls>,
    type_refs: Vec<cqs::parser::ChunkTypeRefs>,
}

/// Stage 3: per-file fused write of embedded chunks + call graph + function
/// calls + fingerprint stamp; type edges deferred to the end. #1835: each file
/// is written in ONE all-or-nothing transaction (`upsert_file_fused`) when it
/// completes, so the index is never left with chunks-without-calls,
/// calls-without-chunks, or a stamp ahead of its content.
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

    // Per-file accumulators, keyed by origin. A file is flushed (one fused tx)
    // the moment its fingerprint arrives (last chunk); only mid-straddle files
    // are ever held here.
    let mut accums: HashMap<PathBuf, FileAccum> = HashMap::new();

    // Per-chunk `calls` (FK on chunks(id)) keyed by caller chunk id. A file's
    // own calls are drained at its flush and written in the SAME fused tx (the
    // caller chunk is present in that tx, so the FK holds). Any leftover — a
    // call whose caller chunk never arrived (shouldn't happen) or a cross-file
    // straggler — flushes FK-checked at the end via `upsert_calls_batch`.
    let mut pending_chunk_calls: HashMap<String, Vec<cqs::parser::CallSite>> = HashMap::new();

    // Files already flushed this run, keyed by origin. This is the
    // order-independence safety net: the upstream parser stage keeps each file's
    // chunks inside ONE embed stage (file-aligned parse batches) so a file's
    // chunks, relationships, and fingerprint arrive together — but if a future
    // change ever let a file's chunks straddle the GPU/CPU work-steal again, a
    // fingerprint-bearing batch could arrive before (or after) some of the
    // file's chunks/relationships. Without this map those late chunks would land
    // in a fresh, never-flushed accumulator and be silently dropped (and the
    // fingerprint flush's prune would delete any earlier-arriving ones). With it,
    // a late batch for an already-flushed file triggers an ADDITIVE re-flush:
    //   * the prune live set is the UNION of prior-committed ids and new chunks,
    //     so no chunk the file produced is pruned away; and
    //   * the cumulative relationships (function_calls + type_refs) are supplied
    //     on EVERY flush, so the wholesale DELETE-then-INSERT call-graph write
    //     reconstructs the file's complete call graph + type edges instead of
    //     wiping them with an empty late set.
    // Dormant in the in-order production path; the deterministic out-of-order
    // regression tests exercise it directly.
    let mut flushed: HashMap<PathBuf, FlushedFile> = HashMap::new();

    // Flush one file in a single fused transaction: real + sentinel chunks +
    // FTS + per-chunk calls + file-level function_calls + phantom prune +
    // fingerprint stamp, all-or-nothing. On Err the tx rolled back → the file is
    // left in its prior coherent state, UNstamped, so the next reconcile
    // re-selects it (orphan-impossible in either direction). Type edges are
    // written right after, against the now-committed chunks.
    //
    // `function_calls` / `type_refs` are the CUMULATIVE relationship sets for the
    // file (not just this batch's), supplied explicitly so an additive re-flush
    // re-writes the file's whole call graph + type edges rather than wiping them
    // (the call-graph write is a wholesale DELETE-then-INSERT of the supplied
    // set). `extra_live_ids` are chunk ids ALREADY committed for this file in a
    // prior flush this run; they are unioned into the prune live set so the prior
    // flush's chunks survive. Both are empty for the normal single-flush path.
    // Returns `(embedded, calls, type_edges, committed_live_ids)` — the live ids
    // are returned (only on success) so the caller can track the cumulative set
    // for any further additive re-flush.
    let flush_file = |store: &Store,
                      pending_chunk_calls: &mut HashMap<String, Vec<cqs::parser::CallSite>>,
                      origin: &PathBuf,
                      accum: &FileAccum,
                      function_calls: &[cqs::parser::FunctionCalls],
                      type_refs: &[cqs::parser::ChunkTypeRefs],
                      fp: &cqs::store::FileFingerprint,
                      extra_live_ids: &HashSet<String>|
     -> (usize, usize, usize, Option<Vec<String>>) {
        // This flush's own chunk ids.
        let own_ids: Vec<String> = accum
            .real
            .iter()
            .map(|(c, _)| c.id.clone())
            .chain(accum.sentinel.iter().map(|c| c.id.clone()))
            .collect();
        // Complete live-id set for the prune: this flush's chunks PLUS any chunks
        // already committed for this file in a prior (out-of-order) flush, so the
        // prune never deletes the file's earlier-committed chunks.
        let mut live_ids: Vec<String> = own_ids.clone();
        live_ids.extend(extra_live_ids.iter().cloned());
        // Drain this file's own per-chunk calls (caller present in this tx). Only
        // for ids written in THIS tx (`own_ids`); a prior flush already drained
        // its own ids' calls.
        let mut file_calls: Vec<(String, cqs::parser::CallSite)> = Vec::new();
        for id in &own_ids {
            if let Some(sites) = pending_chunk_calls.remove(id) {
                for site in sites {
                    file_calls.push((id.clone(), site));
                }
            }
        }
        // Telemetry: count both the per-chunk `calls` rows and the file-level
        // `function_calls` call sites written in this tx (matches the pre-#1835
        // `total_calls` semantics, which summed both). Credited only on a
        // successful flush below.
        let calls_count: usize = file_calls.len()
            + function_calls
                .iter()
                .map(|fc| fc.calls.len())
                .sum::<usize>();
        let type_edge_count: usize = type_refs.iter().map(|t| t.type_refs.len()).sum();
        let live_refs: Vec<&str> = live_ids.iter().map(|s| s.as_str()).collect();
        match store.upsert_file_fused(
            &accum.real,
            &accum.sentinel,
            fp.mtime,
            &file_calls,
            origin.as_path(),
            &live_refs,
            function_calls,
            // v32 candidate_edges: empty until a later parser-emit lane produces
            // candidate sites. An empty slice clears the file's candidates.
            &[],
            fp,
        ) {
            Ok(_) => {
                // Type edges resolve against THIS file's chunks (by name+line),
                // which are now committed by the fused write above — so they
                // never silently skip a not-yet-committed target. The CUMULATIVE
                // set is re-supplied on every flush, and the per-chunk-id
                // DELETE-then-INSERT in `upsert_type_edges_one_file` is
                // idempotent — so an additive re-flush rebuilds the file's type
                // edges rather than dropping them. Best-effort: a failure here
                // only loses this file's type edges (re-resolved next reindex),
                // it does not roll back the coherent fused write.
                if !type_refs.is_empty() {
                    if let Err(e) =
                        store.upsert_type_edges_for_files(&[(origin.clone(), type_refs.to_vec())])
                    {
                        tracing::warn!(
                            file = %origin.display(),
                            error = %e,
                            "Failed to store type edges after fused write"
                        );
                        return (
                            accum.real.len() + accum.sentinel.len(),
                            calls_count,
                            0,
                            Some(live_ids),
                        );
                    }
                }
                (
                    accum.real.len() + accum.sentinel.len(),
                    calls_count,
                    type_edge_count,
                    Some(live_ids),
                )
            }
            Err(e) => {
                tracing::warn!(
                    file = %origin.display(),
                    error = %e,
                    "Fused per-file write failed; file left in its prior coherent state \
                     (chunks/calls/stamp all rolled back) — re-indexes next run"
                );
                (0, 0, 0, None)
            }
        }
    };

    for mut batch in embed_rx {
        if check_interrupted() {
            break;
        }

        total_cached += batch.cached_count;

        // Per-chunk `calls` ride the first batch of a parsed file-batch; buffer
        // them keyed by caller chunk id until their file flushes.
        for (caller_id, site) in std::mem::take(&mut batch.relationships.chunk_calls) {
            pending_chunk_calls.entry(caller_id).or_default().push(site);
        }

        // Files that ALREADY flushed this run but received NEW relationships this
        // batch (out-of-order: relationships rode a batch AFTER the file's
        // fingerprint+chunks). Their cumulative `flushed` copy grew but no new
        // chunks arrived, so the late-arrival chunk pass below won't pick them
        // up; a dedicated relationship-only re-flush writes the new edges.
        let mut rels_dirty: HashSet<PathBuf> = HashSet::new();

        // File-level function_calls also ride the first batch; buffer per origin
        // until the file completes (its fingerprint arrives in a later batch).
        // If the file ALREADY flushed this run (out-of-order: its fingerprint
        // preceded this relationship batch), merge into the cumulative `flushed`
        // copy instead — an additive re-flush below rewrites the call graph with
        // the complete set.
        for (file, fcs) in std::mem::take(&mut batch.relationships.function_calls) {
            match flushed.get_mut(&file) {
                Some(state) => {
                    state.function_calls.extend(fcs);
                    rels_dirty.insert(file);
                }
                None => accums.entry(file).or_default().function_calls.extend(fcs),
            }
        }

        // Type edges also ride the first batch; buffer per origin and write them
        // with the file's flush (after its chunks commit, so they always resolve).
        // Same already-flushed routing as function_calls.
        for (file, chunk_type_refs) in std::mem::take(&mut batch.relationships.type_refs) {
            match flushed.get_mut(&file) {
                Some(state) => {
                    state.type_refs.extend(chunk_type_refs);
                    rels_dirty.insert(file);
                }
                None => accums
                    .entry(file)
                    .or_default()
                    .type_refs
                    .extend(chunk_type_refs),
            }
        }

        // Accumulate this batch's chunks into their per-file buffers, routing
        // each to real vs sentinel by the same split `upsert_embedded_batch`
        // used: chunks past `cached_count` carry zero-vec sentinels only when
        // `uncached_need_embedding` is set (the `--llm-summaries` skip-first-pass
        // path); otherwise every chunk is a real embedding.
        let cached_slice_end = batch.cached_count.min(batch.chunk_embeddings.len());
        for (i, (chunk, embedding)) in batch.chunk_embeddings.into_iter().enumerate() {
            let accum = accums.entry(chunk.file.clone()).or_default();
            if i < cached_slice_end || !batch.uncached_need_embedding {
                accum.real.push((chunk, embedding));
            } else {
                accum.sentinel.push(chunk);
            }
        }

        // FLUSH completed files. A file's fingerprint rides the batch carrying
        // its LAST chunk, so (in the in-order production path) its presence here
        // means the file is COMPLETE. Track which origins flushed WITH chunks
        // this batch so the zero-chunk pass below skips a stray empty-set entry
        // for the same origin (which would otherwise prune the chunk we just
        // wrote).
        let mut flushed_with_chunks: HashSet<PathBuf> = HashSet::new();
        let empty_live: HashSet<String> = HashSet::new();
        for (file, fp) in std::mem::take(&mut batch.file_fingerprints) {
            match accums.remove(&file) {
                Some(accum) => {
                    flushed_with_chunks.insert(file.clone());
                    let (embedded, calls, type_edges, committed) = flush_file(
                        store,
                        &mut pending_chunk_calls,
                        &file,
                        &accum,
                        &accum.function_calls,
                        &accum.type_refs,
                        &fp,
                        &empty_live,
                    );
                    total_embedded += embedded;
                    total_calls += calls;
                    total_type_edges += type_edges;
                    // Record the flush so a late (out-of-order) batch for this
                    // file triggers an ADDITIVE re-flush instead of stranding its
                    // chunks — carrying forward the cumulative relationships so the
                    // re-flush rewrites (not wipes) the call graph + type edges.
                    // On a failed flush (`None`) record an empty live set with the
                    // fingerprint so a later additive flush still stamps.
                    let entry = flushed.entry(file.clone()).or_default();
                    entry.fp = fp.clone();
                    entry.function_calls = accum.function_calls;
                    entry.type_refs = accum.type_refs;
                    if let Some(ids) = committed {
                        entry.live_ids.extend(ids);
                    }
                }
                None => {
                    // A chunk-bearing fingerprint with no accumulated chunks: in
                    // the in-order path this is unexpected (the stamp rides the
                    // last-chunk batch). With out-of-order arrival it means the
                    // fingerprint preceded ALL of the file's chunks. Record the
                    // fingerprint with an empty live set so when the chunks arrive
                    // later the additive-flush pass writes + stamps them; the file
                    // is left unstamped for now (no chunks to stamp against yet).
                    flushed.entry(file.clone()).or_default().fp = fp.clone();
                    tracing::warn!(
                        file = %file.display(),
                        "Chunk-bearing fingerprint arrived with no accumulated chunks; \
                         deferring stamp until the file's chunks arrive"
                    );
                }
            }
        }

        // LATE-ARRIVAL additive flush: any file that was already flushed this run
        // but has since accumulated MORE chunks (its fingerprint arrived before
        // some of its chunks, out of order). Re-flush the new chunks additively —
        // the prune live set is the UNION of prior-committed ids and the new
        // chunks, so nothing the file produced is pruned away. This cannot fire
        // in the in-order production path (a flushed file's accum is empty); it
        // is the safety net the deterministic out-of-order test exercises.
        let late_files: Vec<PathBuf> = accums
            .iter()
            .filter(|(file, a)| {
                flushed.contains_key(*file) && (!a.real.is_empty() || !a.sentinel.is_empty())
            })
            .map(|(file, _)| file.clone())
            .collect();
        for file in late_files {
            let Some(accum) = accums.remove(&file) else {
                continue;
            };
            // Fold any relationships the late chunk batch carried into the
            // cumulative `flushed` copy (normally none — relationships route
            // straight to `flushed` once a file is flushed — but defensive), then
            // snapshot fp + cumulative live ids + relationships and flush
            // additively. Cumulative relationships are re-supplied so the call
            // graph is reconstructed, not wiped.
            let (fp, prior_ids, function_calls, type_refs) = {
                let Some(state) = flushed.get_mut(&file) else {
                    continue;
                };
                state
                    .function_calls
                    .extend(accum.function_calls.iter().cloned());
                state.type_refs.extend(accum.type_refs.iter().cloned());
                (
                    state.fp.clone(),
                    state.live_ids.clone(),
                    state.function_calls.clone(),
                    state.type_refs.clone(),
                )
            };
            flushed_with_chunks.insert(file.clone());
            let (embedded, calls, type_edges, committed) = flush_file(
                store,
                &mut pending_chunk_calls,
                &file,
                &accum,
                &function_calls,
                &type_refs,
                &fp,
                &prior_ids,
            );
            total_embedded += embedded;
            total_calls += calls;
            total_type_edges += type_edges;
            if let Some(ids) = committed {
                if let Some(entry) = flushed.get_mut(&file) {
                    entry.live_ids.extend(ids);
                }
            }
        }

        // RELATIONSHIP-ONLY re-flush: a file that already flushed this run and
        // received NEW relationships this batch but NO new chunks (relationships
        // rode a batch after the file's fingerprint+chunks). The late-arrival
        // chunk pass above already covers files that also got new chunks (it
        // re-supplies the cumulative relationships), so skip those here. The
        // accum is empty (no new chunks); `extra_live_ids` = the file's already-
        // committed ids so the prune KEEPS them (an empty accum + empty live set
        // would delete the whole file). The cumulative relationships are
        // re-supplied so the call graph + type edges are rewritten complete.
        let rels_only: Vec<PathBuf> = rels_dirty
            .into_iter()
            .filter(|f| !flushed_with_chunks.contains(f))
            .collect();
        let empty_accum = FileAccum::default();
        for file in rels_only {
            let (fp, prior_ids, function_calls, type_refs) = match flushed.get(&file) {
                Some(state) => (
                    state.fp.clone(),
                    state.live_ids.clone(),
                    state.function_calls.clone(),
                    state.type_refs.clone(),
                ),
                None => continue,
            };
            // No committed chunks yet (fingerprint-with-no-chunks deferral) → the
            // file isn't on disk; relationships have no caller chunks to wire to.
            // Defer until its chunks arrive (the chunk pass will re-supply rels).
            if prior_ids.is_empty() {
                continue;
            }
            let (_embedded, calls, type_edges, _committed) = flush_file(
                store,
                &mut pending_chunk_calls,
                &file,
                &empty_accum,
                &function_calls,
                &type_refs,
                &fp,
                &prior_ids,
            );
            total_calls += calls;
            total_type_edges += type_edges;
        }

        // FLUSH zero-chunk files (parsed to nothing this run): same fused
        // primitive with empty chunks + empty live set → clears chunks + FTS,
        // writes any function_calls (oversize-function class), stamps the
        // registry — all-or-nothing. Their function_calls may have been
        // accumulated from the first batch, so pull the accum if present.
        for (file, fp) in std::mem::take(&mut batch.empty_file_fingerprints) {
            // A file already flushed WITH chunks this batch must not be re-flushed
            // as zero-chunk — that would prune the chunks we just wrote (the
            // stray-empty-entry defense; empties never legitimately co-occur with
            // chunks for the same origin).
            if flushed_with_chunks.contains(&file) {
                continue;
            }
            // Pull any accumulated function_calls (oversize-function class rides
            // the first batch); a zero-chunk file carries no chunks, so any stray
            // chunk in the accum is dropped — empties never carry chunks.
            let accum = accums.remove(&file).unwrap_or_default();
            let zero_chunk = FileAccum::default();
            let (embedded, calls, type_edges, _committed) = flush_file(
                store,
                &mut pending_chunk_calls,
                &file,
                &zero_chunk,
                &accum.function_calls,
                &accum.type_refs,
                &fp,
                &empty_live,
            );
            total_embedded += embedded;
            total_calls += calls;
            total_type_edges += type_edges;
        }

        let parsed = parsed_count.load(Ordering::Relaxed);
        let embedded = embedded_count.load(Ordering::Relaxed);
        progress.set_position(parsed as u64);
        progress.set_message(format!(
            "parsed:{} embedded:{} written:{}",
            parsed, embedded, total_embedded
        ));
    }

    // Belt-and-suspenders: additively flush any residual accum for a file that
    // WAS flushed this run (its fingerprint arrived, then more of its chunks
    // arrived out of order in the final batch). The per-batch late pass handles
    // these as they arrive, so this normally finds nothing; it guarantees no
    // already-fingerprinted file's late chunks are stranded at shutdown.
    let residual_flushed: Vec<PathBuf> = accums
        .iter()
        .filter(|(file, a)| {
            flushed.contains_key(*file) && (!a.real.is_empty() || !a.sentinel.is_empty())
        })
        .map(|(file, _)| file.clone())
        .collect();
    for file in residual_flushed {
        let Some(accum) = accums.remove(&file) else {
            continue;
        };
        let (fp, prior_ids, function_calls, type_refs) = {
            let Some(state) = flushed.get_mut(&file) else {
                continue;
            };
            state
                .function_calls
                .extend(accum.function_calls.iter().cloned());
            state.type_refs.extend(accum.type_refs.iter().cloned());
            (
                state.fp.clone(),
                state.live_ids.clone(),
                state.function_calls.clone(),
                state.type_refs.clone(),
            )
        };
        let (embedded, calls, type_edges, committed) = flush_file(
            store,
            &mut pending_chunk_calls,
            &file,
            &accum,
            &function_calls,
            &type_refs,
            &fp,
            &prior_ids,
        );
        total_embedded += embedded;
        total_calls += calls;
        total_type_edges += type_edges;
        if let Some(ids) = committed {
            if let Some(entry) = flushed.get_mut(&file) {
                entry.live_ids.extend(ids);
            }
        }
    }

    // Any accum still holding chunks at end-of-stream is an INCOMPLETE file
    // (its fingerprint never arrived — interrupt or producer crash mid-stream).
    // Leave it UNWRITTEN: its prior state survives untouched and the next run
    // re-indexes it. Writing a half-accumulated file would be exactly the
    // half-state the fused write exists to prevent.
    let incomplete: Vec<&PathBuf> = accums
        .iter()
        .filter(|(_, a)| !a.real.is_empty() || !a.sentinel.is_empty())
        .map(|(f, _)| f)
        .collect();
    if !incomplete.is_empty() {
        tracing::warn!(
            files = incomplete.len(),
            "store_stage ended with incomplete files (no fingerprint); left unwritten, \
             they re-index next run"
        );
    }

    // Final flush: any per-chunk calls whose file never flushed (caller chunk
    // never arrived, or a cross-file straggler). FK-checked; only credit on
    // success (a single tx — an Err means zero rows landed).
    let leftover_calls: Vec<(String, cqs::parser::CallSite)> = pending_chunk_calls
        .into_iter()
        .flat_map(|(id, sites)| sites.into_iter().map(move |s| (id.clone(), s)))
        .collect();
    if !leftover_calls.is_empty() {
        let retained = flush_calls(store, leftover_calls);
        if !retained.is_empty() {
            tracing::warn!(
                count = retained.len(),
                "Some per-chunk calls had no committed caller chunk; dropped from the graph"
            );
        }
    }

    Ok((total_embedded, total_cached, total_type_edges, total_calls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    use super::super::types::RelationshipData;
    use cqs::store::{FileFingerprint, ModelInfo};

    fn chunk(file: &str, id_suffix: &str, body: &str) -> Chunk {
        Chunk {
            id: format!("{file}:1:{id_suffix}"),
            file: PathBuf::from(file),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: id_suffix.to_string(),
            signature: format!("pub fn {id_suffix}()"),
            content: body.to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// Like [`chunk`] but stamps a specific `parser_version` — needed to model
    /// PARSER_VERSION drift (a chunk parsed by an older version vs current).
    fn chunk_at_version(file: &str, id_suffix: &str, body: &str, parser_version: u32) -> Chunk {
        let mut c = chunk(file, id_suffix, body);
        c.parser_version = parser_version;
        c
    }

    fn full_fp(content: &[u8]) -> FileFingerprint {
        FileFingerprint {
            mtime: Some(123_456),
            size: Some(content.len() as u64),
            content_hash: Some(*blake3::hash(content).as_bytes()),
        }
    }

    fn embedded_batch(
        chunks: Vec<Chunk>,
        file_fingerprints: HashMap<PathBuf, FileFingerprint>,
        empty_file_fingerprints: HashMap<PathBuf, FileFingerprint>,
    ) -> EmbeddedBatch {
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        EmbeddedBatch {
            cached_count: 0,
            chunk_embeddings: chunks.into_iter().map(|c| (c, emb.clone())).collect(),
            relationships: RelationshipData::default(),
            file_fingerprints,
            empty_file_fingerprints,
            uncached_need_embedding: false,
        }
    }

    fn run_store_stage(store: &Store, batches: Vec<EmbeddedBatch>) {
        let (tx, rx) = unbounded::<EmbeddedBatch>();
        for b in batches {
            tx.send(b).unwrap();
        }
        drop(tx); // closing the channel = store_stage drains and returns
        let parsed = AtomicUsize::new(0);
        let embedded = AtomicUsize::new(0);
        store_stage(rx, store, &parsed, &embedded, &ProgressBar::hidden()).unwrap();
    }

    /// Build an `EmbeddedBatch` whose chunks ride with a `function_calls` set —
    /// one caller `name` with one callee — keyed on the same origin so the
    /// store stage's `upsert_function_calls_for_files` writes it.
    fn embedded_batch_with_calls(
        chunks: Vec<Chunk>,
        file_fingerprints: HashMap<PathBuf, FileFingerprint>,
        calls: HashMap<PathBuf, Vec<cqs::parser::FunctionCalls>>,
    ) -> EmbeddedBatch {
        let mut b = embedded_batch(chunks, file_fingerprints, HashMap::new());
        b.relationships.function_calls = calls;
        b
    }

    /// One caller → one callee `function_calls` set for `origin`.
    fn one_call(caller: &str, callee: &str) -> Vec<cqs::parser::FunctionCalls> {
        vec![cqs::parser::FunctionCalls {
            name: caller.to_string(),
            line_start: 1,
            calls: vec![cqs::parser::CallSite {
                callee_name: callee.to_string(),
                line_number: 1,
                kind: cqs::parser::CallEdgeKind::Call,
            }],
        }]
    }

    /// Run one raw SQL statement against the on-disk DB through a separate
    /// connection, bypassing the `Store` wrapper — used to drop `function_calls`
    /// out from under the store stage so its calls-replace returns Err.
    fn raw_exec(db_path: &std::path::Path, sql: &str) {
        use sqlx::sqlite::SqliteConnectOptions;
        use sqlx::ConnectOptions;
        let db_path = db_path.to_path_buf();
        let sql = sql.to_string();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut conn = SqliteConnectOptions::new()
                .filename(&db_path)
                .connect()
                .await
                .unwrap();
            sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .execute(&mut conn)
                .await
                .unwrap();
        });
    }

    /// #1835 fused-write guarantee: an INCOMPLETE file (its fingerprint never
    /// arrived — process died / interrupt between a straddling file's batches)
    /// leaves NOTHING on disk. The per-file fused write accumulates a file's
    /// chunks until its fingerprint (last-chunk signal) arrives, then writes the
    /// whole file in ONE tx. A first-half batch with no fingerprint is held in
    /// the accumulator and, when the channel closes without the fingerprint, the
    /// file is dropped UNWRITTEN — its prior state (here: empty) survives. This
    /// is STRONGER than the old per-batch behavior, which committed the half and
    /// relied on the unstamped fingerprint to re-trigger: now there is no
    /// half-state to leak at all.
    #[test]
    fn store_stage_partial_file_leaves_nothing_committed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Batch 1 of 2 for straddle.rs: chunks present, fingerprint WITHHELD
        // (it would have ridden the second, never-committed batch). Channel then
        // closes without the second batch — the post-crash on-disk state.
        let first_half = vec![
            chunk("straddle.rs", "aaaa", "fn a() {}"),
            chunk("straddle.rs", "bbbb", "fn b() {}"),
        ];
        run_store_stage(
            &store,
            vec![embedded_batch(first_half, HashMap::new(), HashMap::new())],
        );

        // NOTHING committed — the file never completed, so the fused write never
        // fired. No chunks, no fingerprint → the file re-indexes next run.
        assert_eq!(
            store.get_chunks_by_origin("straddle.rs").unwrap().len(),
            0,
            "an incomplete file must leave NO chunks (the fused write never fired)"
        );
        let fps = store.fingerprints_for_origins(&["straddle.rs"]).unwrap();
        assert!(
            !fps.contains_key("straddle.rs"),
            "an incomplete file must leave NO stamped fingerprint; got {:?}",
            fps.get("straddle.rs")
        );
    }

    /// File-complete stamping, happy path: the SECOND (last-chunk) batch
    /// carries the fingerprint, and after it commits the file is fully
    /// stamped — proving the stamp lands once both halves are written.
    #[test]
    fn store_stage_stamps_fingerprint_when_last_batch_lands() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let content = b"fn a() {}\nfn b() {}";
        let mut last_fp = HashMap::new();
        last_fp.insert(PathBuf::from("straddle.rs"), full_fp(content));

        run_store_stage(
            &store,
            vec![
                // First half: no fingerprint.
                embedded_batch(
                    vec![chunk("straddle.rs", "aaaa", "fn a() {}")],
                    HashMap::new(),
                    HashMap::new(),
                ),
                // Last half: carries the fingerprint.
                embedded_batch(
                    vec![chunk("straddle.rs", "bbbb", "fn b() {}")],
                    last_fp,
                    HashMap::new(),
                ),
            ],
        );

        let fps = store.fingerprints_for_origins(&["straddle.rs"]).unwrap();
        let fp = fps.get("straddle.rs").expect("origin exists");
        assert!(
            fp.content_hash.is_some() && fp.size.is_some() && fp.mtime.is_some(),
            "after the last batch the file must be fully stamped; got {fp:?}"
        );
    }

    /// Zero-chunk transition: a file previously indexed with chunks now parses
    /// to zero chunks. It rides the pipeline as an `empty_file_fingerprints`
    /// entry; `store_stage` must prune its stale chunks (empty live set) so
    /// they stop polluting search. Pins the correctness fix for the
    /// re-parse-forever / stale-results defect on the CLI path.
    #[test]
    fn store_stage_zero_chunk_file_prunes_old_chunks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Pre-seed gone.rs with chunks + a current fingerprint.
        let seed = vec![
            chunk("gone.rs", "1111", "fn one() {}"),
            chunk("gone.rs", "2222", "fn two() {}"),
        ];
        let mut seed_fp = HashMap::new();
        seed_fp.insert(PathBuf::from("gone.rs"), full_fp(b"seed"));
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        let seed_pairs: Vec<(Chunk, Embedding)> =
            seed.into_iter().map(|c| (c, emb.clone())).collect();
        store
            .upsert_embedded_batch(&seed_pairs, &[], &seed_fp)
            .unwrap();
        assert_eq!(
            store.get_chunks_by_origin("gone.rs").unwrap().len(),
            2,
            "seed chunks must be present before the zero-chunk reindex"
        );

        // Reindex run: gone.rs parses to ZERO chunks → rides the empty set.
        let mut empties = HashMap::new();
        empties.insert(PathBuf::from("gone.rs"), full_fp(b"now empty"));
        run_store_stage(
            &store,
            vec![embedded_batch(Vec::new(), HashMap::new(), empties)],
        );

        assert_eq!(
            store.get_chunks_by_origin("gone.rs").unwrap().len(),
            0,
            "zero-chunk file's stale chunks must be pruned"
        );

        // v29 #1774: the fingerprint must now persist in `file_registry`
        // even though no chunk rows remain — so the next `cqs index` pre-filter
        // skips the parse instead of re-parsing to zero chunks forever. The
        // staleness readers UNION the registry, so the origin resolves here.
        let fps = store.fingerprints_for_origins(&["gone.rs"]).unwrap();
        let fp = fps
            .get("gone.rs")
            .expect("zero-chunk origin must persist its fingerprint in file_registry");
        assert!(
            fp.content_hash.is_some() && fp.size.is_some() && fp.mtime.is_some(),
            "registry fingerprint must be fully populated for the zero-chunk file; got {fp:?}"
        );
    }

    /// A file that produced chunks this run must NOT be clobbered by a stray
    /// empty-set entry for the same origin (defensive: empties never carry
    /// chunks, but the post-loop pass must skip any origin with a real live
    /// set). The chunks survive.
    #[test]
    fn store_stage_chunked_file_not_pruned_by_stray_empty_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let mut fp = HashMap::new();
        fp.insert(PathBuf::from("live.rs"), full_fp(b"live"));
        let mut stray_empty = HashMap::new();
        stray_empty.insert(PathBuf::from("live.rs"), full_fp(b"live"));

        run_store_stage(
            &store,
            vec![embedded_batch(
                vec![chunk("live.rs", "cccc", "fn c() {}")],
                fp,
                stray_empty,
            )],
        );

        assert_eq!(
            store.get_chunks_by_origin("live.rs").unwrap().len(),
            1,
            "a file with a real live set must survive a stray empty entry"
        );
    }

    /// #1835 Finding 2 (MED-HIGH) — a ZERO-chunk file (the oversize-function
    /// class: parses to no chunks but HAS function_calls) whose calls replace
    /// FAILS must keep its OLD chunks and stay UNSTAMPED. The zero-chunk prune
    /// is unconditional in the success path; on a calls failure it must be
    /// FORFEITED so the file's old chunks survive as drift fuel — otherwise the
    /// file is skipped next run with stale calls (the same false-DEAD seal as
    /// Finding 1, via the zero-chunk route). Mirrors the watch path's
    /// prune-forfeit-on-calls-failure.
    #[test]
    fn store_stage_zero_chunk_calls_failure_forfeits_prune_and_stamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Pre-seed gone.rs with chunks + a current fingerprint (prior good run).
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        let mut seed_fp = HashMap::new();
        seed_fp.insert(PathBuf::from("gone.rs"), full_fp(b"seed"));
        store
            .upsert_embedded_batch(
                &[(chunk("gone.rs", "1111", "fn one() {}"), emb.clone())],
                &[],
                &seed_fp,
            )
            .unwrap();
        assert_eq!(
            store.get_chunks_by_origin("gone.rs").unwrap().len(),
            1,
            "precondition: seed chunk present"
        );

        // Reindex: gone.rs parses to ZERO chunks but carries a (refreshed) call
        // set (oversize-function class). Drop `function_calls` so the replace
        // fails, then run: the empty-set prune + registry stamp must be forfeited.
        raw_exec(&db_path, "DROP TABLE function_calls");
        let mut empties = HashMap::new();
        empties.insert(PathBuf::from("gone.rs"), full_fp(b"now empty"));
        let mut calls = HashMap::new();
        calls.insert(PathBuf::from("gone.rs"), one_call("oversize", "callee"));
        let batch = EmbeddedBatch {
            cached_count: 0,
            chunk_embeddings: Vec::new(),
            relationships: RelationshipData {
                function_calls: calls,
                ..RelationshipData::default()
            },
            file_fingerprints: HashMap::new(),
            empty_file_fingerprints: empties,
            uncached_need_embedding: false,
        };
        run_store_stage(&store, vec![batch]);

        // FINDING 2: old chunk NOT pruned (drift fuel survives) ...
        assert_eq!(
            store.get_chunks_by_origin("gone.rs").unwrap().len(),
            1,
            "a calls-write failure must forfeit the zero-chunk prune so old \
             chunks survive as drift fuel"
        );
        // ... and the registry is NOT re-stamped to the new content. The stored
        // fingerprint must still be the prior good run's (seed), not "now empty".
        let fps = store.fingerprints_for_origins(&["gone.rs"]).unwrap();
        let fp = fps.get("gone.rs").expect("origin exists from seed");
        assert_eq!(
            fp.size,
            Some(b"seed".len() as u64),
            "registry must NOT be re-stamped to the new (zero-chunk) content on \
             a calls failure; must stay the prior good fingerprint; got {fp:?}"
        );
    }

    /// Happy-path baseline for #1835: a chunk-bearing file with a function_calls
    /// set, calls write SUCCEEDS, so the fingerprint is stamped — proving the
    /// stamp still lands when chunks + calls are coherent. Without this the
    /// error-path test below could pass vacuously (a never-stamping pipeline).
    #[test]
    fn store_stage_single_batch_calls_success_stamps_fingerprint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let mut fp = HashMap::new();
        fp.insert(PathBuf::from("caller.rs"), full_fp(b"fn caller() {}"));
        let mut calls = HashMap::new();
        calls.insert(PathBuf::from("caller.rs"), one_call("caller", "victim"));

        run_store_stage(
            &store,
            vec![embedded_batch_with_calls(
                vec![chunk("caller.rs", "cccc", "fn caller() { victim(); }")],
                fp,
                calls,
            )],
        );

        // Calls landed and the fingerprint is fully stamped (chunk-row columns
        // + registry shadow) so the next pre-filter SKIPS the file.
        let fps = store.fingerprints_for_origins(&["caller.rs"]).unwrap();
        let stamped = fps.get("caller.rs").expect("origin exists");
        assert!(
            stamped.content_hash.is_some() && stamped.size.is_some() && stamped.mtime.is_some(),
            "coherent chunks+calls must stamp the fingerprint; got {stamped:?}"
        );
    }

    /// #1835 fused-write ERROR-PATH: when the per-file fused write fails (here:
    /// the in-tx `function_calls` write errors because the table was dropped),
    /// the WHOLE transaction rolls back — chunks, calls, function_calls, and the
    /// stamp all revert. The file is never sealed "current" with a stale call
    /// set (false-DEAD), and never left with chunks-without-calls either.
    ///
    /// Fresh-file case: with no prior rows, the rollback means nothing landed —
    /// zero chunks, no stamp — and the next run re-indexes from scratch. (The
    /// chunk-bearing drift case, where the rollback must preserve OLD chunks at
    /// the OLD parser_version, is covered by
    /// `store_stage_chunk_bearing_drift_calls_failure_re_arms_heal`; the
    /// orphan-impossible doctor check by
    /// `store_stage_fused_failure_leaves_no_orphan_and_doctor_clean`.)
    #[test]
    fn store_stage_single_batch_calls_failure_forfeits_stamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let mut fp = HashMap::new();
        fp.insert(PathBuf::from("caller.rs"), full_fp(b"fn caller() {}"));
        let mut calls = HashMap::new();
        calls.insert(PathBuf::from("caller.rs"), one_call("caller", "victim"));

        // Inject the calls-write failure: drop the table the replace writes to.
        raw_exec(&db_path, "DROP TABLE function_calls");

        run_store_stage(
            &store,
            vec![embedded_batch_with_calls(
                vec![chunk("caller.rs", "cccc", "fn caller() { victim(); }")],
                fp,
                calls,
            )],
        );

        // mechanism (b): calls failed → the fresh file's chunk upsert is
        // forfeited (no old rows to preserve), so nothing landed.
        assert_eq!(
            store.get_chunks_by_origin("caller.rs").unwrap().len(),
            0,
            "a calls-write failure forfeits the chunk upsert for a fresh file"
        );

        // ...and the fingerprint is NOT stamped — neither chunk-row columns nor
        // the registry shadow — so the file re-indexes next run.
        let fps = store.fingerprints_for_origins(&["caller.rs"]).unwrap();
        assert!(
            !fps.contains_key("caller.rs"),
            "a calls-write failure must leave NO stamped fingerprint; got {:?}",
            fps.get("caller.rs")
        );
    }

    /// #1835 Finding 1 (HIGH) — REPRODUCING test for the chunk-bearing
    /// unchanged-content PARSER_VERSION-drift class. This is the trust-v30
    /// magnet: a file re-indexed purely because its stored chunks carry an older
    /// parser_version (bytes unchanged). The chunk upsert advances
    /// `chunks.parser_version` to current; if that advance commits before the
    /// calls outcome is known, a calls failure leaves the file reading "current"
    /// (drift query no longer selects it; fingerprint matches disk) yet with
    /// STALE function_calls — skipped FOREVER (false-DEAD / ghost-caller).
    ///
    /// Setup: seed `drift.rs` CLEAN at parser_version N-1 (chunk + registry
    /// shadow + disk-matching fingerprint), confirm it's drift-selected. Then run
    /// store_stage re-indexing the SAME content at version N with a calls set,
    /// `function_calls` dropped so the replace fails. Post-fix assertion: the
    /// chunk advance is FORFEITED — `chunks.parser_version` stays N-1, so
    /// `origins_with_parser_drift` STILL selects it next run (the heal trigger
    /// re-arms). The OLD chunk and its call edge survive intact.
    ///
    /// Pre-fix (chunk advance commits before the calls outcome): the chunk's
    /// parser_version would be N, the drift query would NOT select it, and the
    /// file would be skipped with stale calls — the bug.
    #[test]
    fn store_stage_chunk_bearing_drift_calls_failure_re_arms_heal() {
        let current = cqs::parser_version();
        let stale = current - 1;

        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Seed CLEAN at version N-1: a chunk, a current registry/chunk-row
        // fingerprint, and a function_call edge — the prior successful run's
        // coherent state.
        let body = "fn drifty() { helper(); }";
        let mut seed_fp = HashMap::new();
        seed_fp.insert(PathBuf::from("drift.rs"), full_fp(body.as_bytes()));
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        store
            .upsert_embedded_batch(
                &[(chunk_at_version("drift.rs", "seed", body, stale), emb)],
                &[],
                &seed_fp,
            )
            .unwrap();
        // Seed the prior call edge directly so we can prove it survives.
        store
            .upsert_function_calls_for_files(&[(
                PathBuf::from("drift.rs"),
                one_call("drifty", "helper"),
            )])
            .unwrap();

        // Precondition: the seeded chunk registers as parser-version drifted.
        let drifted_before = store
            .origins_with_parser_drift(&["drift.rs"], current)
            .unwrap();
        assert!(
            drifted_before.contains("drift.rs"),
            "precondition: seeded chunk at N-1 must be drift-selected"
        );

        // Re-index the SAME content at version N, carrying a refreshed call set,
        // with `function_calls` dropped so the replace fails deterministically.
        raw_exec(&db_path, "DROP TABLE function_calls");
        let mut reidx_fp = HashMap::new();
        reidx_fp.insert(PathBuf::from("drift.rs"), full_fp(body.as_bytes()));
        let mut calls = HashMap::new();
        calls.insert(PathBuf::from("drift.rs"), one_call("drifty", "helper"));
        run_store_stage(
            &store,
            vec![embedded_batch_with_calls(
                vec![chunk_at_version("drift.rs", "seed", body, current)],
                reidx_fp,
                calls,
            )],
        );

        // HEAL TRIGGER RE-ARMED: the chunk advance was forfeited, so the stored
        // chunk is STILL at N-1 and the drift query STILL selects it. The drift
        // predicate keys on `chunks.parser_version` and its NOT EXISTS reads
        // `file_registry`, so the dropped `function_calls` table is irrelevant
        // to this query.
        let drifted_after = store
            .origins_with_parser_drift(&["drift.rs"], current)
            .unwrap();
        assert!(
            drifted_after.contains("drift.rs"),
            "FINDING 1: after a calls-write failure the chunk parser_version \
             advance MUST be forfeited so the file stays drift-selected and \
             re-indexes next run — else it is skipped forever with stale calls"
        );

        // The old chunk survives intact (one row, still at the seeded id).
        assert_eq!(
            store.get_chunks_by_origin("drift.rs").unwrap().len(),
            1,
            "the old chunk must survive the forfeited re-index (drift fuel)"
        );
    }

    /// #1835 Finding 1, STRADDLING seam: a drift file whose calls ride the FIRST
    /// embed batch but whose chunks span TWO batches. The file is accumulated
    /// across both batches and written in ONE fused transaction at completion;
    /// a function_calls failure inside that tx rolls the whole file back, so
    /// none of its old chunks advance → drift re-arms. Pins that the forfeit is
    /// whole-file (the fused write is all-or-nothing), not per-batch.
    #[test]
    fn store_stage_straddling_drift_calls_failure_forfeits_all_batches() {
        let current = cqs::parser_version();
        let stale = current - 1;

        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Seed two chunks at N-1 (the straddle file's prior state).
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        let mut seed_fp = HashMap::new();
        seed_fp.insert(PathBuf::from("straddle.rs"), full_fp(b"two fns"));
        store
            .upsert_embedded_batch(
                &[
                    (
                        chunk_at_version("straddle.rs", "aaaa", "fn a() {}", stale),
                        emb.clone(),
                    ),
                    (
                        chunk_at_version("straddle.rs", "bbbb", "fn b() {}", stale),
                        emb.clone(),
                    ),
                ],
                &[],
                &seed_fp,
            )
            .unwrap();
        assert!(store
            .origins_with_parser_drift(&["straddle.rs"], current)
            .unwrap()
            .contains("straddle.rs"));

        // Re-index at N across TWO batches: calls + first chunk in batch 1
        // (drop function_calls so the in-tx replace fails), second chunk in
        // batch 2 (fingerprint rides the last batch, triggering the flush). The
        // fused write at completion rolls back the whole file, so neither
        // chunk advances.
        raw_exec(&db_path, "DROP TABLE function_calls");
        let mut calls = HashMap::new();
        calls.insert(PathBuf::from("straddle.rs"), one_call("a", "b"));
        let mut last_fp = HashMap::new();
        last_fp.insert(PathBuf::from("straddle.rs"), full_fp(b"two fns"));
        run_store_stage(
            &store,
            vec![
                embedded_batch_with_calls(
                    vec![chunk_at_version(
                        "straddle.rs",
                        "aaaa",
                        "fn a() {}",
                        current,
                    )],
                    HashMap::new(),
                    calls,
                ),
                embedded_batch(
                    vec![chunk_at_version(
                        "straddle.rs",
                        "bbbb",
                        "fn b() {}",
                        current,
                    )],
                    last_fp,
                    HashMap::new(),
                ),
            ],
        );

        // Both old chunks survive at N-1 → drift re-arms (the file re-indexes
        // next run). If batch 2's chunk had advanced, the drift query could
        // still select it (one chunk at N-1 suffices), but the SECOND chunk
        // being advanced would corrupt the half-state; assert NEITHER advanced.
        assert!(
            store
                .origins_with_parser_drift(&["straddle.rs"], current)
                .unwrap()
                .contains("straddle.rs"),
            "FINDING 1 straddle: a calls failure must forfeit the chunk advance \
             across the file's whole straddle so drift re-arms"
        );
        assert_eq!(
            store.get_chunks_by_origin("straddle.rs").unwrap().len(),
            2,
            "both old chunks survive the forfeited straddle re-index"
        );
    }

    /// #1835 ORPHAN-IMPOSSIBLE (the structurally-missing test): a forced fused
    /// per-file write failure must leave NO orphan in EITHER direction —
    /// `find_orphaned_function_calls` returns EMPTY (no function_calls row for a
    /// file absent from both `chunks` and `file_registry`), and no
    /// chunks-without-calls. Because the fused write is one tx, a failure rolls
    /// back the function_calls write together with the chunk write, so the
    /// calls-without-chunks magnet the calls-before-chunks reorder created is now
    /// impossible. The file is also left coherent (prior state) and unstamped, so
    /// it re-indexes next run.
    ///
    /// Setup: seed `caller.rs` CLEAN (chunk + call edge + stamp). Re-index the
    /// same content with `function_calls` dropped so the in-tx write fails. After
    /// recreating the table, the doctor finds NO orphan and the old chunk
    /// survives — doctor-clean.
    #[test]
    fn store_stage_fused_failure_leaves_no_orphan_and_doctor_clean() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Prior good run: caller.rs has a chunk, a call edge, and a stamp.
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        let body = "fn caller() { victim(); }";
        let mut seed_fp = HashMap::new();
        seed_fp.insert(PathBuf::from("caller.rs"), full_fp(body.as_bytes()));
        store
            .upsert_embedded_batch(&[(chunk("caller.rs", "seed", body), emb)], &[], &seed_fp)
            .unwrap();
        store
            .upsert_function_calls_for_files(&[(
                PathBuf::from("caller.rs"),
                one_call("caller", "victim"),
            )])
            .unwrap();
        assert!(
            store.find_orphaned_function_calls().unwrap().is_empty(),
            "precondition: no orphan before the failed re-index"
        );

        // Re-index with function_calls dropped → the in-tx fused write fails →
        // the whole tx rolls back.
        raw_exec(&db_path, "DROP TABLE function_calls");
        let mut fp = HashMap::new();
        fp.insert(PathBuf::from("caller.rs"), full_fp(body.as_bytes()));
        let mut calls = HashMap::new();
        calls.insert(PathBuf::from("caller.rs"), one_call("caller", "victim"));
        run_store_stage(
            &store,
            vec![embedded_batch_with_calls(
                vec![chunk("caller.rs", "seed", body)],
                fp,
                calls,
            )],
        );

        // Recreate the table so the doctor query can run, then assert NO orphan
        // in either direction. The rollback reverted the new function_calls
        // write; the prior function_calls + chunk both survive coherently.
        raw_exec(
            &db_path,
            "CREATE TABLE function_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, \
             file TEXT NOT NULL, caller_name TEXT NOT NULL, caller_line INTEGER NOT NULL, \
             callee_name TEXT NOT NULL, call_line INTEGER NOT NULL, \
             edge_kind TEXT NOT NULL DEFAULT 'call')",
        );
        assert!(
            store.find_orphaned_function_calls().unwrap().is_empty(),
            "ORPHAN-IMPOSSIBLE: a fused-write failure must leave NO orphaned \
             function_calls (no calls-without-chunks) — doctor-clean"
        );
        // The prior chunk survives (the rollback reverted any prune), so there is
        // no chunks-without-calls orphan either.
        assert_eq!(
            store.get_chunks_by_origin("caller.rs").unwrap().len(),
            1,
            "the prior chunk survives the rolled-back fused write"
        );
    }

    /// #1835 fused straddle: a file whose chunks span TWO embed batches is held
    /// in the accumulator and written in ONE fused tx when its fingerprint
    /// (last-chunk signal) arrives in the second batch. After completion BOTH
    /// chunks are present and the file is stamped — proving accumulate→fused
    /// works across a straddle, not just single-batch files.
    #[test]
    fn store_stage_straddling_file_written_in_one_fused_tx_at_completion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let content = b"fn a() {}\nfn b() {}";
        let mut last_fp = HashMap::new();
        last_fp.insert(PathBuf::from("straddle.rs"), full_fp(content));

        run_store_stage(
            &store,
            vec![
                // Batch 1: first chunk, NO fingerprint → held in accumulator,
                // nothing written yet.
                embedded_batch(
                    vec![chunk("straddle.rs", "aaaa", "fn a() {}")],
                    HashMap::new(),
                    HashMap::new(),
                ),
                // Batch 2: last chunk + fingerprint → file COMPLETE → one fused
                // write commits both chunks + the stamp.
                embedded_batch(
                    vec![chunk("straddle.rs", "bbbb", "fn b() {}")],
                    last_fp,
                    HashMap::new(),
                ),
            ],
        );

        assert_eq!(
            store.get_chunks_by_origin("straddle.rs").unwrap().len(),
            2,
            "both straddling chunks must land in the single fused write at completion"
        );
        let fps = store.fingerprints_for_origins(&["straddle.rs"]).unwrap();
        let fp = fps.get("straddle.rs").expect("origin exists");
        assert!(
            fp.content_hash.is_some() && fp.size.is_some() && fp.mtime.is_some(),
            "the completed straddle must be fully stamped; got {fp:?}"
        );
    }

    /// #1835 type-edge resolution under per-file flush: a chunk's type edges
    /// ride the first batch but are written at the file's flush — AFTER its
    /// chunks commit — so they always resolve (the resolver maps name+line to a
    /// chunk id, which now exists). Pins that moving type edges from the old
    /// end-of-run deferred flush to per-file did not start silently dropping
    /// them.
    #[test]
    fn store_stage_type_edges_resolve_at_file_flush() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Chunk `user_fn` at line 1; its type edge references `Config`.
        let c = chunk("u.rs", "user_fn", "fn user_fn() {}");
        let mut fp = HashMap::new();
        fp.insert(PathBuf::from("u.rs"), full_fp(b"fn user_fn() {}"));
        let mut batch = embedded_batch(vec![c], fp, HashMap::new());
        batch.relationships.type_refs.insert(
            PathBuf::from("u.rs"),
            vec![cqs::parser::ChunkTypeRefs {
                name: "user_fn".to_string(),
                line_start: 1,
                type_refs: vec![cqs::parser::TypeRef {
                    type_name: "Config".to_string(),
                    line_number: 1,
                    kind: Some(cqs::parser::TypeEdgeKind::Param),
                }],
            }],
        );

        run_store_stage(&store, vec![batch]);

        // The type edge resolved (chunk committed in the same flush) → the type
        // graph reports user_fn as a user of Config.
        let users = store.get_type_users("Config", 10).unwrap();
        assert!(
            users.iter().any(|u| u.name == "user_fn"),
            "type edge must resolve at the file's fused flush; got {:?}",
            users.iter().map(|u| &u.name).collect::<Vec<_>>()
        );
    }

    /// THE HEADLINE REGRESSION TEST. A file straddling two embed batches whose
    /// fingerprint-bearing batch (the file's "last" chunk) arrives at
    /// `store_stage` BEFORE the file's earlier-batch chunks — exactly the effect
    /// of the GPU/CPU `parse_rx` work-steal race when a file's halves are
    /// processed by stages running at different speeds. EVERY one of the file's
    /// chunks must end up in the store: ZERO loss.
    ///
    /// Pre-fix (revert both the parser file-alignment AND the `store_stage`
    /// additive-reflush net): the fingerprint flush prunes the file to the
    /// partial live set {c} the moment it arrives, then [a, b] land in a fresh
    /// accumulator that never flushes (its fingerprint was already consumed) and
    /// are silently dropped at end-of-stream — only `c` survives. That is the
    /// confirmed, non-deterministic full-corpus chunk loss, made deterministic
    /// here by feeding the batches in the adversarial order directly.
    #[test]
    fn store_stage_out_of_order_fingerprint_before_chunks_loses_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let content = b"fn a() {}\nfn b() {}\nfn c() {}";
        let mut last_fp = HashMap::new();
        last_fp.insert(PathBuf::from("straddle.rs"), full_fp(content));

        // ADVERSARIAL ORDER: the fingerprint-bearing batch (the file's last
        // chunk `c`) arrives FIRST, before the earlier chunks `a` and `b`.
        run_store_stage(
            &store,
            vec![
                embedded_batch(
                    vec![chunk("straddle.rs", "cccc", "fn c() {}")],
                    last_fp,
                    HashMap::new(),
                ),
                embedded_batch(
                    vec![
                        chunk("straddle.rs", "aaaa", "fn a() {}"),
                        chunk("straddle.rs", "bbbb", "fn b() {}"),
                    ],
                    HashMap::new(),
                    HashMap::new(),
                ),
            ],
        );

        // All three chunks present — none pruned, none stranded.
        let stored = store.get_chunks_by_origin("straddle.rs").unwrap();
        assert_eq!(
            stored.len(),
            3,
            "out-of-order arrival must lose NO chunks; got {:?}",
            stored.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        let ids: HashSet<String> = stored.iter().map(|c| c.id.clone()).collect();
        for suffix in ["aaaa", "bbbb", "cccc"] {
            assert!(
                ids.contains(&format!("straddle.rs:1:{suffix}")),
                "chunk {suffix} missing from the store after out-of-order arrival; got {ids:?}"
            );
        }

        // And the file is stamped (its fingerprint landed with the flush).
        let fps = store.fingerprints_for_origins(&["straddle.rs"]).unwrap();
        assert!(
            fps.get("straddle.rs")
                .is_some_and(|fp| fp.content_hash.is_some()),
            "the completed file must be stamped after the additive flush"
        );
    }

    /// Extreme out-of-order: the fingerprint arrives in a batch with NONE of the
    /// file's chunks (all of them follow in a LATER batch). The chunk-bearing
    /// fingerprint's empty-accum arm defers the stamp; the later chunks then
    /// trigger the additive flush that writes + stamps them. ZERO loss.
    #[test]
    fn store_stage_out_of_order_fingerprint_with_no_chunks_then_chunks_later() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let content = b"fn a() {}\nfn b() {}";
        let mut fp_only = HashMap::new();
        fp_only.insert(PathBuf::from("late.rs"), full_fp(content));

        run_store_stage(
            &store,
            vec![
                // Fingerprint, but no chunks for late.rs in this batch.
                embedded_batch(Vec::new(), fp_only, HashMap::new()),
                // The file's chunks arrive afterward.
                embedded_batch(
                    vec![
                        chunk("late.rs", "aaaa", "fn a() {}"),
                        chunk("late.rs", "bbbb", "fn b() {}"),
                    ],
                    HashMap::new(),
                    HashMap::new(),
                ),
            ],
        );

        let stored = store.get_chunks_by_origin("late.rs").unwrap();
        assert_eq!(
            stored.len(),
            2,
            "fingerprint-before-all-chunks must still write every chunk; got {:?}",
            stored.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        let fps = store.fingerprints_for_origins(&["late.rs"]).unwrap();
        assert!(
            fps.get("late.rs")
                .is_some_and(|fp| fp.content_hash.is_some()),
            "the file must be stamped once its chunks arrive and flush"
        );
    }

    /// Out-of-order arrival must NOT clobber a prior run's chunks for a DIFFERENT
    /// file, nor leave the re-indexed file's prior chunks behind. Seed the file
    /// with stale chunks, then re-index it out-of-order; the prune live set is
    /// the UNION of all this-run chunks, so the stale chunk is removed and all
    /// new chunks survive (no over-prune, no under-prune).
    #[test]
    fn store_stage_out_of_order_prunes_stale_keeps_all_new() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Seed two STALE chunks (different ids than the new run will produce).
        let emb = Embedding::new(vec![0.25_f32; cqs::EMBEDDING_DIM]);
        let mut seed_fp = HashMap::new();
        seed_fp.insert(PathBuf::from("reidx.rs"), full_fp(b"old"));
        store
            .upsert_embedded_batch(
                &[
                    (chunk("reidx.rs", "old1", "fn old1() {}"), emb.clone()),
                    (chunk("reidx.rs", "old2", "fn old2() {}"), emb.clone()),
                ],
                &[],
                &seed_fp,
            )
            .unwrap();
        assert_eq!(store.get_chunks_by_origin("reidx.rs").unwrap().len(), 2);

        // Re-index out-of-order with three NEW chunks; fingerprint rides the
        // batch with the "last" new chunk, which arrives first.
        let content = b"fn n1() {}\nfn n2() {}\nfn n3() {}";
        let mut new_fp = HashMap::new();
        new_fp.insert(PathBuf::from("reidx.rs"), full_fp(content));
        run_store_stage(
            &store,
            vec![
                embedded_batch(
                    vec![chunk("reidx.rs", "new3", "fn n3() {}")],
                    new_fp,
                    HashMap::new(),
                ),
                embedded_batch(
                    vec![
                        chunk("reidx.rs", "new1", "fn n1() {}"),
                        chunk("reidx.rs", "new2", "fn n2() {}"),
                    ],
                    HashMap::new(),
                    HashMap::new(),
                ),
            ],
        );

        let ids: HashSet<String> = store
            .get_chunks_by_origin("reidx.rs")
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        // All three new chunks present.
        for suffix in ["new1", "new2", "new3"] {
            assert!(
                ids.contains(&format!("reidx.rs:1:{suffix}")),
                "new chunk {suffix} missing after out-of-order reindex; got {ids:?}"
            );
        }
        // Neither stale chunk survives (the union prune removed prior-run rows).
        for suffix in ["old1", "old2"] {
            assert!(
                !ids.contains(&format!("reidx.rs:1:{suffix}")),
                "stale chunk {suffix} must be pruned by the out-of-order reindex; got {ids:?}"
            );
        }
        assert_eq!(
            ids.len(),
            3,
            "exactly the three new chunks remain; got {ids:?}"
        );
    }

    /// Build an `EmbeddedBatch` carrying a type-edge set for `file` — chunk
    /// `caller` (line 1) references type `Config`. Mirrors the real pipeline,
    /// where type_refs ride the first parse batch.
    fn one_type_ref() -> Vec<cqs::parser::ChunkTypeRefs> {
        vec![cqs::parser::ChunkTypeRefs {
            name: "caller".to_string(),
            line_start: 1,
            type_refs: vec![cqs::parser::TypeRef {
                type_name: "Config".to_string(),
                line_number: 1,
                kind: Some(cqs::parser::TypeEdgeKind::Param),
            }],
        }]
    }

    /// THE EDGE-SURVIVAL REGRESSION TEST (relationships-with-fingerprint, chunks
    /// late). Out-of-order arrival must preserve the file's CALL GRAPH and TYPE
    /// EDGES, not just its chunks. The relationships ride the FIRST batch (with
    /// the fingerprint); the caller chunk arrives LATER. The additive re-flush
    /// for the late chunk must re-supply the cumulative relationships so the
    /// call-graph write (a wholesale DELETE-then-INSERT) rebuilds the edge rather
    /// than wiping it.
    ///
    /// Pre-fix (additive re-flush supplies an EMPTY relationship set): the
    /// re-flush's `DELETE FROM function_calls WHERE file = ?` wipes the
    /// `caller→victim` edge the first flush wrote and inserts nothing — chunks
    /// survive (union-prune) but `get_callers_full("victim")` returns `[]`. The
    /// load-bearing assertion is EDGE SURVIVAL; the no-orphan check is kept as a
    /// secondary assertion (it is vacuously satisfied by a fully-deleted edge
    /// set, which is exactly why it alone was an insufficient guard).
    #[test]
    fn store_stage_out_of_order_preserves_call_graph() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let content = b"fn caller() { victim(); }\nfn other() {}";
        let mut last_fp = HashMap::new();
        last_fp.insert(PathBuf::from("calls.rs"), full_fp(content));

        // Fingerprint-bearing batch carries the file's relationships (call edge
        // caller→victim + a type edge caller→Config) and chunk `other`; the
        // `caller` chunk arrives in the LATER batch.
        let mut first = embedded_batch(
            vec![chunk("calls.rs", "other", "fn other() {}")],
            last_fp,
            HashMap::new(),
        );
        first
            .relationships
            .function_calls
            .insert(PathBuf::from("calls.rs"), one_call("caller", "victim"));
        first
            .relationships
            .type_refs
            .insert(PathBuf::from("calls.rs"), one_type_ref());

        run_store_stage(
            &store,
            vec![
                first,
                embedded_batch(
                    vec![chunk("calls.rs", "caller", "fn caller() { victim(); }")],
                    HashMap::new(),
                    HashMap::new(),
                ),
            ],
        );

        // Both chunks present.
        assert_eq!(
            store.get_chunks_by_origin("calls.rs").unwrap().len(),
            2,
            "out-of-order arrival with relationships must keep all chunks"
        );
        // LOAD-BEARING: the call edge survives the additive re-flush.
        let callers = store.get_callers_full("victim").unwrap();
        assert!(
            callers.iter().any(|c| c.name == "caller"),
            "the caller→victim call edge must survive the out-of-order re-flush; got {:?}",
            callers.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        // LOAD-BEARING: the type edge survives too.
        let users = store.get_type_users("Config", 10).unwrap();
        assert!(
            users.iter().any(|u| u.name == "caller"),
            "the caller→Config type edge must survive the out-of-order re-flush; got {:?}",
            users.iter().map(|u| &u.name).collect::<Vec<_>>()
        );
        // Secondary: no calls-without-chunks orphan (vacuous if the edge set were
        // wiped — kept only as a complement to the edge-survival checks above).
        assert!(
            store.find_orphaned_function_calls().unwrap().is_empty(),
            "out-of-order arrival must not leave an orphaned function_calls row"
        );
    }

    /// EDGE-SURVIVAL, the OTHER ordering (fingerprint+chunks first, relationships
    /// LATE). The relationship batch arrives AFTER the file already flushed, so
    /// it lands in the cumulative `flushed` copy and must trigger a
    /// relationship-only re-flush that writes the edge against the now-present
    /// caller chunk — without re-pruning the file's already-committed chunks.
    #[test]
    fn store_stage_out_of_order_relationships_after_flush_write_edge() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        let content = b"fn caller() { victim(); }";
        let mut fp = HashMap::new();
        fp.insert(PathBuf::from("calls.rs"), full_fp(content));

        // Batch 1: fingerprint + the caller chunk, NO relationships (the file
        // flushes complete-on-chunks). Batch 2: the relationships arrive late.
        let mut rel_batch = embedded_batch(Vec::new(), HashMap::new(), HashMap::new());
        rel_batch
            .relationships
            .function_calls
            .insert(PathBuf::from("calls.rs"), one_call("caller", "victim"));
        rel_batch
            .relationships
            .type_refs
            .insert(PathBuf::from("calls.rs"), one_type_ref());

        run_store_stage(
            &store,
            vec![
                embedded_batch(
                    vec![chunk("calls.rs", "caller", "fn caller() { victim(); }")],
                    fp,
                    HashMap::new(),
                ),
                rel_batch,
            ],
        );

        // The chunk survived (not re-pruned by the relationship-only re-flush).
        assert_eq!(
            store.get_chunks_by_origin("calls.rs").unwrap().len(),
            1,
            "the relationship-only re-flush must not prune the file's chunks"
        );
        // The late edge was written.
        let callers = store.get_callers_full("victim").unwrap();
        assert!(
            callers.iter().any(|c| c.name == "caller"),
            "a late relationship batch must write its call edge; got {:?}",
            callers.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        let users = store.get_type_users("Config", 10).unwrap();
        assert!(
            users.iter().any(|u| u.name == "caller"),
            "a late relationship batch must write its type edge; got {:?}",
            users.iter().map(|u| &u.name).collect::<Vec<_>>()
        );
    }
}
