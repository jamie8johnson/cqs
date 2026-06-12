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
/// Overridable via `CQS_DEFERRED_FLUSH_BATCHES` env var (the value is a
/// batch count, not a duration).
fn deferred_flush_interval() -> usize {
    std::env::var("CQS_DEFERRED_FLUSH_BATCHES")
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
    // Files that survived the staleness pre-filter but parsed to zero chunks
    // this run: their stale chunks must be pruned (empty live set) so they
    // don't survive forever and re-classify the file STALE every run. Stored
    // separately from `live_ids_per_file` so a file that DID produce chunks in
    // one batch isn't clobbered into an empty live set by a stray empty entry.
    let mut empty_file_fingerprints: HashMap<PathBuf, cqs::store::FileFingerprint> = HashMap::new();
    let mut batch_counter: usize = 0;
    let flush_interval = deferred_flush_interval();

    for batch in embed_rx {
        if check_interrupted() {
            break;
        }

        // Stash zero-chunk files for the post-loop prune. A later batch may
        // still carry chunks for the same origin (a file straddling batches
        // where only a later half is empty cannot happen — empties have no
        // chunks anywhere — but guard anyway): the post-loop pass skips any
        // origin that also appears in `live_ids_per_file`.
        for (file, fp) in batch.empty_file_fingerprints {
            empty_file_fingerprints.entry(file).or_insert(fp);
        }

        // Use pre-extracted chunk calls from the parse stage (rayon parallel)
        // instead of re-parsing each chunk sequentially here.
        // Defer chunk_calls — they reference caller_id with FK on chunks(id),
        // and chunks from later batches aren't in the DB yet.
        deferred_chunk_calls.extend(batch.relationships.chunk_calls);

        let batch_count = batch.chunk_embeddings.len();

        // Upsert chunks WITHOUT calls (calls are deferred). Also accumulate
        // per-file live IDs for the post-loop prune pass.
        //
        // When `uncached_need_embedding` is set, the chunks past index
        // `cached_count` carry zero-vec sentinels (skip-first-pass path
        // under `--llm-summaries`). Cached chunks still carry real
        // embeddings (from the global cache). Slice the batch and route
        // each half to the correct insert mode so cached chunks land at
        // `needs_embedding=0` while sentinel chunks land at
        // `needs_embedding=1`.
        //
        // The whole batch — real chunks, sentinel chunks, and the per-file
        // fingerprint stamps — writes in ONE transaction
        // (`upsert_embedded_batch`), not one transaction per file: per-file
        // granularity existed only because the old upsert API took a single
        // source_mtime, and it cost a BEGIN/COMMIT + content-hash snapshot
        // SELECT per file. Chunk-level atomicity is preserved: a crash may
        // lose whole uncommitted batches, but chunks and their FTS rows
        // always commit together. (The watch path keeps its own per-file
        // fused tx — see `upsert_chunks_calls_and_prune` — because a daemon
        // tick must commit chunks + calls + function_calls + prune per file
        // as one unit.)
        let cached_slice_end = batch.cached_count.min(batch.chunk_embeddings.len());
        let mut real_pairs: Vec<(Chunk, Embedding)> = Vec::new();
        let mut sentinel_chunks: Vec<Chunk> = Vec::new();
        for (i, (chunk, embedding)) in batch.chunk_embeddings.into_iter().enumerate() {
            live_ids_per_file
                .entry(chunk.file.clone())
                .or_default()
                .insert(chunk.id.clone());
            if i < cached_slice_end || !batch.uncached_need_embedding {
                real_pairs.push((chunk, embedding));
            } else {
                // Past cached_count and skip-first-pass mode is on — chunk
                // carries a zero-vec sentinel; route to the unembedded mode.
                sentinel_chunks.push(chunk);
            }
        }
        store.upsert_embedded_batch(&real_pairs, &sentinel_chunks, &batch.file_fingerprints)?;

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
    //
    // One transaction for the whole sweep — the per-file variant opened a
    // BEGIN/COMMIT per origin, thousands of round-trips of pure overhead
    // on a full reindex.
    //
    // Zero-chunk files (survived the pre-filter, parsed to nothing this run)
    // are pruned with an EMPTY live set: `delete_phantom_chunks_batch` deletes
    // every chunk AND every function_calls row for an origin whose live set is
    // empty (the whole-file-emptied end-state). Without this their old chunks
    // survive the prune (the loop above never saw them, so they have no
    // `live_ids_per_file` entry) and keep returning stale search hits forever;
    // and — because a zero-chunk file no longer rides in
    // `relationships.function_calls` (parsing.rs only stashes NON-empty call
    // sets, so `upsert_function_calls_for_files` never DELETE-then-INSERTs the
    // file's now-empty set) — their old call edges would orphan, surfacing as
    // ghost `cqs callers` rows and vetoing otherwise-dead callees. The
    // empty-live-set branch of the prune clears both. Skip any origin that DID
    // produce chunks this run — that file is handled by its real live set, and
    // its function_calls were DELETE-then-INSERTed above.
    //
    // v29 #1774: the fingerprint no longer lives ONLY on chunk rows — the
    // `file_registry` table persists it for zero-chunk origins. We prune the
    // file's stale chunks below, then stamp its fingerprint into the registry
    // (after the prune block) so the next run's pre-filter sees a stored
    // fingerprint and SKIPS the parse entirely instead of re-parsing to zero
    // chunks every run. That re-parse was cheap but not free; the registry
    // closes it.
    let mut prune_entries: Vec<(&std::path::Path, Vec<&str>)> = live_ids_per_file
        .iter()
        .map(|(file, live_ids)| {
            (
                file.as_path(),
                live_ids.iter().map(|s| s.as_str()).collect::<Vec<&str>>(),
            )
        })
        .collect();
    for file in empty_file_fingerprints.keys() {
        if !live_ids_per_file.contains_key(file) {
            prune_entries.push((file.as_path(), Vec::new()));
        }
    }
    match store.delete_phantom_chunks_batch(&prune_entries) {
        Ok(deleted) if deleted > 0 => {
            tracing::info!(
                count = deleted,
                "Pruned phantom chunks from prior chunker versions"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                files = prune_entries.len(),
                error = %e,
                "Batched phantom prune failed; orphan rows from prior chunker versions may persist"
            );
        }
    }

    // v29 #1774: persist the reconcile fingerprint for files that parsed to
    // ZERO chunks this run into `file_registry`. A file that produced chunks in
    // some batch is excluded — its fingerprint already rode the chunk-write
    // transaction via `upsert_embedded_batch`. Only the genuinely zero-chunk
    // survivors need the registry stamp, so the next `cqs index` pre-filter
    // sees a stored fingerprint and skips the parse instead of re-parsing to
    // zero chunks forever. Best-effort: a failure here only forfeits the skip
    // (the file re-parses next run, idempotently), so it warns rather than
    // aborting the index.
    let registry_entries: Vec<(std::path::PathBuf, cqs::store::FileFingerprint)> =
        empty_file_fingerprints
            .into_iter()
            .filter(|(file, _)| !live_ids_per_file.contains_key(file))
            .collect();
    if !registry_entries.is_empty() {
        match store.set_file_registry_fingerprints_batch(&registry_entries) {
            Ok(stamped) => tracing::debug!(
                stamped,
                "Stamped file_registry fingerprints for zero-chunk files"
            ),
            Err(e) => tracing::warn!(
                files = registry_entries.len(),
                error = %e,
                "Failed to stamp file_registry for zero-chunk files; they will re-parse next run"
            ),
        }
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
            content_hash: blake3::hash(body.as_bytes()).to_hex().to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
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

    /// Simulated crash between a straddling file's two batch commits. The
    /// pipeline stamps a file's fingerprint only in the batch carrying its
    /// LAST chunk; if the process dies after committing the file's first
    /// (chunk-only, no-fingerprint) batch but before the second, the file is
    /// half-indexed and UNSTAMPED. The staleness pre-filter consumes these
    /// fingerprints (NULL columns degrade to "not current"), so the file MUST
    /// classify stale on the next run rather than be skipped permanently.
    ///
    /// Drives `store_stage` with only the first batch (no fingerprint) and
    /// then closes the channel — exactly the post-crash on-disk state.
    #[test]
    fn store_stage_partial_file_leaves_fingerprint_unstamped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        store.init(&ModelInfo::default()).unwrap();

        // Batch 1 of 2 for straddle.rs: chunks present, fingerprint WITHHELD
        // (it would have ridden the second, never-committed batch).
        let first_half = vec![
            chunk("straddle.rs", "aaaa", "fn a() {}"),
            chunk("straddle.rs", "bbbb", "fn b() {}"),
        ];
        run_store_stage(
            &store,
            vec![embedded_batch(first_half, HashMap::new(), HashMap::new())],
        );

        // Chunks landed...
        assert_eq!(
            store.get_chunks_by_origin("straddle.rs").unwrap().len(),
            2,
            "first-half chunks must be committed"
        );

        // ...but the reconcile fingerprint the staleness pre-filter reads is
        // unpopulated, so the file is NOT marked current.
        let fps = store.fingerprints_for_origins(&["straddle.rs"]).unwrap();
        let fp = fps
            .get("straddle.rs")
            .expect("origin row exists for the committed chunks");
        assert!(
            fp.content_hash.is_none() && fp.size.is_none(),
            "a half-indexed file must stay unstamped so it re-indexes; got {fp:?}"
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
}
