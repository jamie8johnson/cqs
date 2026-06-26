//! Stage 1: Parse files in parallel batches, filter by staleness, send to embedder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::Sender;
use rayon::prelude::*;

use cqs::store::{FileFingerprint, FingerprintPolicy};
use cqs::{normalize_path, Parser as CqParser, Store};

use super::types::{embed_batch_size_for, file_batch_size, ParsedBatch, RelationshipData};
use crate::cli::check_interrupted;

/// Context struct for parser_stage to avoid too_many_arguments.
pub(super) struct ParserStageContext {
    pub root: PathBuf,
    pub force: bool,
    pub parser: Arc<CqParser>,
    pub store: Arc<Store>,
    pub parsed_count: Arc<AtomicUsize>,
    pub parse_errors: Arc<AtomicUsize>,
    /// Model config so the per-batch send loop can pick a dim/seq-scaled batch
    /// size. At batch=64 nomic-coderank (768 dim, 2048 seq) OOMs an 8 GB GPU;
    /// the model-aware helper drops it to 16.
    pub model_config: cqs::embedder::ModelConfig,
}

/// Read a full disk fingerprint (mtime + size + BLAKE3) for a file that is
/// about to be (re)indexed. `FingerprintPolicy::HashOnly` forces the hash
/// read regardless of stored state, so every reindexed file leaves fully
/// populated fingerprint columns behind. Returns `Default` (all `None`)
/// when `metadata()` fails — the parser surfaces the real I/O error.
fn full_disk_fingerprint(abs_path: &std::path::Path) -> FileFingerprint {
    FileFingerprint::read_disk(
        abs_path,
        &FileFingerprint::default(),
        FingerprintPolicy::HashOnly,
    )
    .unwrap_or_default()
}

/// How many leading chunks of `chunks` form a FILE-ALIGNED batch under the soft
/// cap `batch_size`.
///
/// `chunks` must have each file's chunks in a contiguous run (the parser's rayon
/// fold/reduce guarantees this). The returned `take` is a sum of whole
/// contiguous file-runs from the front: it never splits a file's run across the
/// boundary. The first file's run is ALWAYS included, even when that one file
/// alone exceeds `batch_size` (so an oversize file rides as its own batch rather
/// than being split — splitting is exactly the cross-stage race this prevents).
/// Subsequent file-runs are added only while the running total stays at or below
/// `batch_size`.
///
/// Invariants (held for any non-empty `chunks`, any `batch_size >= 1`):
///   * `1 <= take <= chunks.len()`
///   * `chunks[take - 1].file != chunks[take].file` when `take < chunks.len()`
///     (the cut lands on a file boundary)
fn file_aligned_take(chunks: &[cqs::Chunk], batch_size: usize) -> usize {
    debug_assert!(
        !chunks.is_empty(),
        "file_aligned_take needs a non-empty slice"
    );
    let mut take = 0usize;
    while take < chunks.len() {
        // Extent of the contiguous run for the file at `take`.
        let run_file = &chunks[take].file;
        let run_len = chunks[take..]
            .iter()
            .take_while(|c| &c.file == run_file)
            .count();
        // Always include the first run; stop before adding a later run that
        // would push the batch over the soft cap.
        if take > 0 && take + run_len > batch_size {
            break;
        }
        take += run_len;
        // The first run is committed; once we're at/over the cap, stop adding
        // more files (the next run starts a fresh batch).
        if take >= batch_size {
            break;
        }
    }
    take
}

/// Result of the per-batch staleness pre-filter: which files to parse, and
/// the disk fingerprint for each of them.
struct StalenessFilterResult {
    survivors: Vec<PathBuf>,
    fingerprints: HashMap<PathBuf, FileFingerprint>,
}

/// Decide which files in `file_batch` actually need reindexing, BEFORE any
/// parsing happens.
///
/// One batched `fingerprints_for_origins` SELECT per file batch replaces
/// the old per-file `needs_reindex` round-trip (one stat + one SELECT per
/// file), and skipped files are never parsed at all — previously every file
/// was fully tree-sitter parsed and the staleness filter ran on the parsed
/// chunks, making a no-op incremental index O(corpus) parses. Same batched
/// shape as the watch daemon's reconcile pre-filter.
///
/// Comparison uses `FingerprintPolicy::MtimeOrHash`: mtime+size fast path,
/// BLAKE3 tiebreak when they disagree. Rows whose fingerprint columns are
/// still NULL (pre-v23 rows, low-level upserts) degrade to mtime equality.
///
/// When the tiebreak proves a mtime-bumped file content-identical
/// (`git checkout`, formatter no-op), the stored fingerprint is refreshed
/// in one batched write so neither the next `cqs index` nor the daemon
/// reconcile has to re-hash the file.
///
/// `force` bypasses the filter entirely (every file reindexes) but still
/// reads full fingerprints so the upsert stamps them.
fn filter_stale_files(
    store: &Store,
    root: &std::path::Path,
    file_batch: &[PathBuf],
    force: bool,
) -> StalenessFilterResult {
    let _span = tracing::debug_span!("filter_stale_files", files = file_batch.len()).entered();
    let mut fingerprints: HashMap<PathBuf, FileFingerprint> =
        HashMap::with_capacity(file_batch.len());

    if force {
        for rel in file_batch {
            fingerprints.insert(rel.clone(), full_disk_fingerprint(&root.join(rel)));
        }
        return StalenessFilterResult {
            survivors: file_batch.to_vec(),
            fingerprints,
        };
    }

    let origins: Vec<String> = file_batch.iter().map(|p| normalize_path(p)).collect();
    let origin_refs: Vec<&str> = origins.iter().map(|s| s.as_str()).collect();
    let stored_map = match store.fingerprints_for_origins(&origin_refs) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Batched fingerprint lookup failed; reindexing whole batch");
            HashMap::new()
        }
    };

    // PARSER_VERSION drift makes a file stale even when its bytes are
    // unchanged: the stored chunks (and their derived call edges / edge_kind
    // / doc enrichment) were extracted by an older parser. Selecting by
    // fingerprint alone would never re-tag these without `--force`. The query
    // is one batched SELECT; on failure we degrade to fingerprint-only
    // (drift simply isn't healed this pass, same as before this fix).
    let drifted = match store.origins_with_parser_drift(&origin_refs, cqs::parser_version()) {
        Ok(set) => set,
        Err(e) => {
            tracing::warn!(error = %e, "Parser-version drift lookup failed; fingerprint-only filter");
            std::collections::HashSet::new()
        }
    };

    let mut survivors = Vec::with_capacity(file_batch.len());
    let mut refreshes: Vec<(PathBuf, FileFingerprint)> = Vec::new();
    for (rel, origin) in file_batch.iter().zip(origins.iter()) {
        let abs_path = root.join(rel);
        // Version drift short-circuits the fingerprint comparison: the file
        // must re-parse regardless of whether its bytes moved. Read a full
        // disk fingerprint so the re-upsert stamps fresh mtime/size/hash.
        if drifted.contains(origin.as_str()) {
            fingerprints.insert(rel.clone(), full_disk_fingerprint(&abs_path));
            survivors.push(rel.clone());
            continue;
        }
        match stored_map.get(origin.as_str()) {
            // Not indexed yet — always a survivor.
            None => {
                fingerprints.insert(rel.clone(), full_disk_fingerprint(&abs_path));
                survivors.push(rel.clone());
            }
            Some(stored_fp) => {
                match FileFingerprint::read_disk(
                    &abs_path,
                    stored_fp,
                    FingerprintPolicy::MtimeOrHash,
                ) {
                    Some(disk_fp)
                        if stored_fp.matches(&disk_fp, FingerprintPolicy::MtimeOrHash) =>
                    {
                        // Unchanged — skip the parse. If the match came from
                        // the hash tiebreak (mtime/size moved, content
                        // identical), refresh the stored fingerprint so the
                        // next walk fast-paths on mtime+size again.
                        if disk_fp.content_hash.is_some()
                            && (disk_fp.mtime != stored_fp.mtime || disk_fp.size != stored_fp.size)
                        {
                            refreshes.push((rel.clone(), disk_fp));
                        }
                    }
                    Some(disk_fp) => {
                        // Divergent — reindex. `read_disk` only hashed when
                        // the stored row had fingerprint columns; ensure the
                        // hash is present so the upsert stamps a full
                        // fingerprint.
                        let fp = if disk_fp.content_hash.is_some() {
                            disk_fp
                        } else {
                            full_disk_fingerprint(&abs_path)
                        };
                        fingerprints.insert(rel.clone(), fp);
                        survivors.push(rel.clone());
                    }
                    None => {
                        // metadata() failed (deleted mid-walk, permission
                        // flip). Keep the file so the parser surfaces the
                        // real error; fingerprint stays empty.
                        fingerprints.insert(rel.clone(), FileFingerprint::default());
                        survivors.push(rel.clone());
                    }
                }
            }
        }
    }

    if !refreshes.is_empty() {
        match store.set_file_fingerprints_batch(&refreshes) {
            Ok(rows) => tracing::debug!(
                files = refreshes.len(),
                rows,
                "Refreshed fingerprints for mtime-bumped content-identical files"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                files = refreshes.len(),
                "Failed to refresh fingerprints; files will re-hash on the next walk"
            ),
        }
    }

    StalenessFilterResult {
        survivors,
        fingerprints,
    }
}

/// Stage 1: Parse files in parallel batches, filter by staleness, and send to embedder channels.
pub(super) fn parser_stage(
    files: Vec<PathBuf>,
    ctx: ParserStageContext,
    parse_tx: Sender<ParsedBatch>,
) -> Result<()> {
    let _span = tracing::info_span!("parser_stage").entered();
    let ParserStageContext {
        root,
        force,
        parser,
        store,
        parsed_count,
        parse_errors,
        model_config,
    } = ctx;
    let batch_size = embed_batch_size_for(&model_config);
    let file_batch_size = file_batch_size();

    // Dedicated rayon pool for the per-file parse below, with an explicit worker
    // stack size. The recursive tree-walk is bounded by PARSER_MAX_WALK_DEPTH,
    // which is sized to complete inside the configured stack; pinning the stack
    // here (rather than inheriting rayon's global-pool default) makes the
    // depth-rail-fits-the-stack assumption load-bearing-by-design. Built once
    // and reused across all file batches. On builder failure we fall back to the
    // global pool — the depth rail still prevents an overflow at the default
    // stack size, so a missing dedicated pool degrades the belt-and-suspenders,
    // not correctness.
    let parse_pool = rayon::ThreadPoolBuilder::new()
        .stack_size(cqs::limits::parser_stack_size())
        .build();
    let parse_pool = match parse_pool {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to build dedicated parser thread pool; using the global pool (depth rail still bounds stack usage)"
            );
            None
        }
    };

    for (batch_idx, file_batch) in files.chunks(file_batch_size).enumerate() {
        if check_interrupted() {
            break;
        }

        // Staleness pre-filter BEFORE parsing: one batched SELECT decides
        // which files diverged from the index; only those get the (much more
        // expensive) tree-sitter parse below. `--force` bypasses the filter.
        let StalenessFilterResult {
            survivors,
            fingerprints: file_fingerprints,
        } = filter_stale_files(&store, &root, file_batch, force);

        tracing::info!(
            batch = batch_idx + 1,
            files = file_batch.len(),
            to_parse = survivors.len(),
            "Processing file batch"
        );

        if survivors.is_empty() {
            parsed_count.fetch_add(file_batch.len(), Ordering::Relaxed);
            continue;
        }

        // Parse surviving files in parallel, collecting chunks and
        // relationships. The third tuple element accumulates the normalized
        // origins of files that FAILED to parse — recorded after the reduce as
        // a drift parse-failure marker (v31) so a version-drifted unparseable
        // file is not re-queued by `origins_with_parser_drift` every `cqs index`
        // run forever. Collected thread-locally in the fold, merged in reduce,
        // written once on this (store-owning) thread.
        // The par_iter parse chain, run on the dedicated stack-sized pool when
        // it built (else the global pool). `pool.install` confines this rayon
        // work to the pool with the explicit worker stack size matched to the
        // depth rail.
        let run_parse = || -> (Vec<cqs::Chunk>, RelationshipData, Vec<String>) {
            survivors
            .par_iter()
            .fold(
                || (Vec::new(), RelationshipData::default(), Vec::new()),
                |(mut all_chunks, mut all_rels, mut all_failed), rel_path| {
                    let abs_path = root.join(rel_path);
                    match parser.parse_file_all_with_chunk_calls(&abs_path) {
                        Ok((
                            mut chunks,
                            function_calls,
                            chunk_type_refs,
                            mut chunk_calls,
                            candidate_edges,
                        )) => {
                            // Rewrite paths to be relative for storage
                            // Normalize path separators to forward slashes for cross-platform consistency
                            let path_str = normalize_path(rel_path);
                            // The path display string the extract-time sites
                            // (`extract_chunk`, the markdown parser) folded into
                            // each chunk's id. `chunk.id` was built from THIS
                            // path; `new_id` rebuilds from the normalized
                            // relative `path_str`.
                            let abs_path_display = abs_path.display().to_string();
                            // Build a map of old IDs -> new IDs for parent_id fixup.
                            // MUST reconstruct via the same `chunk_id` helper the
                            // extract-time site used, including `byte_start`, or the
                            // map silently fails to remap. `byte_start` makes the id
                            // injective within a file (same-line byte-identical
                            // chunks no longer collide), so this map no longer
                            // collapses distinct chunks onto one entry.
                            //
                            // Suffix preservation: some chunks carry a structural
                            // suffix appended after the 4-field base (markdown
                            // table chunks `:t{idx}`, table windows `:t{idx}w{widx}`
                            // — see `chunk_id_suffixed`). The base reconstruction
                            // alone would drop that suffix, sending a suffixed
                            // `chunk.id` to a suffix-less `new_id`: non-injective
                            // (distinct table windows collapse onto one id) and the
                            // remapped id no longer matches the stored chunk. So we
                            // recover the suffix by stripping the extract-time base
                            // (rebuilt from the same absolute path) off `chunk.id`,
                            // then re-append it to the new base. Path-safe even when
                            // the path contains colons (the base prefix is matched
                            // whole, not by colon-splitting).
                            let id_map: std::collections::HashMap<String, String> = chunks
                                .iter()
                                .map(|chunk| {
                                    let new_base = cqs::parser::chunk_id(
                                        &path_str,
                                        chunk.line_start,
                                        chunk.byte_start,
                                        &chunk.content_hash,
                                    );
                                    let old_base = cqs::parser::chunk_id(
                                        &abs_path_display,
                                        chunk.line_start,
                                        chunk.byte_start,
                                        &chunk.content_hash,
                                    );
                                    let suffix =
                                        chunk.id.strip_prefix(&old_base).unwrap_or("");
                                    let new_id = format!("{new_base}{suffix}");
                                    (chunk.id.clone(), new_id)
                                })
                                .collect();
                            // Injectivity guard. Two failure modes, both the
                            // silent chunk-loss class this id format prevents:
                            //   - old ids collide -> `id_map` key collapse ->
                            //     fewer keys than chunks (`id_map.len()`).
                            //   - new ids collide -> distinct chunks UPSERT onto
                            //     one `chunks.id` PRIMARY KEY row -> fewer distinct
                            //     values than keys.
                            // The new-id check reads the values ACTUALLY assigned
                            // to `chunk.id` below (suffix included), not bare base
                            // ids — a base-only check would pass even when two
                            // table windows differ solely by their dropped suffix.
                            // The keys are the stored `chunk.id`s (suffix
                            // included), so the old-id check sees the real ids too.
                            #[cfg(debug_assertions)]
                            {
                                let distinct_new: std::collections::HashSet<&String> =
                                    id_map.values().collect();
                                debug_assert_eq!(
                                    id_map.len(),
                                    chunks.len(),
                                    "chunk id collision (old ids): {} chunks -> {} keys in {path_str}",
                                    chunks.len(),
                                    id_map.len(),
                                );
                                debug_assert_eq!(
                                    distinct_new.len(),
                                    chunks.len(),
                                    "chunk id collision (new ids): {} chunks -> {} distinct ids in {path_str}",
                                    chunks.len(),
                                    distinct_new.len(),
                                );
                            }
                            for chunk in &mut chunks {
                                chunk.file = rel_path.clone();
                                if let Some(new_id) = id_map.get(&chunk.id) {
                                    chunk.id = new_id.clone();
                                }
                                // Rewrite parent_id to match rewritten chunk IDs
                                if let Some(ref pid) = chunk.parent_id {
                                    if let Some(new_pid) = id_map.get(pid) {
                                        chunk.parent_id = Some(new_pid.clone());
                                    }
                                }
                            }
                            // parse_file_all_with_chunk_calls already emitted
                            // (chunk_id, CallSite) pairs from Pass 2 — no
                            // per-chunk re-parse needed here. Chunk ids come
                            // back in `path:line_start:byte_start:hash8` form
                            // (from `extract_chunk` using the absolute path);
                            // apply the same id_map we built above so they line
                            // up with the rewritten chunk ids.
                            for (id, _) in &mut chunk_calls {
                                if let Some(new_id) = id_map.get(id) {
                                    *id = new_id.clone();
                                }
                            }
                            all_rels.chunk_calls.extend(chunk_calls);
                            all_chunks.extend(chunks);
                            if !chunk_type_refs.is_empty() {
                                all_rels
                                    .type_refs
                                    .entry(rel_path.clone())
                                    .or_default()
                                    .extend(chunk_type_refs);
                            }
                            // Stash EVERY parsed file's call set, keyed on
                            // "file was parsed" — empty sets included. This is
                            // the parse-completion signal that drives the single
                            // function_calls writer (`upsert_function_calls_for_files`
                            // in store_stage), decoupled from chunk count. A file
                            // that went has-calls → no-calls (or had only
                            // oversize functions whose chunks were dropped) MUST
                            // ride here so its old rows are DELETE-then-INSERT
                            // replaced (empty → cleared; non-empty → refreshed).
                            // Gating on non-empty here was the orphan-edge leak:
                            // the chunk prune cannot — and must not — clean
                            // function_calls (an oversize-function file is
                            // zero-chunk but non-empty-calls).
                            all_rels
                                .function_calls
                                .entry(rel_path.clone())
                                .or_default()
                                .extend(function_calls);
                            // Stash EVERY parsed file's candidate set keyed on
                            // "file was parsed" (empty included) — the same
                            // wholesale-per-file replace semantics as
                            // function_calls, so a file that went has-candidates
                            // → none clears its old `candidate_edges` rows.
                            all_rels
                                .candidate_edges
                                .entry(rel_path.clone())
                                .or_default()
                                .extend(candidate_edges);
                        }
                        Err(e) => {
                            // Structured fields so a hot-path parse failure
                            // carries `path` + `error` cleanly across the rayon
                            // reduce instead of being interpolated into the
                            // message.
                            tracing::warn!(
                                path = %abs_path.display(),
                                error = %e,
                                "Failed to parse file"
                            );
                            parse_errors.fetch_add(1, Ordering::Relaxed);
                            // Stash the normalized origin so the post-reduce
                            // step can stamp the drift parse-failure marker.
                            all_failed.push(normalize_path(rel_path));
                        }
                    }
                    (all_chunks, all_rels, all_failed)
                },
            )
            .reduce(
                || (Vec::new(), RelationshipData::default(), Vec::new()),
                |(mut chunks_a, mut rels_a, mut failed_a), (chunks_b, rels_b, failed_b)| {
                    chunks_a.extend(chunks_b);
                    for (file, refs) in rels_b.type_refs {
                        rels_a.type_refs.entry(file).or_default().extend(refs);
                    }
                    for (file, calls) in rels_b.function_calls {
                        rels_a.function_calls.entry(file).or_default().extend(calls);
                    }
                    for (file, cands) in rels_b.candidate_edges {
                        rels_a
                            .candidate_edges
                            .entry(file)
                            .or_default()
                            .extend(cands);
                    }
                    rels_a.chunk_calls.extend(rels_b.chunk_calls);
                    failed_a.extend(failed_b);
                    (chunks_a, rels_a, failed_a)
                },
            )
        };
        let (chunks, batch_rels, parse_failed_origins) = match &parse_pool {
            Some(pool) => pool.install(run_parse),
            None => run_parse(),
        };

        // Stamp the drift parse-failure marker for every file that failed to
        // parse this batch (v31). Without this a version-drifted file that can't
        // parse re-enters the survivor set on every `cqs index` run forever:
        // its `chunks.parser_version` never advances (no successful re-parse),
        // so `origins_with_parser_drift` keeps selecting it. Recording the
        // current parser version here makes that query exclude the origin until
        // its content changes (a successful re-parse clears the marker). Mirrors
        // the watch path's `touch_mtime_or_warn` loop-breaker, for the drift
        // predicate rather than the fingerprint predicate.
        if !parse_failed_origins.is_empty() {
            let failed_refs: Vec<&str> = parse_failed_origins.iter().map(|s| s.as_str()).collect();
            if let Err(e) = store.record_parse_failures(&failed_refs, cqs::parser_version()) {
                tracing::warn!(
                    error = %e,
                    count = failed_refs.len(),
                    "Failed to record parse-failure markers; drifted unparseable files may re-queue next run"
                );
            }
        }

        // No post-parse staleness filter: only survivors of the pre-filter
        // were parsed, so every chunk and relationship here belongs to a
        // file that needs reindexing. Every parsed file has an entry in
        // `file_fingerprints` (the old relationship pruning by mtime map
        // membership is therefore a guaranteed no-op and was removed).

        parsed_count.fetch_add(file_batch.len(), Ordering::Relaxed);

        // Files that survived the pre-filter (previously indexed, now
        // divergent) but produced zero chunks this run: their stale chunks
        // must be pruned. Count how many chunks each file contributes so the
        // drain loop below can stamp a file's fingerprint only in the batch
        // carrying its LAST chunk, and so we can tell which survivors are
        // empty. A file with a parse ERROR is excluded from `empty_file_fingerprints`
        // (the only carrier of the zero-chunk prune+stamp into store_stage): it
        // has zero chunks because the parse FAILED, not because the file is
        // genuinely item-free. Including it would prune its last-good chunks
        // with an empty live set AND stamp its fingerprint current — a
        // syntax-broken file would lose its real chunks and be sealed
        // "skip forever" until its bytes change #1835. The v31 drift marker
        // suppresses the drift re-queue but does NOT undo a prune+stamp, so the
        // exclusion has to happen here. Excluded files keep their old chunks
        // untouched and stay UNSTAMPED, so the next run's pre-filter retries
        // the parse (self-healing).
        let mut remaining_per_file: std::collections::HashMap<PathBuf, usize> =
            std::collections::HashMap::with_capacity(file_fingerprints.len());
        for c in &chunks {
            *remaining_per_file.entry(c.file.clone()).or_insert(0) += 1;
        }
        let failed_set: std::collections::HashSet<&str> =
            parse_failed_origins.iter().map(|s| s.as_str()).collect();
        let empty_file_fingerprints: std::collections::HashMap<PathBuf, FileFingerprint> =
            file_fingerprints
                .iter()
                .filter(|(rel, _)| !remaining_per_file.contains_key(*rel))
                .filter(|(rel, _)| !failed_set.contains(normalize_path(rel).as_str()))
                .map(|(rel, fp)| (rel.clone(), fp.clone()))
                .collect();

        if chunks.is_empty() {
            // No chunks at all in this file batch, but some survivors may have
            // gone to zero chunks — send a chunk-less batch so the store stage
            // prunes their stale rows. Skip the send when there is nothing to
            // prune (e.g. every survivor was a parse error).
            if !empty_file_fingerprints.is_empty()
                && parse_tx
                    .send(ParsedBatch {
                        chunks: Vec::new(),
                        relationships: batch_rels,
                        file_fingerprints: std::collections::HashMap::new(),
                        empty_file_fingerprints,
                    })
                    .is_err()
            {
                break; // Receiver dropped
            }
            continue;
        }

        // Send in embedding-sized batches, FILE-ALIGNED: a single file's chunks
        // are never split across two `ParsedBatch`es. The two embed stages
        // (GPU + CPU) work-steal `parse_rx`, so a file split across batches
        // could have its halves processed by different stages at different
        // speeds; the fingerprint-bearing half (the file's last chunk) could
        // then reach `store_stage` BEFORE the file's earlier-batch chunks,
        // firing a flush+prune on a PARTIAL accumulator and stranding the
        // late-arriving chunks in an accum that never flushes — silent,
        // non-deterministic chunk loss. Keeping each file's whole contiguous run
        // inside one batch makes that file ride exactly one stage in order, so
        // its chunks and fingerprint arrive together (or, on a GPU-failure
        // split, cached-then-requeued in FIFO order on the single `embed_tx`).
        // Completion is then order-independent by construction, not by timing.
        //
        // A file's chunks are contiguous in `chunks` (the rayon fold appends
        // each file's chunks in one `extend`, and reduce concatenates disjoint
        // partitions), so a batch is a sum of whole contiguous file-runs.
        // `batch_size` is a soft cap: a single file larger than `batch_size`
        // rides alone (its own batch). `embed_documents` re-batches internally
        // at `embed_batch_size` for GPU memory, so an oversize per-file batch
        // does not change peak GPU usage — and `store_stage` already buffers a
        // whole straddling file in its accumulator regardless, so peak host
        // memory is unchanged too.
        //
        // Relationships ride with the first batch only; per-chunk data
        // (chunk_calls, type_edges) is deferred in store_stage until all chunks
        // are committed. A file's fingerprint is stamped in the (single) batch
        // carrying its chunks, so the stamp lands together with — never before —
        // the file's data. The empty-file prune set rides with the first batch
        // (it references no chunks, so ordering against chunk writes is
        // irrelevant).
        //
        // Drain owned chunks into each batch instead of `chunks.chunks(n)` +
        // `.to_vec()`, which would clone every Chunk. We own `chunks` here and
        // never reuse it after this loop, so moving each window out is safe.
        let mut remaining_rels = Some(batch_rels);
        let mut remaining_empties = Some(empty_file_fingerprints);
        let mut chunks = chunks;
        while !chunks.is_empty() {
            let take = file_aligned_take(&chunks, batch_size);
            // Compute the per-batch fingerprint stamp set from a borrow first;
            // `drain` below moves the same chunks out. Decrement each file's
            // remaining count as its chunks leave; a file whose count reaches
            // zero in this window has delivered its last chunk, so stamp it.
            // With file-aligned batching a file's whole run is in one window, so
            // its count drops to zero in the same batch as its chunks — the
            // stamp can never precede the data.
            let mut batch_fps: std::collections::HashMap<PathBuf, FileFingerprint> =
                std::collections::HashMap::new();
            for c in &chunks[..take] {
                if let Some(remaining) = remaining_per_file.get_mut(&c.file) {
                    *remaining -= 1;
                    if *remaining == 0 {
                        if let Some(fp) = file_fingerprints.get(&c.file) {
                            batch_fps.insert(c.file.clone(), fp.clone());
                        }
                    }
                }
            }
            let batch: Vec<cqs::Chunk> = chunks.drain(..take).collect();
            if parse_tx
                .send(ParsedBatch {
                    chunks: batch,
                    relationships: remaining_rels.take().unwrap_or_default(),
                    file_fingerprints: batch_fps,
                    empty_file_fingerprints: remaining_empties.take().unwrap_or_default(),
                })
                .is_err()
            {
                break; // Receiver dropped
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::types::embed_batch_size;
    use super::*;
    use crossbeam_channel::unbounded;
    use std::collections::HashSet;

    /// Build a chunk whose only load-bearing field for `file_aligned_take` is
    /// its `file` — the function groups by contiguous `file` runs.
    fn aligned_chunk(file: &str) -> cqs::Chunk {
        cqs::Chunk {
            id: format!("{file}:0"),
            file: PathBuf::from(file),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: "x".to_string(),
            signature: String::new(),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: String::new(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    fn aligned_chunks(files: &[&str]) -> Vec<cqs::Chunk> {
        files.iter().map(|f| aligned_chunk(f)).collect()
    }

    /// `file_aligned_take` never splits a single file's contiguous run, and the
    /// cut always lands on a file boundary. This is the by-construction property
    /// that kills the GPU/CPU work-steal chunk-loss race: a file that rides one
    /// `ParsedBatch` rides one embed stage in order, so its fingerprint never
    /// reaches `store_stage` before its data.
    #[test]
    fn file_aligned_take_respects_soft_cap_when_files_split_cleanly() {
        // Three files of 2 chunks each, cap 3: take only file a (2 <= 3, then
        // adding b's run would be 4 > 3 → stop). Cut lands between a and b.
        let chunks = aligned_chunks(&["a", "a", "b", "b", "c", "c"]);
        let take = file_aligned_take(&chunks, 3);
        assert_eq!(take, 2, "take only the first whole file-run under the cap");
        assert_ne!(
            chunks[take - 1].file,
            chunks[take].file,
            "cut must land on a file boundary"
        );
    }

    #[test]
    fn file_aligned_take_packs_multiple_small_files_up_to_cap() {
        // Files of 1 chunk each, cap 3: pack a, b, c (3 == cap → stop).
        let chunks = aligned_chunks(&["a", "b", "c", "d"]);
        let take = file_aligned_take(&chunks, 3);
        assert_eq!(take, 3, "pack whole small files until the cap is reached");
        assert_ne!(chunks[take - 1].file, chunks[take].file);
    }

    #[test]
    fn file_aligned_take_oversize_file_rides_alone() {
        // First file alone exceeds the cap: it must STILL be taken whole (never
        // split — splitting is the race). The next file starts a fresh batch.
        let chunks = aligned_chunks(&["big", "big", "big", "big", "small"]);
        let take = file_aligned_take(&chunks, 2);
        assert_eq!(
            take, 4,
            "an oversize first file rides alone, whole — never split"
        );
        assert_eq!(chunks[take - 1].file, PathBuf::from("big"));
        assert_eq!(chunks[take].file, PathBuf::from("small"));
    }

    #[test]
    fn file_aligned_take_single_file_takes_all() {
        let chunks = aligned_chunks(&["a", "a", "a"]);
        assert_eq!(file_aligned_take(&chunks, 2), 3, "lone file: take its run");
    }

    /// Driving `file_aligned_take` to exhaustion (as the drain loop does) must
    /// (1) deliver every chunk exactly once, (2) never split a file across two
    /// batches. Property-style sweep over a fixed corpus and several caps.
    #[test]
    fn file_aligned_take_full_drain_never_splits_a_file() {
        let layouts: &[&[&str]] = &[
            &["a", "a", "b", "c", "c", "c", "d"],
            &["x", "x", "x", "x", "y"],
            &["p", "q", "r", "s"],
            &["solo", "solo", "solo"],
        ];
        for layout in layouts {
            for cap in 1..=6 {
                let mut chunks = aligned_chunks(layout);
                let mut delivered: Vec<PathBuf> = Vec::new();
                while !chunks.is_empty() {
                    let take = file_aligned_take(&chunks, cap);
                    assert!(take >= 1 && take <= chunks.len(), "take in bounds");
                    // Boundary: the chunk just before the cut and just after must
                    // belong to different files (no split).
                    if take < chunks.len() {
                        assert_ne!(
                            chunks[take - 1].file,
                            chunks[take].file,
                            "layout {layout:?} cap {cap}: cut split a file run"
                        );
                    }
                    delivered.extend(chunks.drain(..take).map(|c| c.file));
                }
                // Every chunk delivered exactly once, in original order.
                let expected: Vec<PathBuf> =
                    aligned_chunks(layout).into_iter().map(|c| c.file).collect();
                assert_eq!(
                    delivered, expected,
                    "layout {layout:?} cap {cap}: drain lost or reordered chunks"
                );
            }
        }
    }

    /// End-to-end: a file whose chunks exceed `embed_batch_size` must ride
    /// EXACTLY ONE `ParsedBatch` — never straddle two. This is the parser-side
    /// guarantee that makes the GPU/CPU work-steal chunk-loss bug impossible by
    /// construction: a file confined to one batch is processed by one embed
    /// stage in order, so its fingerprint-bearing data can never overtake its
    /// earlier chunks in the cross-stage race.
    #[test]
    fn parser_stage_never_straddles_a_file_across_batches() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();

        // `big.rs` has > embed_batch_size functions; `a.rs`/`b.rs` are small.
        let mut big = String::new();
        for i in 0..200 {
            use std::fmt::Write as _;
            writeln!(&mut big, "pub fn big_{i}() {{}}").unwrap();
        }
        std::fs::write(root.join("big.rs"), &big).unwrap();
        std::fs::write(root.join("a.rs"), "pub fn a_one() {}\npub fn a_two() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "pub fn b_one() {}\n").unwrap();

        let rel_paths = vec![
            PathBuf::from("big.rs"),
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
        ];

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: true,
            parser: Arc::new(CqParser::new().unwrap()),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(rel_paths, ctx, tx).unwrap();

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        assert!(
            batches.len() >= 2,
            "big.rs should force multiple batches, got {}",
            batches.len()
        );

        // No file appears in more than one batch (the no-straddle invariant).
        let mut file_to_batch: std::collections::HashMap<PathBuf, usize> =
            std::collections::HashMap::new();
        for (bi, b) in batches.iter().enumerate() {
            let files_in_batch: HashSet<PathBuf> =
                b.chunks.iter().map(|c| c.file.clone()).collect();
            for f in files_in_batch {
                if let Some(prev) = file_to_batch.insert(f.clone(), bi) {
                    assert_eq!(
                        prev, bi,
                        "file {f:?} straddled parse batches {prev} and {bi} — \
                         the no-straddle invariant is broken"
                    );
                }
            }
        }
        // big.rs really did produce more than one batch's worth of chunks, so
        // the no-straddle property was non-trivially exercised.
        let big = PathBuf::from("big.rs");
        let big_chunks: usize = batches
            .iter()
            .flat_map(|b| b.chunks.iter())
            .filter(|c| c.file == big)
            .count();
        assert!(
            big_chunks > embed_batch_size(),
            "big.rs must exceed embed_batch_size to exercise the no-straddle path; got {big_chunks}"
        );
    }

    /// Fixture-driven regression test for the file-aligned send loop. Builds a
    /// many-file fixture corpus, runs `parser_stage` end-to-end, and verifies:
    ///
    /// * every chunk the parser produced is delivered (no loss)
    /// * chunk IDs are unique across batches (drain did not alias data)
    /// * NO file straddles two batches (the no-straddle invariant that kills the
    ///   GPU/CPU work-steal chunk-loss race)
    /// * each batch is at or below `embed_batch_size()` (soft cap; a lone file
    ///   larger than the cap is the only exception, absent from this fixture)
    /// * at least two batches are emitted (so the loop actually iterates)
    /// * relationships ride with exactly one batch
    ///
    /// Uses many SMALL files (each well under the cap) so the file-aligned packer
    /// must group several files per batch and roll to a second batch — exercising
    /// the multi-file packing path. Avoids mutating the shared
    /// `CQS_EMBED_BATCH_SIZE` env var, which would race with
    /// `pipeline::tests::test_embed_batch_size`.
    #[test]
    fn parser_stage_drains_chunks_without_loss() {
        // Serialize with `pipeline::tests::test_embed_batch_size`, which
        // mutates CQS_EMBED_BATCH_SIZE — without this guard a parallel run
        // could flip the batch size mid-test and invalidate assertions.
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();

        // 50 files of 2 functions each = 100 chunks. With the default
        // embed_batch_size (64) the file-aligned packer fills a first batch with
        // ~32 files (64 chunks), then rolls the rest into a second batch —
        // forcing multiple iterations and exercising multi-file packing, all
        // with files far smaller than the cap (so none rides alone/oversize).
        let mut rel_paths: Vec<PathBuf> = Vec::new();
        for i in 0..50 {
            let name = format!("f{i}.rs");
            std::fs::write(
                root.join(&name),
                format!("pub fn f{i}_a() {{}}\npub fn f{i}_b() {{}}\n"),
            )
            .unwrap();
            rel_paths.push(PathBuf::from(name));
        }

        // Store + parser — same flavour as the real pipeline.
        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        let parser = Arc::new(CqParser::new().unwrap());

        // Ground truth: parse each file directly and count chunks so we can
        // assert the send loop delivered the full set.
        let expected_total: usize = rel_paths
            .iter()
            .map(|rel| {
                let abs = root.join(rel);
                parser.parse_file_all(&abs).unwrap().0.len()
            })
            .sum();
        assert!(
            expected_total > embed_batch_size(),
            "fixture must produce more chunks than batch_size; got {expected_total}"
        );

        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: true, // bypass needs_reindex on fresh store
            parser: Arc::clone(&parser),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            // `resolve` returns `Self`, not Result/Option — no `.unwrap()`.
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(rel_paths, ctx, tx).unwrap();

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        assert!(
            batches.len() >= 2,
            "fixture should force multiple batches, got {}",
            batches.len()
        );

        let max_batch = embed_batch_size();
        let mut ids: HashSet<String> = HashSet::new();
        let mut total = 0usize;
        let mut rels_seen = 0usize;
        // Every file appears in at most one batch (no straddle).
        let mut file_to_batch: std::collections::HashMap<PathBuf, usize> =
            std::collections::HashMap::new();
        for (bi, b) in batches.iter().enumerate() {
            assert!(!b.chunks.is_empty(), "empty batch should not be sent");
            // Soft cap: every fixture file is 2 chunks (< cap), so no batch may
            // exceed the cap (only a lone oversize file could, and there is none).
            assert!(
                b.chunks.len() <= max_batch,
                "batch must respect the soft cap embed_batch_size={max_batch}, got {}",
                b.chunks.len()
            );
            total += b.chunks.len();
            for c in &b.chunks {
                assert!(ids.insert(c.id.clone()), "duplicated chunk id: {}", c.id);
                if let Some(prev) = file_to_batch.insert(c.file.clone(), bi) {
                    assert_eq!(
                        prev, bi,
                        "file {:?} straddled batches {prev} and {bi}",
                        c.file
                    );
                }
            }
            let has_rels = !b.relationships.type_refs.is_empty()
                || !b.relationships.function_calls.is_empty()
                || !b.relationships.chunk_calls.is_empty()
                || !b.relationships.candidate_edges.is_empty();
            if has_rels {
                rels_seen += 1;
            }
        }
        assert_eq!(
            total, expected_total,
            "drain loop must deliver every parsed chunk once"
        );
        assert!(
            rels_seen <= 1,
            "relationships should ride with at most one batch, saw {rels_seen}"
        );
    }

    /// Re-id must preserve a chunk's structural id suffix end-to-end.
    ///
    /// Table windows (and the markdown table chunk) carry a `:t{idx}w{widx}`
    /// suffix appended AFTER the four-field base by `chunk_id_suffixed`. The
    /// re-id remap rebuilds each chunk's id from its persisted coordinates
    /// (path absolute→relative); a base-only reconstruction drops the suffix,
    /// so the suffixed extract-time `chunk.id` would be rewritten to a
    /// suffix-LESS id — diverging from the parser-native form. The store would
    /// then hold an id no fresh `parse_file` can reproduce, so an incremental
    /// re-index churns the row, and parent_id resolution across the windows of
    /// one table goes ambiguous. This drives a no-heading markdown file that is
    /// one LARGE table (forcing row-wise windowing) through the full
    /// `parser_stage` and asserts every stored window id still ends in its
    /// `:t0w{idx}` suffix and equals the suffix-aware re-id form.
    #[test]
    fn parser_stage_preserves_table_window_id_suffix_through_reid() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();

        // A no-heading file that is EXACTLY one table, wide+long enough to
        // exceed MAX_TABLE_CHARS (1500) so the table is split row-wise into
        // multiple windows. No trailing newline (also stresses the #1911 P1
        // whole-file/table base collision: the whole-file chunk and the table
        // share the (1, 0) start coords).
        let mut md = String::new();
        md.push_str("| Column A | Column B | Column C | Column D | Column E |\n");
        md.push_str("|----------|----------|----------|----------|----------|\n");
        for i in 0..60 {
            use std::fmt::Write as _;
            writeln!(
                &mut md,
                "| value_{i}_aaaa | value_{i}_bbbb | value_{i}_cccc | value_{i}_dddd | value_{i}_eeee |"
            )
            .unwrap();
        }
        while md.ends_with('\n') {
            md.pop();
        }
        let rel = PathBuf::from("only_table.md");
        std::fs::write(root.join(&rel), &md).unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        let parser = Arc::new(CqParser::new().unwrap());

        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: true,
            parser: Arc::clone(&parser),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(vec![rel.clone()], ctx, tx).unwrap();

        let chunks: Vec<cqs::Chunk> = rx.try_iter().flat_map(|b| b.chunks).collect();
        assert!(!chunks.is_empty(), "parser_stage produced no chunks");

        // Every stored id is injective within the file.
        let mut ids: HashSet<&str> = HashSet::new();
        for c in &chunks {
            assert!(
                ids.insert(c.id.as_str()),
                "stored chunk id collision after re-id: {} (name={:?})",
                c.id,
                c.name,
            );
        }

        // The table windowed: at least two window chunks exist (window_idx set).
        let windows: Vec<&cqs::Chunk> = chunks.iter().filter(|c| c.window_idx.is_some()).collect();
        assert!(
            windows.len() >= 2,
            "fixture must produce >=2 table windows, got {} (chunks: {:?})",
            windows.len(),
            chunks.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        );

        // Each window's stored id must STILL carry its `:t{idx}w{widx}` suffix
        // and equal the suffix-aware re-id reconstruction from the normalized
        // relative path. A base-only re-id (the bug) drops the `:t…w…` tail.
        let path_str = normalize_path(&rel);
        for w in &windows {
            let widx = w.window_idx.unwrap();
            let expected = cqs::parser::chunk_id_suffixed(
                &path_str,
                w.line_start,
                w.byte_start,
                &w.content_hash,
                &format!("t0w{widx}"),
            );
            assert_eq!(
                w.id, expected,
                "table window id lost its suffix through re-id: stored {} vs expected {}",
                w.id, expected,
            );
            assert!(
                w.id.contains(":t0w"),
                "table window id must retain its :t{{idx}}w{{widx}} suffix: {}",
                w.id,
            );
        }

        // parent_id of every window resolves to exactly one chunk (the section).
        for w in &windows {
            if let Some(pid) = &w.parent_id {
                let matches = chunks.iter().filter(|o| &o.id == pid).count();
                assert!(
                    matches <= 1,
                    "window parent_id {pid} resolves to {matches} chunks — ambiguous",
                );
            }
        }
    }

    /// Incremental (`force=false`) runs must parse ONLY files whose disk
    /// fingerprint diverges from the stored one. The staleness pre-filter
    /// runs BEFORE the tree-sitter parse, and there is no post-parse filter
    /// any more — so a chunk from an unchanged file appearing in the output
    /// would prove the file was parsed. Three cases in one pass:
    ///
    /// * `fresh.rs` — indexed with a fingerprint matching disk → skipped
    /// * `new.rs` — not indexed at all → parsed
    /// * `stale.rs` — indexed with a divergent fingerprint → parsed
    #[test]
    fn parser_stage_incremental_parses_only_changed_files() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("fresh.rs"), "pub fn fresh_fn() {}\n").unwrap();
        std::fs::write(root.join("new.rs"), "pub fn new_fn() {}\n").unwrap();
        std::fs::write(root.join("stale.rs"), "pub fn stale_fn() {}\n").unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Helper: a minimal indexed chunk for `rel` so the origin exists.
        // Stamp the CURRENT parser version so this test exercises the
        // fingerprint-only path — a version-0 stamp would register as parser
        // drift and requeue every file (covered separately by
        // `parser_stage_reparses_version_drifted_file`).
        let seed_chunk = |rel: &str, name: &str| cqs::Chunk {
            id: format!("{rel}:1:seed"),
            file: PathBuf::from(rel),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: name.to_string(),
            signature: format!("pub fn {name}()"),
            content: format!("pub fn {name}() {{}}"),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "seed".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: cqs::parser_version(),
        };
        let emb = cqs::Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);

        // fresh.rs: stored fingerprint == disk fingerprint → must be skipped.
        let fresh_fp = FileFingerprint::read_disk(
            &root.join("fresh.rs"),
            &FileFingerprint::default(),
            FingerprintPolicy::HashOnly,
        )
        .expect("read fresh.rs fingerprint");
        assert!(fresh_fp.content_hash.is_some(), "HashOnly must hash");
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("fresh.rs"), fresh_fp);
        store
            .upsert_embedded_batch(
                &[(seed_chunk("fresh.rs", "fresh_fn"), emb.clone())],
                &[],
                &fps,
            )
            .unwrap();

        // stale.rs: stored fingerprint diverges (wrong mtime+size+hash).
        let stale_fp = cqs::store::FileFingerprint {
            mtime: Some(1_000),
            size: Some(1),
            content_hash: Some(*blake3::hash(b"old content").as_bytes()),
        };
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("stale.rs"), stale_fp);
        store
            .upsert_embedded_batch(&[(seed_chunk("stale.rs", "stale_fn"), emb)], &[], &fps)
            .unwrap();

        let rel_paths = vec![
            PathBuf::from("fresh.rs"),
            PathBuf::from("new.rs"),
            PathBuf::from("stale.rs"),
        ];
        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: false,
            parser: Arc::new(CqParser::new().unwrap()),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(rel_paths, ctx, tx).unwrap();

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        let parsed_files: HashSet<PathBuf> = batches
            .iter()
            .flat_map(|b| b.chunks.iter().map(|c| c.file.clone()))
            .collect();
        assert!(
            !parsed_files.contains(&PathBuf::from("fresh.rs")),
            "unchanged file must be skipped by the pre-filter, got {parsed_files:?}"
        );
        assert!(
            parsed_files.contains(&PathBuf::from("new.rs")),
            "unindexed file must be parsed, got {parsed_files:?}"
        );
        assert!(
            parsed_files.contains(&PathBuf::from("stale.rs")),
            "divergent file must be parsed, got {parsed_files:?}"
        );

        // Every parsed file ships a fully-populated fingerprint exactly once,
        // in the batch carrying its last chunk (file-complete stamping). The
        // union across batches must cover every parsed file.
        let mut stamped: HashMap<PathBuf, FileFingerprint> = HashMap::new();
        for b in &batches {
            for (file, fp) in &b.file_fingerprints {
                assert!(
                    stamped.insert(file.clone(), fp.clone()).is_none(),
                    "fingerprint for {file:?} stamped in more than one batch"
                );
            }
        }
        for file in &parsed_files {
            let fp = stamped
                .get(file)
                .unwrap_or_else(|| panic!("missing fingerprint for {file:?}"));
            assert!(fp.mtime.is_some(), "{file:?} fingerprint needs mtime");
            assert!(fp.size.is_some(), "{file:?} fingerprint needs size");
            assert!(
                fp.content_hash.is_some(),
                "{file:?} fingerprint needs content hash"
            );
        }
    }

    /// PARSER_VERSION drift makes an unchanged file stale. A file whose
    /// disk fingerprint matches the stored one EXACTLY, but whose stored chunk
    /// carries an older `parser_version`, must still be selected for reparse —
    /// otherwise a `PARSER_VERSION` bump heals nothing without `--force` and
    /// the v30 migration's "re-tags on next reindex" promise is false.
    #[test]
    fn parser_stage_reparses_version_drifted_file() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("drift.rs"), "pub fn drift_fn() {}\n").unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Seed an indexed chunk at a STALE parser_version (current - 1) with a
        // disk-matching fingerprint, so the only thing making it stale is the
        // version drift, not a fingerprint divergence.
        let stale_version = cqs::parser_version() - 1;
        let chunk = cqs::Chunk {
            id: "drift.rs:1:seed".to_string(),
            file: PathBuf::from("drift.rs"),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: "drift_fn".to_string(),
            signature: "pub fn drift_fn()".to_string(),
            content: "pub fn drift_fn() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "seed".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: stale_version,
        };
        let emb = cqs::Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);

        // Fingerprint that exactly matches disk — fingerprint-only filter
        // would SKIP this file.
        let disk_fp = FileFingerprint::read_disk(
            &root.join("drift.rs"),
            &FileFingerprint::default(),
            FingerprintPolicy::HashOnly,
        )
        .expect("read drift.rs fingerprint");
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("drift.rs"), disk_fp);
        store
            .upsert_embedded_batch(&[(chunk, emb)], &[], &fps)
            .unwrap();

        // Sanity: the seeded chunk really does carry the stale version.
        let drifted = store
            .origins_with_parser_drift(&["drift.rs"], cqs::parser_version())
            .unwrap();
        assert!(
            drifted.contains("drift.rs"),
            "seeded chunk must register as parser-version drifted"
        );

        let rel_paths = vec![PathBuf::from("drift.rs")];
        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: false, // incremental — only drift can save this file
            parser: Arc::new(CqParser::new().unwrap()),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(rel_paths, ctx, tx).unwrap();

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        let parsed_files: HashSet<PathBuf> = batches
            .iter()
            .flat_map(|b| b.chunks.iter().map(|c| c.file.clone()))
            .collect();
        assert!(
            parsed_files.contains(&PathBuf::from("drift.rs")),
            "version-drifted file must be reparsed even with a matching fingerprint, got {parsed_files:?}"
        );
    }

    /// Seam-audit Finding 2 (wiring): a version-drifted file that FAILS to
    /// parse in `parser_stage` must not loop forever. The bulk path selects it
    /// (drifted), the parse errors (here: an IO failure — the file is removed
    /// after seeding, the same `Err` arm a tree-sitter `ParseFailed` takes), and
    /// the post-reduce step stamps the drift parse-failure marker. A second
    /// `origins_with_parser_drift` query — the pre-filter's drift selector —
    /// must then NO LONGER select it; its chunks never advanced past the stale
    /// version because no parse succeeded.
    ///
    /// Fails before the fix: the drift query keyed on `chunks.parser_version`
    /// alone, so the second query still selected the origin.
    #[test]
    fn drifted_unparseable_file_not_requeued_after_failure() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("broken.rs"), "pub fn broken() {}\n").unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Seed an indexed chunk at a STALE parser_version with a disk-matching
        // fingerprint, so only version drift (not fingerprint divergence) makes
        // it stale.
        let stale_version = cqs::parser_version() - 1;
        let chunk = cqs::Chunk {
            id: "broken.rs:1:seed".to_string(),
            file: PathBuf::from("broken.rs"),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: "broken".to_string(),
            signature: "pub fn broken()".to_string(),
            content: "pub fn broken() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "seed".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: stale_version,
        };
        let emb = cqs::Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);
        let disk_fp = FileFingerprint::read_disk(
            &root.join("broken.rs"),
            &FileFingerprint::default(),
            FingerprintPolicy::HashOnly,
        )
        .expect("read broken.rs fingerprint");
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("broken.rs"), disk_fp);
        store
            .upsert_embedded_batch(&[(chunk, emb)], &[], &fps)
            .unwrap();

        // First query: drifted (the loop's starting condition).
        let drifted_first = store
            .origins_with_parser_drift(&["broken.rs"], cqs::parser_version())
            .unwrap();
        assert!(
            drifted_first.contains("broken.rs"),
            "seeded drifted file must register as drifted on the first pass"
        );

        // Force the parse to fail: remove the file so `parse_file_all_with_chunk_calls`
        // hits an IO error (the same `Err` arm as a tree-sitter ParseFailed).
        // The drifted pre-filter still selects it (it pushes drifted origins
        // unconditionally), then the parse errors and the marker is recorded.
        std::fs::remove_file(root.join("broken.rs")).unwrap();

        let (tx, _rx) = unbounded::<ParsedBatch>();
        let parse_errors = Arc::new(AtomicUsize::new(0));
        let ctx = ParserStageContext {
            root: root.clone(),
            force: false,
            parser: Arc::new(CqParser::new().unwrap()),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::clone(&parse_errors),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(vec![PathBuf::from("broken.rs")], ctx, tx).unwrap();
        assert!(
            parse_errors.load(Ordering::Relaxed) >= 1,
            "broken.rs must have failed to parse for this regression to be meaningful"
        );

        // Second query: the marker now suppresses the requeue. The file's chunks
        // are still at the stale version (no parse succeeded), so without the
        // loop-breaker this would still select broken.rs — the loop.
        let drifted_second = store
            .origins_with_parser_drift(&["broken.rs"], cqs::parser_version())
            .unwrap();
        assert!(
            !drifted_second.contains("broken.rs"),
            "a drifted file that already failed to parse at the current version must \
             NOT be re-queued by drift on the second pass (Finding 2)"
        );

        // The marker is version-scoped: a future PARSER_VERSION bump (modeled by
        // querying at current+1) re-arms the requeue so the file is retried once
        // at the new version, where it either heals or re-stamps the marker.
        let drifted_next_bump = store
            .origins_with_parser_drift(&["broken.rs"], cqs::parser_version() + 1)
            .unwrap();
        assert!(
            drifted_next_bump.contains("broken.rs"),
            "a marker recorded at version N must NOT suppress drift at version N+1 \
             — a new bump retries the parse once"
        );
    }

    /// File-complete fingerprint stamping under file-aligned batching: a large
    /// file (>64 functions) rides EXACTLY ONE `ParsedBatch` — its chunks are
    /// never split across batches — and its fingerprint is stamped exactly once,
    /// on that single batch. This is the post-fix shape of the crash-safety +
    /// no-loss invariant: because the file is confined to one batch (and thus
    /// one embed stage, in order), the fingerprint physically rides with the
    /// file's data, so it can NEVER reach `store_stage` before the file's chunks
    /// (the GPU/CPU work-steal race the no-straddle rule eliminates).
    #[test]
    fn parser_stage_stamps_fingerprint_only_on_last_chunk_batch() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();

        // One file with >64 functions: pre-fix this straddled two batches; with
        // file-aligned batching it rides ONE (oversize-rides-alone) batch.
        let mut big = String::new();
        for i in 0..70 {
            use std::fmt::Write as _;
            writeln!(&mut big, "pub fn straddle_{i}() {{}}").unwrap();
        }
        std::fs::write(root.join("straddle.rs"), &big).unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        let parser = Arc::new(CqParser::new().unwrap());

        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: true,
            parser,
            store,
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(vec![PathBuf::from("straddle.rs")], ctx, tx).unwrap();

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        let straddle = PathBuf::from("straddle.rs");

        // The file's chunks all ride a SINGLE batch (no straddle).
        let chunk_bearing: Vec<usize> = batches
            .iter()
            .enumerate()
            .filter(|(_, b)| b.chunks.iter().any(|c| c.file == straddle))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            chunk_bearing.len(),
            1,
            "a single file must ride exactly one batch (no straddle), got batches {chunk_bearing:?}"
        );

        // Its fingerprint is stamped exactly once, on that same batch — never
        // ahead of the data.
        let mut stamp_count = 0;
        for (i, b) in batches.iter().enumerate() {
            if b.file_fingerprints.contains_key(&straddle) {
                stamp_count += 1;
                assert_eq!(
                    i, chunk_bearing[0],
                    "fingerprint stamped on batch {i}, expected the file's only batch {}",
                    chunk_bearing[0]
                );
            }
        }
        assert_eq!(stamp_count, 1, "fingerprint must be stamped exactly once");
    }

    /// A previously-indexed file that now parses to ZERO chunks rides the
    /// pipeline as an `empty_file_fingerprints` entry (not a chunk batch) so
    /// the store stage can prune its stale chunks. Forces the all-empty
    /// file-batch path: the only survivor produces no chunks, so a chunk-less
    /// `ParsedBatch` must still be emitted carrying the empty-file entry.
    #[test]
    fn parser_stage_routes_zero_chunk_file_to_empty_set() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        // A Rust file with only comments parses to zero chunks.
        std::fs::write(root.join("empty.rs"), "// just a comment, no items\n").unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        let parser = Arc::new(CqParser::new().unwrap());

        // Confirm the fixture really parses to zero chunks; otherwise the test
        // would silently exercise the wrong path.
        let direct = parser.parse_file_all(&root.join("empty.rs")).unwrap().0;
        assert!(
            direct.is_empty(),
            "fixture must parse to zero chunks, got {}",
            direct.len()
        );

        let (tx, rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: true, // survivor regardless of stored state
            parser,
            store,
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(vec![PathBuf::from("empty.rs")], ctx, tx).unwrap();

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        let empty = PathBuf::from("empty.rs");
        let carries_empty = batches
            .iter()
            .any(|b| b.empty_file_fingerprints.contains_key(&empty));
        assert!(
            carries_empty,
            "zero-chunk survivor must ride as an empty_file_fingerprints entry"
        );
        // It must NOT appear as a chunk-bearing fingerprint anywhere.
        assert!(
            batches
                .iter()
                .all(|b| !b.file_fingerprints.contains_key(&empty)),
            "zero-chunk file must not be stamped via the chunk path"
        );
    }

    /// #1835 Defect A — a survivor that FAILS to parse (zero chunks because the
    /// parse errored, NOT because the file is genuinely item-free) must NOT be
    /// routed into `empty_file_fingerprints`. That set is the only carrier of
    /// the zero-chunk prune+stamp into store_stage; routing a parse-error file
    /// there would prune its last-good chunks with an empty live set AND stamp
    /// its fingerprint current — a syntax-broken file would lose its real chunks
    /// and be sealed "skip forever". The file must instead be left untouched and
    /// UNSTAMPED so the next run retries the parse (self-healing).
    ///
    /// Fixture: seed `broken.rs` with a last-good chunk at a divergent
    /// fingerprint (so it's a survivor), then remove the file so the parse hits
    /// an IO error (the same `Err` arm a tree-sitter `ParseFailed` takes). Assert
    /// the emitted batches carry `broken.rs` in NEITHER `empty_file_fingerprints`
    /// NOR `file_fingerprints`.
    #[test]
    fn parser_stage_parse_error_survivor_not_routed_to_empty_set() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("broken.rs"), "pub fn broken() {}\n").unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Seed a last-good chunk with a DIVERGENT fingerprint so the pre-filter
        // selects broken.rs as a survivor (fingerprint mismatch), at the current
        // parser version so the drift selector is not what's keeping it in.
        let chunk = cqs::Chunk {
            id: "broken.rs:1:seed".to_string(),
            file: PathBuf::from("broken.rs"),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: "broken".to_string(),
            signature: "pub fn broken()".to_string(),
            content: "pub fn broken() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "seed".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: cqs::parser_version(),
        };
        let emb = cqs::Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);
        let divergent_fp = cqs::store::FileFingerprint {
            mtime: Some(1_000),
            size: Some(1),
            content_hash: Some(*blake3::hash(b"old content").as_bytes()),
        };
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("broken.rs"), divergent_fp);
        store
            .upsert_embedded_batch(&[(chunk, emb)], &[], &fps)
            .unwrap();

        // Force the parse to fail: remove the file so the parser hits an IO
        // error (same `Err` arm as a tree-sitter ParseFailed).
        std::fs::remove_file(root.join("broken.rs")).unwrap();

        let (tx, rx) = unbounded::<ParsedBatch>();
        let parse_errors = Arc::new(AtomicUsize::new(0));
        let ctx = ParserStageContext {
            root: root.clone(),
            force: false, // incremental — the divergent fingerprint makes it a survivor
            parser: Arc::new(CqParser::new().unwrap()),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::clone(&parse_errors),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(vec![PathBuf::from("broken.rs")], ctx, tx).unwrap();

        assert!(
            parse_errors.load(Ordering::Relaxed) >= 1,
            "broken.rs must have failed to parse for this regression to be meaningful"
        );

        let batches: Vec<ParsedBatch> = rx.try_iter().collect();
        let broken = PathBuf::from("broken.rs");
        assert!(
            batches
                .iter()
                .all(|b| !b.empty_file_fingerprints.contains_key(&broken)),
            "a parse-error survivor must NOT be routed to empty_file_fingerprints \
             (else store_stage prunes its last-good chunks + stamps it current)"
        );
        assert!(
            batches
                .iter()
                .all(|b| !b.file_fingerprints.contains_key(&broken)),
            "a parse-error survivor must NOT be stamped via the chunk path either"
        );

        // The last-good chunk must still be in the index (the parser stage never
        // pruned it) so search/callers keep working until a successful re-parse.
        assert_eq!(
            store.get_chunks_by_origin("broken.rs").unwrap().len(),
            1,
            "parse-error survivor keeps its last-good chunks"
        );
    }

    /// #1835 Defect A, end-to-end through store_stage: drive the full
    /// parse → store pipeline for a parse-error survivor and confirm the store
    /// stage neither prunes its chunks nor stamps its fingerprint, so the
    /// pre-filter re-selects it next run.
    #[test]
    fn parse_error_survivor_keeps_chunks_and_stays_unstamped_through_store() {
        let _lock = super::super::types::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("broken.rs"), "pub fn broken() {}\n").unwrap();

        let db_path = root.join("index.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let chunk = cqs::Chunk {
            id: "broken.rs:1:seed".to_string(),
            file: PathBuf::from("broken.rs"),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: "broken".to_string(),
            signature: "pub fn broken()".to_string(),
            content: "pub fn broken() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: "seed".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: cqs::parser_version(),
        };
        let emb = cqs::Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);
        let divergent_fp = cqs::store::FileFingerprint {
            mtime: Some(1_000),
            size: Some(1),
            content_hash: Some(*blake3::hash(b"old content").as_bytes()),
        };
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("broken.rs"), divergent_fp);
        store
            .upsert_embedded_batch(&[(chunk, emb)], &[], &fps)
            .unwrap();

        std::fs::remove_file(root.join("broken.rs")).unwrap();

        // Drive parser_stage, then feed its output into store_stage so the
        // prune+stamp decision is exercised exactly as the real pipeline does.
        let (parse_tx, parse_rx) = unbounded::<ParsedBatch>();
        let ctx = ParserStageContext {
            root: root.clone(),
            force: false,
            parser: Arc::new(CqParser::new().unwrap()),
            store: Arc::clone(&store),
            parsed_count: Arc::new(AtomicUsize::new(0)),
            parse_errors: Arc::new(AtomicUsize::new(0)),
            model_config: cqs::embedder::ModelConfig::resolve(None, None),
        };
        parser_stage(vec![PathBuf::from("broken.rs")], ctx, parse_tx).unwrap();

        // Convert ParsedBatch → EmbeddedBatch (no embedding needed for empties;
        // there are no chunks here) and run store_stage.
        let embed_emb = cqs::Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);
        let (embed_tx, embed_rx) = unbounded::<super::super::types::EmbeddedBatch>();
        for pb in parse_rx.try_iter() {
            embed_tx
                .send(super::super::types::EmbeddedBatch {
                    cached_count: 0,
                    chunk_embeddings: pb
                        .chunks
                        .into_iter()
                        .map(|c| (c, embed_emb.clone()))
                        .collect(),
                    relationships: pb.relationships,
                    file_fingerprints: pb.file_fingerprints,
                    empty_file_fingerprints: pb.empty_file_fingerprints,
                    uncached_need_embedding: false,
                })
                .unwrap();
        }
        drop(embed_tx);
        let parsed = AtomicUsize::new(0);
        let embedded = AtomicUsize::new(0);
        super::super::upsert::store_stage(
            embed_rx,
            &store,
            &parsed,
            &embedded,
            &indicatif::ProgressBar::hidden(),
        )
        .unwrap();

        // The last-good chunk survived the store stage (no empty-set prune).
        assert_eq!(
            store.get_chunks_by_origin("broken.rs").unwrap().len(),
            1,
            "store_stage must not prune a parse-error survivor's last-good chunks"
        );
        // The fingerprint stays divergent from disk-of-record (it was never
        // re-stamped this run), so the file re-selects next run. The stored
        // fingerprint must still be the OLD divergent one, not a fresh stamp.
        let stored = store.fingerprints_for_origins(&["broken.rs"]).unwrap();
        let fp = stored.get("broken.rs").expect("origin exists");
        assert_eq!(
            fp.mtime,
            Some(1_000),
            "parse-error survivor's fingerprint must NOT be re-stamped (stays the \
             seeded divergent value so the pre-filter re-selects it); got {fp:?}"
        );
    }

    /// The dedicated parse pool is built with the configured worker stack size,
    /// and rayon actually applies it: a deep recursion (at the depth rail's 800
    /// levels, with a fat per-frame array) completes on the pool's threads. This
    /// is the load-bearing-by-design link — if the pool inherited a too-small
    /// stack, the install below would overflow. Building the pool here exactly
    /// as `parser_stage` does keeps the test honest to the production
    /// construction.
    #[test]
    fn parse_pool_applies_configured_stack_size() {
        let stack = cqs::limits::parser_stack_size();
        assert!(
            stack >= 2 * 1024 * 1024,
            "resolved stack must be at least the depth-rail floor (2 MiB), got {stack}"
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .stack_size(stack)
            .num_threads(2)
            .build()
            .expect("dedicated parse pool builds");

        // Recurse with a non-trivial per-frame footprint so a stack smaller than
        // configured would overflow. `black_box` prevents the optimizer from
        // collapsing the frames away. Depth matches the parser depth rail (800).
        fn deep(level: usize, max: usize) -> u64 {
            let scratch = [0u8; 256];
            let acc = std::hint::black_box(scratch[level % scratch.len()]) as u64;
            if level >= max {
                acc
            } else {
                acc.wrapping_add(deep(level + 1, max))
            }
        }

        let depth = 800usize;
        let got = pool.install(|| deep(0, depth));
        // The point is no overflow; the returned value is incidental but pins
        // that the recursion actually ran to the rail.
        assert_eq!(got, 0, "deep recursion ran on the stack-sized pool");
    }
}
