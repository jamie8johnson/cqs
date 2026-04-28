//! The reindex hot path: parse + embed + store for files, and the
//! lighter notes-only path. Plus the daemon's resident SPLADE encoder
//! and its incremental-encode helper.
//!
//! `reindex_files` is the heaviest function in the watch loop — it
//! coordinates the file enumeration, parser, embedder cache lookups
//! (per-slot store + global cross-slot, #1129), and the chunk upsert.
//! Carved out so the loop's surrounding state machine
//! (`process_file_changes` in `events.rs`) reads as orchestration
//! rather than being inlined alongside ~350 lines of pipeline detail.

use super::*;

/// P2.74: count directories under `root` that `notify::RecommendedWatcher`
/// would register an inotify watch on, honoring `.gitignore` so we don't
/// over-count dirs the watcher already excludes via the gitignore matcher.
///
/// Used at `cmd_watch` startup to warn operators before saves silently stop
/// triggering reindex because inotify exhausted `fs.inotify.max_user_watches`.
#[cfg(target_os = "linux")]
pub(super) fn count_watchable_dirs(root: &Path) -> usize {
    let mut count = 0usize;
    let walker = ignore::WalkBuilder::new(root).hidden(false).build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            count += 1;
        }
    }
    count
}

/// Opaque identity of a database file for detecting replacements (DS-W5).
/// On Unix uses (device, inode) — survives renames that preserve the inode
/// and detects replacements where `index --force` creates a new file.
#[cfg(unix)]
pub(super) fn db_file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
pub(super) fn db_file_identity(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
/// #1004: Build the resident SPLADE encoder for the daemon's incremental
/// reindex path. Returns `None` when:
///
/// - `CQS_WATCH_INCREMENTAL_SPLADE=0` (feature flag kill-switch)
/// - No SPLADE model configured (no `CQS_SPLADE_MODEL`, no default at
///   `~/.cache/huggingface/splade-onnx/`)
/// - Encoder fails to load (corrupted ONNX, tokenizer mismatch, etc.)
///
/// A `None` encoder is not fatal: the daemon continues without
/// incremental SPLADE. Existing sparse vectors are preserved; coverage
/// drifts until a manual `cqs index` runs. A `warn!` is logged on load
/// failure so operators see the cause.
pub(super) fn build_splade_encoder_for_watch() -> Option<cqs::splade::SpladeEncoder> {
    let _span = tracing::info_span!("build_splade_encoder_for_watch").entered();

    if std::env::var("CQS_WATCH_INCREMENTAL_SPLADE").as_deref() == Ok("0") {
        tracing::info!(
            "CQS_WATCH_INCREMENTAL_SPLADE=0 — daemon runs dense-only, \
             sparse coverage will drift until manual 'cqs index'"
        );
        return None;
    }

    let dir = match cqs::splade::resolve_splade_model_dir() {
        Some(d) => d,
        None => {
            tracing::info!("No SPLADE model configured — incremental SPLADE disabled");
            return None;
        }
    };

    // Match the encoder's default score threshold used elsewhere (0.01).
    match cqs::splade::SpladeEncoder::new(&dir, 0.01) {
        Ok(enc) => {
            tracing::info!(
                model_dir = %dir.display(),
                "SPLADE encoder loaded for incremental encoding"
            );
            Some(enc)
        }
        Err(e) => {
            tracing::warn!(
                model_dir = %dir.display(),
                error = %e,
                "SPLADE encoder load failed — existing sparse_vectors untouched, \
                 coverage will drift until manual 'cqs index'"
            );
            None
        }
    }
}

/// #1004: Encode + upsert sparse vectors for the chunks that were just
/// (re)indexed. Called after a successful `reindex_files` when an encoder
/// is resident. Best-effort: encoding failures are logged and skipped
/// so a pathological chunk cannot block the watch loop.
pub(super) fn encode_splade_for_changed_files(
    encoder_mu: &std::sync::Mutex<cqs::splade::SpladeEncoder>,
    store: &Store,
    changed_files: &[PathBuf],
) {
    let batch_size = splade_batch_size();
    let _span = tracing::info_span!(
        "encode_splade_for_changed_files",
        n_files = changed_files.len(),
        batch_size
    )
    .entered();

    // Gather chunks for the changed files. `get_chunks_by_origin` returns
    // ChunkSummary which carries id + content. These are the chunks we
    // need to encode (re-encode over existing sparse_vectors is fine —
    // upsert_sparse_vectors deletes then inserts atomically).
    let mut batch: Vec<(String, String)> = Vec::new();
    for file in changed_files {
        // PB-V1.29-2: `file.display()` emits Windows backslashes, which
        // never match the forward-slash origins stored at ingest (chunks
        // are upserted via `normalize_path`). Using `.display()` here
        // makes SPLADE encoding a silent no-op on Windows.
        let origin = cqs::normalize_path(file);
        let chunks = match store.get_chunks_by_origin(&origin) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    origin = %origin,
                    error = %e,
                    "SPLADE encode: failed to fetch chunks for file — skipping"
                );
                continue;
            }
        };
        for chunk in chunks {
            batch.push((chunk.id, chunk.content));
        }
    }

    if batch.is_empty() {
        tracing::debug!("SPLADE encode: no chunks to encode, nothing to do");
        return;
    }

    let mut encoded: Vec<(String, cqs::splade::SparseVector)> = Vec::with_capacity(batch.len());
    let encoder = match encoder_mu.lock() {
        Ok(e) => e,
        Err(poisoned) => {
            tracing::warn!("SPLADE encoder mutex poisoned — recovering");
            poisoned.into_inner()
        }
    };

    for sub in batch.chunks(batch_size) {
        let texts: Vec<&str> = sub.iter().map(|(_, t)| t.as_str()).collect();
        match encoder.encode_batch(&texts) {
            Ok(sparse_batch) => {
                for ((chunk_id, _), sparse) in sub.iter().zip(sparse_batch) {
                    encoded.push((chunk_id.clone(), sparse));
                }
                tracing::debug!(batch_size = sub.len(), "SPLADE batch encoded");
            }
            Err(e) => {
                // Don't block the watch loop on a single bad batch — log + skip.
                // Coverage gap for these chunks self-heals on next 'cqs index'.
                tracing::warn!(
                    batch_size = sub.len(),
                    error = %e,
                    "SPLADE batch encode failed — skipping batch"
                );
            }
        }
    }
    drop(encoder);

    if encoded.is_empty() {
        return;
    }

    match store.upsert_sparse_vectors(&encoded) {
        Ok(inserted) => tracing::info!(
            chunks_encoded = encoded.len(),
            rows_inserted = inserted,
            "SPLADE incremental encode complete"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "SPLADE upsert failed — sparse_vectors not updated for this cycle"
        ),
    }
}

/// SPLADE batch size for incremental encoding. Mirrors the reranker
/// batch pattern (#963). Default 32 matches the reranker default.
pub(super) fn splade_batch_size() -> usize {
    std::env::var("CQS_SPLADE_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(32)
}
/// Reindex specific files.
///
/// Returns `(chunk_count, content_hashes)` — the content hashes can be used for
/// incremental HNSW insertion (looking up embeddings by hash instead of
/// rebuilding the full index).
///
/// `global_cache` (#1129) is the project-scoped cross-slot embedding cache;
/// when present, the cache is consulted before the per-slot store fallback,
/// matching the bulk pipeline's `prepare_for_embedding` shape. `None` mirrors
/// the pre-#1129 behaviour (store cache only) for tests and the
/// `CQS_CACHE_ENABLED=0` operator override.
pub(super) fn reindex_files(
    root: &Path,
    store: &Store,
    files: &[PathBuf],
    parser: &CqParser,
    embedder: &Embedder,
    global_cache: Option<&cqs::cache::EmbeddingCache>,
    quiet: bool,
) -> Result<(usize, Vec<String>)> {
    let _span = info_span!(
        "reindex_files",
        file_count = files.len(),
        global_cache = global_cache.is_some()
    )
    .entered();
    info!(file_count = files.len(), "Reindexing files");

    // Parse changed files once — extract chunks, calls, AND type refs in a single pass.
    // Avoids the previous double-read + double-parse per file.
    let mut all_type_refs: Vec<(PathBuf, Vec<ChunkTypeRefs>)> = Vec::new();
    // P2.67: collect per-chunk call sites from the parser instead of re-parsing
    // each chunk's body via `extract_calls_from_chunk` after the fact. The bulk
    // pipeline already does this via `parse_file_all_with_chunk_calls`; the
    // watch path was paying ~14k extra tree-sitter parses per repo-wide reindex.
    let mut per_file_chunk_calls: Vec<(String, cqs::parser::CallSite)> = Vec::new();
    let chunks: Vec<_> = files
        .iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            if !abs_path.exists() {
                // RT-DATA-7: File was deleted — remove its chunks from the store
                if let Err(e) = store.delete_by_origin(rel_path) {
                    tracing::warn!(
                        path = %rel_path.display(),
                        error = %e,
                        "Failed to delete chunks for deleted file"
                    );
                }
                return vec![];
            }
            match parser.parse_file_all_with_chunk_calls(&abs_path) {
                Ok((mut file_chunks, calls, chunk_type_refs, chunk_calls)) => {
                    // Rewrite paths to be relative (AC-2: fix both file and id)
                    //
                    // PB-V1.29-3: Use `cqs::normalize_path` on both sides. On
                    // Windows verbatim paths (`\\?\C:\...`) `abs_path.display()`
                    // keeps backslashes + the verbatim prefix, but `chunk.id`
                    // is built by the parser with forward-slash / stripped
                    // prefix — so the strip silently misses and chunks keep
                    // the absolute prefix, breaking cross-index equality and
                    // call-graph resolution. Normalize both sides so the
                    // prefix-strip actually matches, and the replacement uses
                    // the same convention.
                    let abs_norm = cqs::normalize_path(&abs_path);
                    let rel_norm = cqs::normalize_path(rel_path);
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                        // Rewrite id: replace absolute path prefix with relative
                        // ID format: {path}:{line_start}:{content_hash}
                        if let Some(rest) = chunk.id.strip_prefix(abs_norm.as_str()) {
                            chunk.id = format!("{}{}", rel_norm, rest);
                        }
                    }
                    // P2.67: stash chunk-level calls keyed by the post-rewrite
                    // chunk id so the post-loop fold can build `calls_by_id`
                    // without re-parsing each chunk.
                    for (abs_chunk_id, call) in chunk_calls {
                        let chunk_id = match abs_chunk_id.strip_prefix(abs_norm.as_str()) {
                            Some(rest) => format!("{}{}", rel_norm, rest),
                            None => abs_chunk_id,
                        };
                        per_file_chunk_calls.push((chunk_id, call));
                    }
                    // Stash type refs for upsert after chunks are stored
                    if !chunk_type_refs.is_empty() {
                        all_type_refs.push((rel_path.clone(), chunk_type_refs));
                    }
                    // RT-DATA-8: Write function_calls table (file-level call graph).
                    // Previously discarded — callers/impact/trace commands need this.
                    //
                    // Always invoked, even on empty `calls`: the function does
                    // DELETE WHERE file=X then INSERT current. Skipping the call
                    // when current is empty leaks rows for files that previously
                    // had function_calls but no longer do (audit P1 #17 / E.2:
                    // `delete_phantom_chunks` cannot do this cleanup itself
                    // because it would wipe the just-written rows).
                    if let Err(e) = store.upsert_function_calls(rel_path, &calls) {
                        tracing::warn!(
                            path = %rel_path.display(),
                            error = %e,
                            "Failed to write function_calls for watched file"
                        );
                    }
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!(
                        path = %abs_path.display(),
                        error = %e,
                        "Failed to parse file — touching mtime to break reconcile loop"
                    );
                    // EH-V1.30.1-1: refresh `chunks.source_mtime` for this
                    // origin so the next `run_daemon_reconcile` pass sees
                    // `disk == stored` and stops re-queuing the file every
                    // 30 s (default reconcile cadence). Without this the
                    // file stays in the divergent set forever — every
                    // tick triggers a parse, fails, emits a warn, and
                    // requeues. The mtime touch is the load-bearing
                    // piece; the file's previous chunks remain visible
                    // in search until the user fixes the syntax error
                    // and the next save retriggers a successful re-parse.
                    if let Ok(meta) = std::fs::metadata(&abs_path) {
                        if let Ok(disk_mtime) = meta.modified() {
                            if let Ok(d) = disk_mtime.duration_since(std::time::UNIX_EPOCH) {
                                let mtime_ms = cqs::duration_to_mtime_millis(d);
                                if let Err(touch_err) =
                                    store.touch_source_mtime(rel_path, mtime_ms)
                                {
                                    tracing::warn!(
                                        path = %rel_path.display(),
                                        error = %touch_err,
                                        "Failed to touch source_mtime for parse-failed file — reconcile loop may persist"
                                    );
                                }
                            }
                        }
                    }
                    vec![]
                }
            }
        })
        .collect();

    // Apply windowing to split long chunks into overlapping windows
    let chunks = crate::cli::pipeline::apply_windowing(chunks, embedder);

    if chunks.is_empty() {
        return Ok((0, Vec::new()));
    }

    // #1129: cache-check chain mirrors `prepare_for_embedding`'s
    // global-cache → store-cache → embed fallback. Pre-#1129 the watch path
    // only consulted `store.get_embeddings_by_hashes` so a chunk hashed in
    // another slot (or under a previous model) paid GPU cost on every save
    // even though `EmbeddingCache::project_default_path` had the vector.
    //
    // The dim guard matches `prepare_for_embedding`: skip the per-slot
    // store cache when `embedder.embedding_dim() != store.dim()` (a model
    // swap is in progress); the global cache is dim-checked inside
    // `read_batch` so dimension drift there is silently filtered.
    let dim = embedder.embedding_dim();
    let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();

    // Step 1: global (project-scoped, cross-slot) cache.
    let mut global_hits: HashMap<String, Embedding> = HashMap::new();
    if let Some(cache) = global_cache {
        let model_fp = embedder.model_fingerprint();
        match cache.read_batch(&hashes, model_fp, cqs::cache::CachePurpose::Embedding, dim) {
            Ok(hits) => {
                if !hits.is_empty() {
                    tracing::debug!(hits = hits.len(), "Watch global cache hits");
                }
                for (hash, emb_vec) in hits {
                    if let Ok(emb) = Embedding::try_new(emb_vec) {
                        global_hits.insert(hash, emb);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Global cache read failed (best-effort)");
            }
        }
    }

    // Step 2: per-slot store cache. Only query for hashes the global cache
    // didn't satisfy (P3.42 mirror) and only when the embedder's dim matches
    // store dim — a model swap mid-watch means the stored vectors are stale.
    let mut store_hits: HashMap<String, Embedding> = if dim == store.dim() {
        let missed: Vec<&str> = hashes
            .iter()
            .copied()
            .filter(|h| !global_hits.contains_key(*h))
            .collect();
        if missed.is_empty() {
            HashMap::new()
        } else {
            store.get_embeddings_by_hashes(&missed)?
        }
    } else {
        tracing::info!(
            store_dim = store.dim(),
            embedder_dim = dim,
            "Skipping store embedding cache in watch (dimension mismatch — model switch)"
        );
        HashMap::new()
    };

    let mut cached: Vec<(usize, Embedding)> = Vec::new();
    let mut to_embed: Vec<(usize, &cqs::Chunk)> = Vec::new();
    // P3.46: take ownership via `.remove()` instead of `.get().clone()`. Each
    // cached embedding is ~4 KB (1024-dim BGE-large), so cloning per chunk on
    // a thousand-chunk reindex was 4 MB of avoidable allocation churn. Two
    // chunks with the same content_hash within one reindex (rare — implies
    // duplicate content across files) fall through to `to_embed` on the
    // second hit, which is correct: one cached embedding satisfies one slot.
    let global_hits_total = global_hits.len();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(emb) = global_hits.remove(&chunk.content_hash) {
            cached.push((i, emb));
        } else if let Some(emb) = store_hits.remove(&chunk.content_hash) {
            cached.push((i, emb));
        } else {
            to_embed.push((i, chunk));
        }
    }

    // OB-11: Log cache hit/miss stats for observability. #1129 expands the
    // breakdown to surface global vs. store cache hits independently.
    tracing::info!(
        cached = cached.len(),
        global_hits = global_hits_total,
        store_hits = cached.len().saturating_sub(global_hits_total),
        to_embed = to_embed.len(),
        "Embedding cache stats"
    );

    // Collect content hashes of NEWLY EMBEDDED chunks only (for incremental HNSW).
    // Unchanged chunks (cache hits) are already in the HNSW index from a prior cycle,
    // so re-inserting them would create duplicates (hnsw_rs has no dedup).
    let content_hashes: Vec<String> = to_embed
        .iter()
        .map(|(_, c)| c.content_hash.clone())
        .collect();

    // Only embed chunks that don't have cached embeddings
    let new_embeddings: Vec<Embedding> = if to_embed.is_empty() {
        vec![]
    } else {
        let texts: Vec<String> = to_embed
            .iter()
            .map(|(_, c)| generate_nl_description(c))
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        embedder.embed_documents(&text_refs)?.into_iter().collect()
    };

    // #1129: write fresh embeddings back to the global cache so the next
    // file save (or another slot) hits cache instead of going through the
    // embedder. Best-effort — mirrors the bulk pipeline's write-back shape
    // with borrowed slices to skip per-entry allocations (P3 #127).
    if let (Some(cache), false) = (global_cache, to_embed.is_empty()) {
        let entries: Vec<(&str, &[f32])> = to_embed
            .iter()
            .zip(new_embeddings.iter())
            .map(|((_, chunk), emb)| (chunk.content_hash.as_str(), emb.as_slice()))
            .collect();
        if let Err(e) = cache.write_batch(
            &entries,
            embedder.model_fingerprint(),
            cqs::cache::CachePurpose::Embedding,
            dim,
        ) {
            tracing::warn!(error = %e, "Watch global cache write failed (best-effort)");
        }
    }

    // Merge cached and new embeddings in original chunk order.
    //
    // P3.41: build via a HashMap keyed by chunk index instead of pre-allocating
    // `chunk_count` empty `Embedding::new(vec![])` placeholders. The old shape
    // wasted N×Vec allocations on every reindex AND left a zero-length-vector
    // landmine if a slot was ever skipped (cosine distance with len-0 = NaN).
    // Mirrors the bulk pipeline's `create_embedded_batch` order-merge logic.
    let chunk_count = chunks.len();
    let mut by_index: HashMap<usize, Embedding> = HashMap::with_capacity(chunk_count);
    for (i, emb) in cached {
        by_index.insert(i, emb);
    }
    for ((i, _), emb) in to_embed.into_iter().zip(new_embeddings) {
        by_index.insert(i, emb);
    }
    let embeddings: Vec<Embedding> = (0..chunk_count)
        .map(|i| {
            by_index.remove(&i).unwrap_or_else(|| {
                // Should be unreachable: every chunk index is filled either
                // from `cached` or from `to_embed` above. If we ever land
                // here, the upstream split lost a chunk.
                tracing::error!(
                    chunk_index = i,
                    chunk_count,
                    "missing embedding at chunk index — upstream split lost a chunk"
                );
                panic!("missing embedding at chunk index {i} (chunk_count={chunk_count})")
            })
        })
        .collect();

    // P2.67: build calls_by_id directly from `per_file_chunk_calls` (collected
    // by `parse_file_all_with_chunk_calls` above) instead of re-parsing every
    // chunk's body with `extract_calls_from_chunk`. The bulk indexing pipeline
    // has used this shape since #1040; the watch path now matches it.
    let mut calls_by_id: HashMap<String, Vec<cqs::parser::CallSite>> = HashMap::new();
    for (chunk_id, call) in per_file_chunk_calls {
        calls_by_id.entry(chunk_id).or_default().push(call);
    }
    // Group chunks by file and atomically upsert chunks + calls in a single transaction
    let mut mtime_cache: HashMap<PathBuf, Option<i64>> = HashMap::new();
    let mut by_file: HashMap<PathBuf, Vec<(cqs::Chunk, Embedding)>> = HashMap::new();
    for (chunk, embedding) in chunks.into_iter().zip(embeddings) {
        let file_key = chunk.file.clone();
        by_file
            .entry(file_key)
            .or_default()
            .push((chunk, embedding));
    }
    for (file, pairs) in &by_file {
        let mtime = *mtime_cache.entry(file.clone()).or_insert_with(|| {
            let abs_path = root.join(file);
            // bundle-reconcile-stat: capture the stat error separately so
            // we can surface it via tracing instead of silently storing
            // `mtime=None` for the file. A `None` here means reconcile
            // (`reconcile.rs:124-138`) treats the entry as un-stat-able
            // and skips it indefinitely, so the operator needs an
            // observable trail when the cause is a permission flip or
            // transient-AV-scan.
            match abs_path.metadata().and_then(|m| m.modified()) {
                Ok(t) => t
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as i64),
                Err(e) => {
                    tracing::debug!(
                        path = %abs_path.display(),
                        error = %e,
                        "Reindex: stat failed, storing mtime=None (file will be left to GC by reconcile)"
                    );
                    None
                }
            }
        });
        // PERF-4: O(1) lookup per chunk via pre-grouped HashMap instead of linear scan.
        let file_calls: Vec<_> = pairs
            .iter()
            .flat_map(|(c, _)| {
                calls_by_id
                    .get(&c.id)
                    .into_iter()
                    .flat_map(|calls| calls.iter().map(|call| (c.id.clone(), call.clone())))
            })
            .collect();
        // DS2-4: Upsert chunks+calls AND prune phantom chunks in one tx.
        // The previous two-step `upsert_chunks_and_calls` + `delete_phantom_chunks`
        // committed independently — a crash between them left the index
        // half-pruned (new chunks visible, removed chunks still present)
        // alongside a dirty HNSW flag. `upsert_chunks_calls_and_prune` fuses
        // both operations into a single `begin_write` transaction, making the
        // reindex all-or-nothing. RT-DATA-10 / DS-37.
        let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
        store.upsert_chunks_calls_and_prune(
            pairs,
            mtime,
            &file_calls,
            Some(file.as_path()),
            &live_ids,
        )?;
    }

    // Upsert type edges from the earlier parse_file_all() results.
    // Type edges are soft data — separate from chunk+call atomicity.
    // They depend on chunk IDs existing in the DB, which is why we upsert
    // them after chunks are stored above. Use batched version (single transaction).
    if let Err(e) = store.upsert_type_edges_for_files(&all_type_refs) {
        tracing::warn!(error = %e, "Failed to update type edges");
    }

    if let Err(e) = store.touch_updated_at() {
        tracing::warn!(error = %e, "Failed to update timestamp");
    }

    if !quiet {
        println!("Updated {} file(s)", files.len());
    }

    Ok((chunk_count, content_hashes))
}

/// Reindex notes from docs/notes.toml
pub(super) fn reindex_notes(root: &Path, store: &Store, quiet: bool) -> Result<usize> {
    let _span = info_span!("reindex_notes").entered();

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(0);
    }

    // DS-34: Hold shared lock during read+index to prevent partial reads
    // if another process is writing notes concurrently (e.g., `cqs notes add`).
    let lock_file = std::fs::File::open(&notes_path)?;
    lock_file.lock_shared()?;

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        drop(lock_file);
        return Ok(0);
    }

    let count = cqs::index_notes(&notes, &notes_path, store)?;

    drop(lock_file); // release lock after index completes

    if !quiet {
        let ns = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            ns.total, ns.warnings, ns.patterns
        );
    }

    Ok(count)
}
