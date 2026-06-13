//! The reindex hot path: parse + embed + store for files, and the
//! lighter notes-only path. Plus the daemon's resident SPLADE encoder
//! and its incremental-encode helper.
//!
//! `reindex_files` is the heaviest function in the watch loop — it
//! coordinates the file enumeration, parser, embedder cache lookups
//! (per-slot store + global cross-slot), and the chunk upsert.
//! Lives apart so the loop's surrounding state machine
//! (`process_file_changes` in `events.rs`) reads as orchestration
//! rather than being inlined alongside ~350 lines of pipeline detail.

use super::*;

/// Count directories under `root` that `notify::RecommendedWatcher`
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

/// Opaque identity of a database file for detecting replacements.
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
/// Build the resident SPLADE encoder for the daemon's incremental
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

/// Encode + upsert sparse vectors for the chunks that were just
/// (re)indexed. Called after a successful `reindex_files` when an encoder
/// is resident. Best-effort: encoding failures are logged and skipped
/// so a pathological chunk cannot block the watch loop.
pub(super) fn encode_splade_for_changed_files(
    encoder_mu: &std::sync::Mutex<cqs::splade::SpladeEncoder>,
    store: &Store,
    changed_files: &[PathBuf],
) {
    // Read the encoder's probed dims so the batch size scales with model
    // width / seq length. ensembledistil at 768/256 gets 32; SPLADE-Code
    // 0.6B at 1024/512 gets 8 instead of OOMing.
    let (hidden_size, max_length) = {
        let enc = encoder_mu.lock().unwrap_or_else(|p| p.into_inner());
        (enc.hidden_size(), enc.max_length())
    };
    let batch_size = splade_batch_size_for(hidden_size, max_length);
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
        // `file.display()` emits Windows backslashes, which never match the
        // forward-slash origins stored at ingest (chunks are upserted via
        // `normalize_path`). Using `.display()` here would make SPLADE
        // encoding a silent no-op on Windows.
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

/// SPLADE baseline batch + reference dims, paired so the formula in
/// [`splade_batch_size_for`] returns 32 for the canonical SPLADE-base
/// shape (hidden=768, max_length=256). Mirrors the reranker / embedder
/// patterns.
const DEFAULT_SPLADE_BATCH: usize = 32;
const SPLADE_REFERENCE_HIDDEN: usize = 768;
const SPLADE_REFERENCE_MAX_LENGTH: usize = 256;

/// SPLADE batch size for incremental encoding. Mirrors the reranker
/// batch pattern. Default 32 matches the reranker default.
///
/// Env-only path. Callers that have a [`SpladeEncoder`] in scope should
/// use [`splade_batch_size_for`] instead so the batch scales by the loaded
/// model's `(hidden_size, max_length)`.
pub(super) fn splade_batch_size() -> usize {
    std::env::var("CQS_SPLADE_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_SPLADE_BATCH)
}

/// SPLADE batch size scaled by `(hidden_size, max_length)`.
/// Mirrors `reranker::reranker_batch_size`. Lets SPLADE-Code 0.6B at
/// 1024-hidden / 512-seq use a smaller batch than ensembledistil at
/// 768/256, instead of OOMing the same default. `CQS_SPLADE_BATCH` env
/// wins regardless of the loaded model's dims.
///
/// Formula:
///   batch = 32 * (REFERENCE_HIDDEN / hidden).max(0.25)
///              * (REFERENCE_MAX_LENGTH / max_length).max(0.25)
/// rounded to a power of two clamped to `[1, 256]`.
pub(super) fn splade_batch_size_for(hidden_size: usize, max_length: usize) -> usize {
    if std::env::var_os("CQS_SPLADE_BATCH").is_some() {
        return splade_batch_size(); // env override path
    }
    let baseline = DEFAULT_SPLADE_BATCH as f64;
    let hidden_size = hidden_size.max(1) as f64;
    let max_length = max_length.max(1) as f64;
    let hidden_factor = (SPLADE_REFERENCE_HIDDEN as f64 / hidden_size).max(0.25);
    let seq_factor = (SPLADE_REFERENCE_MAX_LENGTH as f64 / max_length).max(0.25);
    let scaled = (baseline * hidden_factor * seq_factor).max(1.0) as usize;
    let rounded = scaled.next_power_of_two().clamp(1, 256);
    if rounded != DEFAULT_SPLADE_BATCH {
        tracing::debug!(
            hidden_size = hidden_size as usize,
            max_length = max_length as usize,
            scaled,
            rounded,
            "splade_batch_size_for: scaling baseline by (hidden_size, max_length)"
        );
    }
    rounded
}

/// Touch the stored `source_mtime` to disk's mtime so the next reconcile
/// pass sees `disk == stored` and stops re-queuing the file. Returns `true`
/// on success.
///
/// Each FS step (`metadata` → `modified` → `duration_since(UNIX_EPOCH)` →
/// `touch_source_mtime`) that fails silently would abandon the touch, leaving
/// the stored mtime stale and the reconcile loop running forever for that
/// file — and `cqs status --watch-fresh` would then claim `state == fresh`
/// while the touch never landed. The helper logs a distinct `tracing::warn!`
/// at each failure step so operators can see exactly which FS API or store
/// call broke the chain.
pub(super) fn touch_mtime_or_warn(store: &Store, rel_path: &Path, abs_path: &Path) -> bool {
    let meta = match std::fs::metadata(abs_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                path = %rel_path.display(),
                error = %e,
                "Cannot touch source_mtime: metadata() failed; reconcile loop may persist"
            );
            return false;
        }
    };
    let disk_mtime = match meta.modified() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                path = %rel_path.display(),
                error = %e,
                "Cannot touch source_mtime: modified() failed; reconcile loop may persist"
            );
            return false;
        }
    };
    let d = match disk_mtime.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                path = %rel_path.display(),
                error = %e,
                "Cannot touch source_mtime: file mtime predates UNIX_EPOCH (clock skew?); reconcile loop may persist"
            );
            return false;
        }
    };
    let mtime_ms = cqs::duration_to_mtime_millis(d);
    if let Err(e) = store.touch_source_mtime(rel_path, mtime_ms) {
        tracing::warn!(
            path = %rel_path.display(),
            error = %e,
            "Failed to touch source_mtime for parse-failed file — reconcile loop may persist"
        );
        return false;
    }
    true
}

/// Finalize watched files that parsed successfully but produced ZERO chunks
/// #1774: replace their `function_calls` from the freshly parsed call set,
/// prune their stale chunk rows, then stamp the v29 `file_registry` reconcile
/// fingerprint.
///
/// Both watch zero-chunk routes (whole-batch-empty early return AND the
/// partial route at the end of `reindex_files`) flow through here, so the
/// zero-chunk end-state is reached in exactly one place.
///
/// **function_calls replacement (parse-driven, decoupled from chunk count).**
/// "Zero chunks" is NOT "zero calls": a file whose single function exceeds
/// `CQS_PARSER_MAX_CHUNK_BYTES` parses to zero chunks but a NON-EMPTY call set
/// (Pass 1 drops the oversize chunk; Pass 2 emits file-level call edges with no
/// size gate). So each zero-chunk file's `function_calls` must be REPLACED with
/// its real parsed set — empty set → cleared (the comment-only / emptied case,
/// removing orphaned caller-side edges that would surface as ghost `cqs
/// callers` rows and veto otherwise-dead callees), NON-empty set → refreshed
/// (the oversize-function case, whose edges MUST survive). The replacement
/// rides the single parse-driven writer (`upsert_function_calls_for_files`), so
/// the chunk prune below never touches the call graph.
///
/// **chunk prune.** Mirrors the bulk pipeline (`upsert_chunks_calls_and_prune`):
/// a file edited from has-chunks to zero-chunks keeps its OLD chunk rows unless
/// they are deleted with an EMPTY live set. Leaving them is the #1830 ghost
/// (stale rows stay searchable) AND drift-loop fuel — `origins_with_parser_drift`
/// keys on `chunks.parser_version`, so a PARSER_VERSION bump makes the ghost
/// rows re-drift-eligible every reconcile tick forever (parse to zero → stamp →
/// no prune → loop). Pruning to zero rows removes both.
///
/// **registry stamp.** A zero-chunk file has no chunk row to carry the
/// chunk-level fingerprint, so without the registry stamp the watch reconcile
/// reclassifies it ADDED every 30 s tick and re-parses it forever. Computing
/// the full disk fingerprint (mtime + size + BLAKE3) and persisting it lets the
/// next reconcile pass skip the file like any unchanged one.
///
/// Ordering AND success are the safety guarantee. Sequence: replace
/// calls and prune chunks FIRST, stamp registry SECOND — a crash between leaves
/// the refreshed call set + zero chunk rows + an unstamped registry, re-parsed
/// idempotently next tick. Stamping before pruning would mark the file current
/// while ghost rows survive. But sequence alone is crash-safe, NOT error-safe:
/// a step that returns Err (calls-replace or prune) commits nothing yet the
/// later steps would still run. So each step is CONDITIONAL ON SUCCESS — a
/// calls-replace Err returns early (no prune, no stamp); a prune Err returns
/// early (no stamp). The stamp lands only when both upstream writes succeeded.
/// The forfeited stamp is the heal trigger: an un-stamped file is reclassified
/// and re-finalized next reconcile tick instead of committing stale calls +
/// zero chunks + a current fingerprint (the ghost-caller / false-DEAD bug,
/// reconcile-skipped on fingerprint-match and doctor-blind under the
/// `find_orphaned_function_calls` registry-UNION arm — permanent if stamped).
///
/// `entries` carry each zero-chunk file's project-relative path and its
/// freshly parsed call set; `root` joins the paths for the disk read.
/// Best-effort: a stat/read failure forfeits the skip (the file re-parses next
/// tick) but is not fatal.
fn finalize_zero_chunk_files(
    store: &Store,
    root: &Path,
    entries: &[(PathBuf, Vec<cqs::parser::FunctionCalls>)],
) {
    if entries.is_empty() {
        return;
    }

    // Replace each zero-chunk file's function_calls from its real parsed set —
    // the single parse-driven writer. Empty sets clear, non-empty (oversize
    // function) sets refresh. This is the decoupled call-graph half; the chunk
    // prune below makes no call-graph decision.
    //
    // CONDITIONAL ON SUCCESS, not just ordered: a transient replace failure
    // (e.g. SQLITE_BUSY past busy_timeout under a concurrent `cqs index
    // --force`) must FORFEIT the prune AND the stamp. The stamp-last
    // convention is crash-safe (nothing commits) but NOT error-safe — if the
    // calls-replace fails and we stamped anyway, the file commits stale calls +
    // zero chunks + a current fingerprint, which reconcile skips on
    // fingerprint-match (drift keys on chunks, now gone) and the dead-code
    // registry-UNION arm shields from the doctor: a PERMANENT ghost-caller /
    // false-DEAD. Returning early leaves the file UN-stamped, which is exactly
    // the designed heal trigger — the next reconcile tick reclassifies it ADDED
    // and re-finalizes idempotently. The warn is kept for telemetry; we just
    // stop committing past the failure.
    if let Err(e) = store.upsert_function_calls_for_files(entries) {
        tracing::warn!(
            files = entries.len(),
            error = %e,
            "Failed to replace function_calls for zero-chunk watched files; skipping prune + stamp so the files re-heal next tick"
        );
        return;
    }

    let rel_paths: Vec<&Path> = entries.iter().map(|(p, _)| p.as_path()).collect();

    // Prune each survivor's stale CHUNK rows with an EMPTY live set BEFORE
    // stamping the registry below — same mechanism as the bulk path's
    // `delete_phantom_chunks_batch(.., Vec::new())`. Chunks + FTS only; the
    // call graph was replaced above. A prune failure ALSO forfeits the stamp
    // (return early): an unstamped file re-finalizes next tick rather than
    // committing surviving ghost rows under a current fingerprint that
    // `origins_with_parser_drift` would re-arm forever.
    let prune_entries: Vec<(&Path, Vec<&str>)> =
        rel_paths.iter().map(|p| (*p, Vec::new())).collect();
    match store.delete_phantom_chunks_batch(&prune_entries) {
        Ok(deleted) if deleted > 0 => tracing::info!(
            count = deleted,
            "Pruned phantom chunks from zero-chunk watched files"
        ),
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                files = prune_entries.len(),
                error = %e,
                "Failed to prune chunks for zero-chunk watched files; skipping registry stamp so the files re-heal next tick"
            );
            return;
        }
    }
    let mut entries_fp: Vec<(PathBuf, cqs::store::FileFingerprint)> =
        Vec::with_capacity(rel_paths.len());
    for rel in &rel_paths {
        let abs_path = root.join(rel);
        let meta = match std::fs::metadata(&abs_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(
                    path = %abs_path.display(),
                    error = %e,
                    "Zero-chunk registry stamp: metadata() failed; file will re-parse next tick"
                );
                continue;
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(cqs::duration_to_mtime_millis);
        let size = Some(meta.len());
        let content_hash = match std::fs::File::open(&abs_path) {
            Ok(f) => {
                let mut hasher = blake3::Hasher::new();
                if hasher.update_reader(std::io::BufReader::new(f)).is_ok() {
                    Some(*hasher.finalize().as_bytes())
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        entries_fp.push((
            rel.to_path_buf(),
            cqs::store::FileFingerprint {
                mtime,
                size,
                content_hash,
            },
        ));
    }
    if !entries_fp.is_empty() {
        if let Err(e) = store.set_file_registry_fingerprints_batch(&entries_fp) {
            tracing::warn!(
                files = entries_fp.len(),
                error = %e,
                "Failed to stamp file_registry for zero-chunk watched files; they will re-parse next tick"
            );
        }
    }
}

/// Reindex specific files.
///
/// Returns `(chunk_count, content_hashes)` — the content hashes can be used for
/// incremental HNSW insertion (looking up embeddings by hash instead of
/// rebuilding the full index).
///
/// `global_cache` is the project-scoped cross-slot embedding cache; when
/// present, the cache is consulted before the per-slot store fallback,
/// matching the bulk pipeline's `prepare_for_embedding` shape. `None` is the
/// store-cache-only path used by tests and the `CQS_CACHE_ENABLED=0` operator
/// override.
// `pub(crate)` (was `pub(super)`): the worktree overlay builder
// (`src/cli/worktree_overlay_build.rs`) calls this to parse+embed the dirty
// delta into the overlay's in-memory store. Both live in the bin crate, so
// `pub(crate)` is the minimal-churn promotion — the lib-side overlay module
// (`src/worktree_overlay.rs`) holds the crate-portable pieces (struct, delta
// discovery, fingerprint) and the bin-side builder owns the pipeline call.
pub(crate) fn reindex_files(
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
    let mut all_type_refs: Vec<(PathBuf, Vec<ChunkTypeRefs>)> = Vec::new();
    // Collect per-chunk call sites from the parser instead of re-parsing each
    // chunk's body via `extract_calls_from_chunk` after the fact. The bulk
    // pipeline does this via `parse_file_all_with_chunk_calls`; re-parsing
    // would pay ~14k extra tree-sitter parses per repo-wide reindex.
    let mut per_file_chunk_calls: Vec<(String, cqs::parser::CallSite)> = Vec::new();
    // Stash file-level function_calls per origin so they're written in the
    // SAME tx as the chunks/FTS upsert below. Writing them before the heavy
    // embed phase would leave the function_calls table ahead of chunks/FTS on
    // a daemon crash during embed (SIGFPE crashes have been observed on
    // TensorRT) — `cqs callers <new_fn>` would work, search / `cqs explain`
    // wouldn't.
    use std::collections::HashMap;
    let mut all_function_calls: HashMap<PathBuf, Vec<cqs::parser::FunctionCalls>> = HashMap::new();
    let chunks: Vec<_> = files
        .iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            if !abs_path.exists() {
                // File was deleted — remove its chunks from the store
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
                    // Rewrite paths to be relative — fix both file and id.
                    //
                    // Use `cqs::normalize_path` on both sides. On Windows
                    // verbatim paths (`\\?\C:\...`) `abs_path.display()` keeps
                    // backslashes + the verbatim prefix, but `chunk.id` is
                    // built by the parser with forward-slash / stripped
                    // prefix — so the strip would silently miss and chunks
                    // would keep the absolute prefix, breaking cross-index
                    // equality and call-graph resolution. Normalize both sides
                    // so the prefix-strip matches and the replacement uses the
                    // same convention.
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
                    // Stash chunk-level calls keyed by the post-rewrite chunk
                    // id so the post-loop fold can build `calls_by_id` without
                    // re-parsing each chunk.
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
                    // Stash function_calls for atomic per-file write alongside
                    // chunks/FTS below (rather than a synchronous upsert before
                    // the embed phase, which would leave an asymmetric state
                    // when the daemon crashed mid-embed).
                    //
                    // Always stash (even with empty `calls`): the per-file
                    // upsert below does DELETE WHERE file=X then INSERT
                    // current. Skipping when empty leaks rows for files that
                    // previously had function_calls but no longer do
                    // (`delete_phantom_chunks` cannot do this cleanup itself).
                    all_function_calls.insert(rel_path.clone(), calls);
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!(
                        path = %abs_path.display(),
                        error = %e,
                        "Failed to parse file — touching mtime to break reconcile loop"
                    );
                    // Refresh `chunks.source_mtime` for this origin so the next
                    // `run_daemon_reconcile` pass sees `disk == stored` and
                    // stops re-queuing the file every 30 s (default reconcile
                    // cadence). Without this the file stays in the divergent
                    // set forever — every tick triggers a parse, fails, emits a
                    // warn, and requeues. The mtime touch is the load-bearing
                    // piece; the file's previous chunks remain visible in
                    // search until the user fixes the syntax error and the next
                    // save retriggers a successful re-parse.
                    //
                    // `touch_mtime_or_warn` is a fail-loud helper that logs a
                    // distinct warn at each FS step so operators can see *why* a
                    // touch failed and the reconcile loop persisted.
                    touch_mtime_or_warn(store, rel_path, &abs_path);
                    // Drift loop-breaker (v31): the mtime touch heals only the
                    // FINGERPRINT predicate. A version-drifted file that fails to
                    // parse keeps stale-version chunks, so `origins_with_parser_drift`
                    // would re-queue it every reconcile tick regardless of the
                    // touched fingerprint. Stamp the parser version it failed at
                    // so drift suppresses the requeue until its content changes.
                    // Recorded AFTER the touch: `touch_source_mtime` resets the
                    // marker to NULL via the shared registry UPSERT, so this must
                    // run last to leave the marker set.
                    let origin = cqs::normalize_path(rel_path);
                    if let Err(e) = store.record_parse_failures(&[origin.as_str()], cqs::parser_version())
                    {
                        tracing::warn!(
                            path = %rel_path.display(),
                            error = %e,
                            "Failed to record parse-failure drift marker; reconcile may re-queue this file on the next PARSER_VERSION drift"
                        );
                    }
                    vec![]
                }
            }
        })
        .collect();

    // Apply windowing to split long chunks into overlapping windows
    let chunks = crate::cli::pipeline::apply_windowing(chunks, embedder);

    if chunks.is_empty() {
        // Every survivor parsed to zero chunks (comment-only files, oversize
        // functions, etc.). They are all in `all_function_calls` (parse-error
        // and deleted files never landed there) WITH their real parsed call set
        // (empty for emptied files, NON-empty for oversize-function files).
        // `finalize_zero_chunk_files` replaces their function_calls from that
        // set, prunes their stale chunks, and stamps the v29 `file_registry`
        // fingerprint #1774 so the next reconcile tick skips them instead of
        // re-parsing to zero chunks every 30 s. Without this early-return
        // finalize, an all-empty batch would never reach the per-file path below.
        let zero_chunk_entries: Vec<(PathBuf, Vec<cqs::parser::FunctionCalls>)> =
            all_function_calls.into_iter().collect();
        finalize_zero_chunk_files(store, root, &zero_chunk_entries);
        return Ok((0, Vec::new()));
    }

    // Resolve embedding reuse (global cache → store cache → embed) via the
    // shared resolver in `cli::pipeline::reuse` — the SAME function the bulk
    // pipeline's `prepare_for_embedding` uses. #1692 unified the reuse DECISION
    // (canonical-key logic, NULL/empty-canonical fallback, dim-mismatch
    // store-cache skip, duplicate-key fallthrough) so a future reuse-semantics
    // change is a single edit. This path keeps its own batching/order-merge
    // below; only the cached-vs-embed split moved into the shared function.
    //
    // `resolve_reuse` returns indices into `chunks` (a borrowed slice here);
    // we rebuild the `(usize, &Chunk)` shape the order-merge below expects so
    // cache hits stay out of the incremental HNSW insert set.
    // Compute the model fingerprint only when a global cache exists — it's
    // the fingerprint's only consumer here (resolve_reuse's global branch +
    // the write-back below), and its first computation streams blake3 over
    // the full ONNX model file. Computed once and reused at both sites.
    let dim = embedder.embedding_dim();
    let model_fp: Option<String> = global_cache.is_some().then(|| embedder.model_fingerprint());
    // `?` on a store-cache read failure aborts this cycle so the watch loop
    // retries next tick with the error visible — a persistent SQLite failure
    // must NOT silently degrade into re-embedding the corpus on GPU each tick.
    let split = crate::cli::pipeline::resolve_reuse(
        &chunks,
        store,
        global_cache,
        dim,
        model_fp.as_deref(),
    )?;
    let global_hits_total = split.global_hits;
    let cached: Vec<(usize, Embedding)> = split.cached;
    let to_embed: Vec<(usize, &cqs::Chunk)> = split
        .to_embed
        .into_iter()
        .map(|i| (i, &chunks[i]))
        .collect();

    // Log cache hit/miss stats for observability, surfacing global vs. store
    // cache hits independently.
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
    //
    // Pre-allocate to skip the `Vec` resize cost on the hot reindex path.
    // TODO: change the downstream HNSW insert API to take `&[&str]` so the
    // per-element clone disappears entirely; the pre-allocation is the cheap
    // interim win.
    let mut content_hashes: Vec<String> = Vec::with_capacity(to_embed.len());
    content_hashes.extend(to_embed.iter().map(|(_, c)| c.content_hash.clone()));

    // Only embed chunks that don't have cached embeddings
    let new_embeddings: Vec<Embedding> = if to_embed.is_empty() {
        vec![]
    } else {
        // Use the model-aware NL variant so section chunks get the full
        // content budget the model can absorb (e.g. nomic-coderank's 2048-seq
        // capacity instead of a 512 cap).
        let model_max_seq_len = embedder.model_config().max_seq_length;
        let texts: Vec<String> = to_embed
            .iter()
            .map(|(_, c)| generate_nl_description_with_seq_len(c, model_max_seq_len))
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        embedder.embed_documents(&text_refs)?.into_iter().collect()
    };

    // Write fresh embeddings back to the global cache so the next file save
    // (or another slot) hits cache instead of going through the embedder.
    // Best-effort — mirrors the bulk pipeline's write-back shape with borrowed
    // slices to skip per-entry allocations.
    if let (Some(cache), Some(fp), false) = (global_cache, model_fp.as_deref(), to_embed.is_empty())
    {
        // Write under the canonical key (v28) so a later comment-only edit
        // reuses this embedding — the shared `canon_key_ref` owns the
        // empty-canonical fallback for both read and write-back sites.
        let entries: Vec<(&str, &[f32])> = to_embed
            .iter()
            .zip(new_embeddings.iter())
            .map(|((_, chunk), emb)| (crate::cli::pipeline::canon_key_ref(chunk), emb.as_slice()))
            .collect();
        if let Err(e) = cache.write_batch(&entries, fp, cqs::cache::CachePurpose::Embedding, dim) {
            tracing::warn!(error = %e, "Watch global cache write failed (best-effort)");
        }
    }

    // Merge cached and new embeddings in original chunk order.
    //
    // Build via a HashMap keyed by chunk index rather than pre-allocating
    // `chunk_count` empty `Embedding::new(vec![])` placeholders — that would
    // waste N×Vec allocations on every reindex and leave a zero-length-vector
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
    // A code path where `new_embeddings.len() != to_embed.len()` (partial ORT
    // batch failure, embedder API change, etc.) returns an Err rather than
    // crashing the watch thread mid-reindex. The watch loop recovers from a
    // returned `Err` by logging and skipping the file batch on the next tick —
    // much better than a hard crash that drops the entire daemon. The "should
    // be unreachable" invariant is preserved as a `tracing::error!` so any
    // real-world hit shows up in journald.
    let mut embeddings: Vec<Embedding> = Vec::with_capacity(chunk_count);
    for i in 0..chunk_count {
        match by_index.remove(&i) {
            Some(e) => embeddings.push(e),
            None => {
                tracing::error!(
                    chunk_index = i,
                    chunk_count,
                    by_index_remaining = by_index.len(),
                    "missing embedding at chunk index — upstream split lost a chunk; \
                     skipping this reindex batch (next tick will retry from SQLite)"
                );
                anyhow::bail!(
                    "watch reindex: chunk index {i} missing embedding (chunk_count={chunk_count}); \
                     daemon will retry on next tick"
                );
            }
        }
    }

    // Build calls_by_id directly from `per_file_chunk_calls` (collected by
    // `parse_file_all_with_chunk_calls` above) instead of re-parsing every
    // chunk's body with `extract_calls_from_chunk`, matching the bulk indexing
    // pipeline.
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
        // Hoist `root.join(file)` so the same PathBuf is reused by both the
        // mtime cache and the fingerprint write-back below. Joining twice per
        // file is ms-scale on WSL 9P, which adds up across a 200-file watch
        // tick.
        let abs_path = root.join(file);
        let mtime = *mtime_cache.entry(file.clone()).or_insert_with(|| {
            // Capture the stat error separately so we can surface it via
            // tracing instead of silently storing `mtime=None` for the file.
            // A `None` here means reconcile (`reconcile.rs:124-138`) treats the
            // entry as un-stat-able and skips it indefinitely, so the operator
            // needs an observable trail when the cause is a permission flip or
            // transient-AV-scan.
            match abs_path.metadata().and_then(|m| m.modified()) {
                Ok(t) => t
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    // Surface overflow as None (treated same as missing mtime)
                    // instead of silently wrapping past `i64::MAX` (~292M
                    // years). Real mtimes are nowhere near the cap, so this is
                    // functionally equivalent on every valid input.
                    .and_then(|d| i64::try_from(d.as_millis()).ok()),
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
        // O(1) lookup per chunk via pre-grouped HashMap instead of linear scan.
        let file_calls: Vec<_> = pairs
            .iter()
            .flat_map(|(c, _)| {
                calls_by_id
                    .get(&c.id)
                    .into_iter()
                    .flat_map(|calls| calls.iter().map(|call| (c.id.clone(), call.clone())))
            })
            .collect();
        // Upsert chunks+calls AND prune phantom chunks in one tx. Committing
        // the upsert and prune independently would leave the index half-pruned
        // (new chunks visible, removed chunks still present) alongside a dirty
        // HNSW flag on a crash between them. `upsert_chunks_calls_and_prune`
        // fuses both operations into a single `begin_write` transaction,
        // making the reindex all-or-nothing. The file-level `function_calls`
        // write folds into the same per-file tx so a mid-embed daemon crash
        // can't leave an asymmetric state.
        let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
        let file_fn_calls = all_function_calls
            .get(file)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        store.upsert_chunks_calls_and_prune_with_file_calls(
            pairs,
            mtime,
            &file_calls,
            Some(file.as_path()),
            &live_ids,
            Some(file_fn_calls),
        )?;

        // Populate the reconcile fingerprint columns (`source_size`,
        // `source_content_hash`) so the next `run_daemon_reconcile` pass can
        // fall back to BLAKE3 when mtime/size alone is unreliable (coarse-mtime
        // FAT32/NTFS/HFS+/SMB mounts; `git checkout` and formatter passes that
        // bump mtime without changing content). Compute size+hash on the same
        // file bytes we just parsed; the fingerprint UPDATE rides outside the
        // upsert transaction (best-effort), so a stat or read failure here only
        // forfeits the BLAKE3 tiebreak — the next save fires the same path.
        // Streaming blake3 + size-from-metadata avoids slurping the whole file
        // into RAM just to hash it. Reuse the `abs_path` hoisted above.
        let size_hint = std::fs::metadata(&abs_path).ok().map(|m| m.len());
        let fp = match std::fs::File::open(&abs_path) {
            Ok(f) => {
                let mut hasher = blake3::Hasher::new();
                match hasher.update_reader(std::io::BufReader::new(f)) {
                    Ok(_) => cqs::store::FileFingerprint {
                        mtime,
                        size: size_hint,
                        content_hash: Some(*hasher.finalize().as_bytes()),
                    },
                    Err(e) => {
                        tracing::debug!(
                            file = %file.display(),
                            error = %e,
                            "blake3 stream read failed; staleness fingerprint skipped"
                        );
                        cqs::store::FileFingerprint {
                            mtime,
                            size: size_hint,
                            content_hash: None,
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    path = %abs_path.display(),
                    error = %e,
                    "Reindex: read failed, leaving fingerprint partial (mtime only)"
                );
                cqs::store::FileFingerprint {
                    mtime,
                    size: None,
                    content_hash: None,
                }
            }
        };
        if let Err(e) = store.set_file_fingerprint(file, &fp) {
            tracing::warn!(
                path = %file.display(),
                error = %e,
                "Reindex: failed to set v23 fingerprint columns; reconcile will use mtime fallback"
            );
        }
    }

    // Any file that parsed to ZERO chunks is in `all_function_calls` but NOT
    // in `by_file` (the per-file upsert loop above only ran for files with at
    // least one chunk). `finalize_zero_chunk_files` handles each: it REPLACES
    // its function_calls from the freshly parsed call set (the partial route's
    // share of the single parse-driven writer — empty set clears an emptied
    // file's edges; NON-empty set refreshes an oversize-function file's edges,
    // which have zero chunks but real calls), prunes its stale chunks, and
    // stamps the v29 `file_registry` reconcile fingerprint #1774 so the next
    // reconcile tick skips it. The chunk-carrying files already wrote their
    // function_calls in the fused per-file tx and stamped their fingerprint
    // inline (`set_file_fingerprint`), which shadows into the registry too.
    let zero_chunk_entries: Vec<(PathBuf, Vec<cqs::parser::FunctionCalls>)> = all_function_calls
        .into_iter()
        .filter(|(rel_path, _)| !by_file.contains_key(rel_path))
        .collect();
    finalize_zero_chunk_files(store, root, &zero_chunk_entries);

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

    // Hold shared lock during read+index to prevent partial reads if another
    // process is writing notes concurrently (e.g., `cqs notes add`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::{Chunk, Embedding};

    fn seed_chunk(store: &Store, id: &str, origin: &str, name: &str, version: u32) {
        let chunk = Chunk {
            id: id.to_string(),
            file: PathBuf::from(origin),
            language: cqs::language::Language::Rust,
            chunk_type: cqs::language::ChunkType::Function,
            name: name.to_string(),
            signature: format!("pub fn {name}()"),
            content: format!("pub fn {name}() {{}}"),
            doc: None,
            line_start: 1,
            line_end: 1,
            byte_start: 0,
            content_hash: id.to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: version,
        };
        let emb = Embedding::new(vec![0.5; cqs::EMBEDDING_DIM]);
        store
            .upsert_chunks_batch(&[(chunk, emb)], Some(100))
            .unwrap();
    }

    /// Seed a single `function_calls` row: `caller` (in `origin`) calls `callee`.
    /// Used to exercise the caller-side orphan: when `origin`'s chunks are later
    /// emptied, this edge must be cleared, not left dangling.
    fn seed_call(store: &Store, origin: &str, caller: &str, callee: &str) {
        store
            .upsert_function_calls(
                &PathBuf::from(origin),
                &[cqs::parser::FunctionCalls {
                    name: caller.to_string(),
                    line_start: 1,
                    calls: vec![cqs::parser::CallSite {
                        callee_name: callee.to_string(),
                        line_number: 1,
                        kind: cqs::parser::CallEdgeKind::Call,
                    }],
                }],
            )
            .unwrap();
    }

    /// Build a single `finalize_zero_chunk_files` entry: a zero-chunk file with
    /// the given freshly-parsed call set (empty for the emptied case, non-empty
    /// for the oversize-function case).
    fn fz_entry(
        origin: &str,
        calls: Vec<cqs::parser::FunctionCalls>,
    ) -> (PathBuf, Vec<cqs::parser::FunctionCalls>) {
        (PathBuf::from(origin), calls)
    }

    /// Build a one-edge call set: `caller` calls `callee`.
    fn one_edge(caller: &str, callee: &str) -> Vec<cqs::parser::FunctionCalls> {
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

    /// Genuinely-emptied (comment-only) zero-chunk file, BOTH watch routes.
    ///
    /// The post-edit content parses to ZERO chunks AND an EMPTY call set, so the
    /// finalization replaces function_calls with the empty set (clearing the
    /// orphaned caller-side edge), prunes chunks/FTS, and stamps the registry.
    /// This is the #1830 / round-3 emptied-file repro — kept green. Both watch
    /// routes flow through `finalize_zero_chunk_files`; exercising it covers both.
    #[test]
    fn watch_zero_chunk_finalize_clears_function_calls_both_routes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("caller.rs"), "// only a comment now\n").unwrap();

        let store = Store::open(&root.join("index.db")).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Pre-edit: caller.rs had a chunk AND named `victim` as a callee.
        seed_chunk(
            &store,
            "caller.rs:1:c",
            "caller.rs",
            "caller_fn",
            cqs::parser_version(),
        );
        seed_call(&store, "caller.rs", "caller_fn", "victim");
        assert_eq!(
            store.get_chunks_by_origin("caller.rs").unwrap().len(),
            1,
            "precondition: seeded caller chunk"
        );
        assert_eq!(
            store.get_callers_full("victim").unwrap().len(),
            1,
            "precondition: victim has one caller edge"
        );

        // The emptied file's freshly parsed call set is EMPTY → replace clears.
        finalize_zero_chunk_files(&store, &root, &[fz_entry("caller.rs", vec![])]);

        // Full coherent end-state.
        assert!(
            store.get_chunks_by_origin("caller.rs").unwrap().is_empty(),
            "chunks pruned"
        );
        assert!(
            store.search_by_name("caller_fn", 5).unwrap().is_empty(),
            "chunks_fts pruned (no search hit)"
        );
        assert!(
            store.get_callers_full("victim").unwrap().is_empty(),
            "function_calls cleared: the deleted caller no longer cites victim"
        );
        let drifted = store
            .origins_with_parser_drift(&["caller.rs"], cqs::parser_version())
            .unwrap();
        assert!(
            drifted.is_empty(),
            "registry stamped, drift won't re-select"
        );
    }

    /// THE CRITICAL DECOUPLE REPRO — oversize-function file, BOTH watch routes.
    ///
    /// A file whose single function exceeds `CQS_PARSER_MAX_CHUNK_BYTES` parses
    /// to ZERO chunks but a NON-EMPTY call set (Pass 1 drops the oversize chunk;
    /// Pass 2 still emits the file-level call edges). "Zero chunks" is NOT "zero
    /// calls": the finalization must REPLACE function_calls with the real
    /// non-empty set — the edge MUST survive and `cqs callers` must still list
    /// the file. The six prior rounds used a chunk-frame signal to make this
    /// call-graph decision and would DELETE this legitimate edge, flipping the
    /// callee to false-DEAD. This test seeds the pre-edit state and re-finalizes
    /// with the non-empty set, asserting chunks gone but the edge refreshed.
    #[test]
    fn watch_zero_chunk_oversize_function_keeps_and_refreshes_edge() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        // On-disk content is irrelevant to finalize (it takes the parsed set
        // directly); a stub is enough for the registry-stamp disk read.
        std::fs::write(root.join("big.rs"), "// huge generated function\n").unwrap();

        let store = Store::open(&root.join("index.db")).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Pre-edit: big.rs had a chunk and called `helper`. (An earlier index
        // when the function was under the size cap, or a stale prior edge.)
        seed_chunk(
            &store,
            "big.rs:1:c",
            "big.rs",
            "big_fn",
            cqs::parser_version(),
        );
        seed_call(&store, "big.rs", "big_fn", "helper");
        assert_eq!(store.get_callers_full("helper").unwrap().len(), 1);

        // The function is now oversize → zero chunks, but the parser STILL
        // emitted the `big_fn → helper` edge. Finalize with that real set.
        finalize_zero_chunk_files(
            &store,
            &root,
            &[fz_entry("big.rs", one_edge("big_fn", "helper"))],
        );

        // Chunks pruned (function exceeds the byte cap, no chunk row)...
        assert!(
            store.get_chunks_by_origin("big.rs").unwrap().is_empty(),
            "oversize function produces zero chunks"
        );
        // ...but the call edge SURVIVED and refreshed: `cqs callers helper`
        // still lists big.rs, and `helper` is NOT flipped to dead.
        let callers = store.get_callers_full("helper").unwrap();
        assert_eq!(
            callers.len(),
            1,
            "oversize-function file's call edge MUST survive the zero-chunk finalize"
        );
        assert_eq!(callers[0].name, "big_fn");
        // The edge is live, so a callee referenced only by it is NOT dead.
        let (confident, _) = store.find_dead_code(true).unwrap();
        assert!(
            !confident.iter().any(|d| d.chunk.name == "helper"),
            "helper must NOT be dead — its caller edge is intact"
        );
    }

    /// Payload-poisoning repro from the audit: a callee referenced ONLY by a
    /// now-deleted caller must become dead-eligible once the caller's file is
    /// zeroed. `fetch_uncalled_functions`' `NOT EXISTS` does not join chunks on
    /// the CALLER side, so an orphaned `function_calls` row (caller chunk gone,
    /// edge surviving) falsely keeps the callee out of the dead set. Clearing
    /// the orphan in the zero-chunk finalization restores the correct verdict.
    #[test]
    fn watch_zero_chunk_orphan_no_longer_vetoes_dead() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("caller.rs"), "// emptied\n").unwrap();

        let store = Store::open(&root.join("index.db")).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // `victim` is a private function in its own file with no other callers.
        seed_chunk(
            &store,
            "victim.rs:1:v",
            "victim.rs",
            "victim",
            cqs::parser_version(),
        );
        // caller.rs is the ONLY thing that references victim.
        seed_chunk(
            &store,
            "caller.rs:1:c",
            "caller.rs",
            "caller_fn",
            cqs::parser_version(),
        );
        seed_call(&store, "caller.rs", "caller_fn", "victim");

        // Before zeroing: the orphan-to-be edge vetoes victim from the dead set.
        let (confident_before, _) = store.find_dead_code(true).unwrap();
        assert!(
            !confident_before.iter().any(|d| d.chunk.name == "victim"),
            "precondition: victim is held live by its (soon-orphaned) caller edge"
        );

        // caller.rs parses to zero chunks under watch (emptied → empty call
        // set) → finalize it.
        finalize_zero_chunk_files(&store, &root, &[fz_entry("caller.rs", vec![])]);

        // The orphan is gone, so victim is now dead-eligible.
        let (confident_after, _) = store.find_dead_code(true).unwrap();
        assert!(
            confident_after.iter().any(|d| d.chunk.name == "victim"),
            "after the caller file is zeroed, victim must become dead-eligible \
             (orphaned edge no longer vetoes the dead verdict)"
        );
    }

    /// #1830 repro: a file edited from has-chunks to zero-chunks under watch
    /// must have its OLD chunk rows pruned, not just the registry stamped.
    /// Before that fix the finalization only stamped the fingerprint, so the
    /// stale rows survived as ghosts and kept returning search hits.
    #[test]
    fn watch_zero_chunk_stamp_prunes_stale_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        // The post-edit on-disk content parses to zero chunks (comment only).
        std::fs::write(root.join("ghost.rs"), "// only a comment now\n").unwrap();

        let store = Store::open(&root.join("index.db")).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Pre-edit state: the file had two indexed chunks.
        seed_chunk(
            &store,
            "ghost.rs:1:a",
            "ghost.rs",
            "ghost_a",
            cqs::parser_version(),
        );
        seed_chunk(
            &store,
            "ghost.rs:2:b",
            "ghost.rs",
            "ghost_b",
            cqs::parser_version(),
        );
        assert_eq!(
            store.get_chunks_by_origin("ghost.rs").unwrap().len(),
            2,
            "precondition: two seeded chunk rows"
        );

        // Simulate the watch zero-chunk success path for this survivor.
        finalize_zero_chunk_files(&store, &root, &[fz_entry("ghost.rs", vec![])]);

        // The ghost rows are GONE — not just the registry stamped.
        assert!(
            store.get_chunks_by_origin("ghost.rs").unwrap().is_empty(),
            "stale chunk rows must be pruned, not left as searchable ghosts"
        );
        // And no search surfaces the ghost (FTS row pruned in the same tx).
        assert!(
            store.search_by_name("ghost_a", 5).unwrap().is_empty(),
            "pruned chunk must not return from search"
        );
        // The registry fingerprint was still stamped so the next tick skips it.
        let drifted = store
            .origins_with_parser_drift(&["ghost.rs"], cqs::parser_version())
            .unwrap();
        assert!(drifted.is_empty());
    }

    /// Round-3 drift-loop closure via the prune. A file with chunks at parser
    /// version N is registry-stamped; a PARSER_VERSION bump to N+1 makes it
    /// drift-eligible; the file now parses to zero chunks. The watch zero-chunk
    /// path must prune the drifted rows so `origins_with_parser_drift` no longer
    /// selects it on the next reconcile pass — closing the requeue loop without
    /// any change to the drift predicate.
    ///
    /// Before the fix: the stamp left the stale-version rows in place, so the
    /// second drift query re-selected the origin every tick forever.
    #[test]
    fn watch_zero_chunk_prune_closes_drift_loop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("drift.rs"), "// parses to zero chunks\n").unwrap();

        let store = Store::open(&root.join("index.db")).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Seed a chunk at the OLD parser version (current - 1): the PARSER_VERSION
        // bump on this branch is what makes the row drift-eligible.
        let stale_version = cqs::parser_version() - 1;
        seed_chunk(
            &store,
            "drift.rs:1:seed",
            "drift.rs",
            "drift_fn",
            stale_version,
        );

        // First reconcile pass: the drifted file IS selected.
        let pass1 = store
            .origins_with_parser_drift(&["drift.rs"], cqs::parser_version())
            .unwrap();
        assert!(
            pass1.contains("drift.rs"),
            "precondition: version-drifted chunk must be selected on pass 1"
        );

        // The file parses to zero chunks this tick → watch zero-chunk path runs.
        finalize_zero_chunk_files(&store, &root, &[fz_entry("drift.rs", vec![])]);

        // Second reconcile pass: the prune removed the drifted rows, so the
        // origin is no longer selected — the loop is closed.
        let pass2 = store
            .origins_with_parser_drift(&["drift.rs"], cqs::parser_version())
            .unwrap();
        assert!(
            !pass2.contains("drift.rs"),
            "after the prune the drifted origin must NOT be re-selected (loop closed)"
        );
        assert!(store.get_chunks_by_origin("drift.rs").unwrap().is_empty());
    }

    /// Run a single raw SQL statement against the on-disk DB through a separate
    /// connection, bypassing the `Store` wrapper. Used to inject a deterministic
    /// step-1 failure (drop `function_calls` out from under the finalize) and to
    /// read `file_registry` directly, independent of the chunk-UNION readers.
    fn raw_exec(db_path: &Path, sql: &str) {
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

    /// Count `file_registry` rows for an origin through a separate connection.
    fn raw_registry_count(db_path: &Path, origin: &str) -> i64 {
        use sqlx::sqlite::SqliteConnectOptions;
        use sqlx::ConnectOptions;
        let db_path = db_path.to_path_buf();
        let origin = origin.to_string();
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
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM file_registry WHERE origin = ?1")
                    .bind(&origin)
                    .fetch_one(&mut conn)
                    .await
                    .unwrap();
            n
        })
    }

    /// ERROR-PATH seam: a step-1 (function_calls replace) FAILURE must forfeit
    /// the prune AND the registry stamp — the steps are conditional on success,
    /// not merely ordered. The stamp-last sequence is crash-safe (nothing
    /// commits) but NOT error-safe: under the prior warn-and-continue, a
    /// transient calls-replace failure (e.g. SQLITE_BUSY past busy_timeout under
    /// a concurrent `cqs index --force`) still pruned the chunks and stamped a
    /// current fingerprint, committing stale calls + zero chunks + a current
    /// stamp — the campaign's ghost-caller / false-DEAD bug made PERMANENT
    /// (reconcile skips on fingerprint-match; the doctor's registry-UNION arm
    /// shields it).
    ///
    /// Injection: drop `function_calls` out from under the finalize so the
    /// replace's leading DELETE returns Err deterministically. A file being
    /// emptied (zero parsed chunks, empty call set) with a surviving chunk row
    /// is finalized.
    ///
    /// Fail-before (warn-and-continue): chunks pruned to zero AND registry
    /// stamped → the file is committed incoherent and reconcile-skipped.
    /// Pass-after (warn-and-return): chunks NOT pruned (the real row survives)
    /// and the registry is NOT stamped, so the next reconcile tick reclassifies
    /// and re-finalizes the file — it self-heals.
    #[test]
    fn watch_zero_chunk_calls_replace_failure_forfeits_prune_and_stamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let db_path = root.join("index.db");
        std::fs::write(root.join("caller.rs"), "// emptied\n").unwrap();

        let store = Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Pre-edit: caller.rs has a chunk and an existing call edge.
        seed_chunk(
            &store,
            "caller.rs:1:c",
            "caller.rs",
            "caller_fn",
            cqs::parser_version(),
        );
        seed_call(&store, "caller.rs", "caller_fn", "victim");
        assert_eq!(
            store.get_chunks_by_origin("caller.rs").unwrap().len(),
            1,
            "precondition: seeded caller chunk"
        );
        assert_eq!(
            raw_registry_count(&db_path, "caller.rs"),
            0,
            "precondition: caller.rs not yet stamped"
        );

        // Inject the step-1 failure: drop the table the calls-replace writes to,
        // so its leading DELETE returns Err.
        raw_exec(&db_path, "DROP TABLE function_calls");

        // Finalize the emptied file. The calls-replace fails; with the
        // conditional fix it returns early, forfeiting prune + stamp.
        finalize_zero_chunk_files(&store, &root, &[fz_entry("caller.rs", vec![])]);

        // PASS-AFTER assertions (fail-before would have pruned + stamped):
        assert_eq!(
            store.get_chunks_by_origin("caller.rs").unwrap().len(),
            1,
            "chunks must NOT be pruned after a calls-replace failure — the file \
             keeps its real row and re-finalizes next tick instead of committing \
             zero chunks under a stale call set"
        );
        assert_eq!(
            raw_registry_count(&db_path, "caller.rs"),
            0,
            "registry must NOT be stamped after a calls-replace failure — an \
             un-stamped file is reclassified and re-finalized next reconcile tick \
             (the designed heal trigger)"
        );
    }
}
