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

    let mut survivors = Vec::with_capacity(file_batch.len());
    let mut refreshes: Vec<(PathBuf, FileFingerprint)> = Vec::new();
    for (rel, origin) in file_batch.iter().zip(origins.iter()) {
        let abs_path = root.join(rel);
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

        // Parse surviving files in parallel, collecting chunks and relationships
        let (chunks, batch_rels): (Vec<cqs::Chunk>, RelationshipData) = survivors
            .par_iter()
            .fold(
                || (Vec::new(), RelationshipData::default()),
                |(mut all_chunks, mut all_rels), rel_path| {
                    let abs_path = root.join(rel_path);
                    match parser.parse_file_all_with_chunk_calls(&abs_path) {
                        Ok((mut chunks, function_calls, chunk_type_refs, mut chunk_calls)) => {
                            // Rewrite paths to be relative for storage
                            // Normalize path separators to forward slashes for cross-platform consistency
                            let path_str = normalize_path(rel_path);
                            // Build a map of old IDs -> new IDs for parent_id fixup
                            let id_map: std::collections::HashMap<String, String> = chunks
                                .iter()
                                .map(|chunk| {
                                    let hash_prefix =
                                        chunk.content_hash.get(..8).unwrap_or(&chunk.content_hash);
                                    let new_id = format!(
                                        "{}:{}:{}",
                                        path_str, chunk.line_start, hash_prefix
                                    );
                                    (chunk.id.clone(), new_id)
                                })
                                .collect();
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
                            // back in `path:line:hash` form (from
                            // `extract_chunk` using the absolute path); apply
                            // the same id_map we built above so they line up
                            // with the rewritten chunk ids.
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
                            if !function_calls.is_empty() {
                                all_rels
                                    .function_calls
                                    .entry(rel_path.clone())
                                    .or_default()
                                    .extend(function_calls);
                            }
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
                        }
                    }
                    (all_chunks, all_rels)
                },
            )
            .reduce(
                || (Vec::new(), RelationshipData::default()),
                |(mut chunks_a, mut rels_a), (chunks_b, rels_b)| {
                    chunks_a.extend(chunks_b);
                    for (file, refs) in rels_b.type_refs {
                        rels_a.type_refs.entry(file).or_default().extend(refs);
                    }
                    for (file, calls) in rels_b.function_calls {
                        rels_a.function_calls.entry(file).or_default().extend(calls);
                    }
                    rels_a.chunk_calls.extend(rels_b.chunk_calls);
                    (chunks_a, rels_a)
                },
            );

        // No post-parse staleness filter: only survivors of the pre-filter
        // were parsed, so every chunk and relationship here belongs to a
        // file that needs reindexing. Every parsed file has an entry in
        // `file_fingerprints` (the old relationship pruning by mtime map
        // membership is therefore a guaranteed no-op and was removed).

        parsed_count.fetch_add(file_batch.len(), Ordering::Relaxed);

        if !chunks.is_empty() {
            // Send in embedding-sized batches with per-file fingerprints and relationships.
            // Relationships are sent with the first batch only. Per-file data
            // (function_calls, type_refs) is safe. Per-chunk data (chunk_calls,
            // type_edges) is deferred in store_stage until all chunks are committed.
            //
            // Drain owned chunks into each batch instead of
            // `chunks.chunks(n)` + `.to_vec()`, which would clone every Chunk
            // (deep copy of id/file/signature/content/content_hash/...).
            // We own `chunks` here and never reuse it after this loop, so
            // moving each window out is safe and saves one full Chunk copy
            // per indexed chunk per batch.
            let mut remaining_rels = Some(batch_rels);
            let mut chunks = chunks;
            while !chunks.is_empty() {
                let take = batch_size.min(chunks.len());
                // Compute fingerprints from a borrow first; `drain` below
                // will move the same chunks out, so we can't borrow after
                // that.
                let batch_fps: std::collections::HashMap<PathBuf, FileFingerprint> = chunks[..take]
                    .iter()
                    .filter_map(|c| {
                        file_fingerprints
                            .get(&c.file)
                            .map(|fp| (c.file.clone(), fp.clone()))
                    })
                    .collect();
                let batch: Vec<cqs::Chunk> = chunks.drain(..take).collect();
                if parse_tx
                    .send(ParsedBatch {
                        chunks: batch,
                        relationships: remaining_rels.take().unwrap_or_default(),
                        file_fingerprints: batch_fps,
                    })
                    .is_err()
                {
                    break; // Receiver dropped
                }
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

    /// Fixture-driven regression test for the drain-based send loop. Builds a
    /// small fixture corpus, runs `parser_stage` end-to-end,
    /// and verifies:
    ///
    /// * every chunk the parser produced is delivered (no loss)
    /// * chunk IDs are unique across batches (drain did not alias data)
    /// * each batch respects `embed_batch_size()`
    /// * at least two batches are emitted (so the loop actually iterates)
    /// * relationships ride with exactly one batch
    ///
    /// The fixture produces >64 chunks so the default `embed_batch_size()`
    /// of 64 forces multiple iterations — avoids mutating the shared
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

        // Three fixture files. File `big.rs` has 70 trivial functions so the
        // total chunk count exceeds the default embed_batch_size (64),
        // guaranteeing at least two batches without touching env vars.
        let mut big = String::new();
        for i in 0..70 {
            use std::fmt::Write as _;
            writeln!(&mut big, "pub fn big_{i}() {{}}").unwrap();
        }
        std::fs::write(root.join("big.rs"), &big).unwrap();
        std::fs::write(root.join("a.rs"), "pub fn a_one() {}\npub fn a_two() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "pub fn b_one() {}\n").unwrap();

        let rel_paths: Vec<PathBuf> = vec![
            PathBuf::from("big.rs"),
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
        ];

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
        for b in &batches {
            assert!(!b.chunks.is_empty(), "empty batch should not be sent");
            assert!(
                b.chunks.len() <= max_batch,
                "batch must respect embed_batch_size={max_batch}, got {}",
                b.chunks.len()
            );
            total += b.chunks.len();
            for c in &b.chunks {
                assert!(ids.insert(c.id.clone()), "duplicated chunk id: {}", c.id);
            }
            let has_rels = !b.relationships.type_refs.is_empty()
                || !b.relationships.function_calls.is_empty()
                || !b.relationships.chunk_calls.is_empty();
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
            content_hash: "seed".to_string(),
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
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

        // Every parsed file ships a fully-populated fingerprint for the
        // store stage to stamp.
        for b in &batches {
            for c in &b.chunks {
                let fp = b
                    .file_fingerprints
                    .get(&c.file)
                    .unwrap_or_else(|| panic!("missing fingerprint for {:?}", c.file));
                assert!(fp.mtime.is_some(), "{:?} fingerprint needs mtime", c.file);
                assert!(fp.size.is_some(), "{:?} fingerprint needs size", c.file);
                assert!(
                    fp.content_hash.is_some(),
                    "{:?} fingerprint needs content hash",
                    c.file
                );
            }
        }
    }
}
