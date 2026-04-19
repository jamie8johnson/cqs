//! Stage 1: Parse files in parallel batches, filter by staleness, send to embedder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::Sender;
use rayon::prelude::*;

use cqs::{normalize_path, Parser as CqParser, Store};

use super::types::{embed_batch_size, file_batch_size, ParsedBatch, RelationshipData};
use crate::cli::check_interrupted;

/// CQ-39: Context struct for parser_stage to avoid too_many_arguments.
pub(super) struct ParserStageContext {
    pub root: PathBuf,
    pub force: bool,
    pub parser: Arc<CqParser>,
    pub store: Arc<Store>,
    pub parsed_count: Arc<AtomicUsize>,
    pub parse_errors: Arc<AtomicUsize>,
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
    } = ctx;
    let batch_size = embed_batch_size();
    let file_batch_size = file_batch_size();

    for (batch_idx, file_batch) in files.chunks(file_batch_size).enumerate() {
        if check_interrupted() {
            break;
        }

        tracing::info!(
            batch = batch_idx + 1,
            files = file_batch.len(),
            "Processing file batch"
        );

        // Parse files in parallel, collecting chunks and relationships
        let (chunks, batch_rels): (Vec<cqs::Chunk>, RelationshipData) = file_batch
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
                            // P2 #63: parse_file_all_with_chunk_calls already
                            // emitted (chunk_id, CallSite) pairs from Pass 2 —
                            // no per-chunk re-parse needed here. Chunk ids
                            // came back in `path:line:hash` form (from
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
                            tracing::warn!("Failed to parse {}: {}", abs_path.display(), e);
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

        // Filter by needs_reindex unless forced, caching mtime per-file to avoid double reads
        let mut file_mtimes: std::collections::HashMap<PathBuf, i64> =
            std::collections::HashMap::new();
        let chunks: Vec<cqs::Chunk> = if force {
            // Force mode: still need to get mtimes for storage
            for c in &chunks {
                if !file_mtimes.contains_key(&c.file) {
                    let abs_path = root.join(&c.file);
                    let mtime = abs_path
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    file_mtimes.insert(c.file.clone(), mtime);
                }
            }
            chunks
        } else {
            // Cache needs_reindex results per-file to avoid redundant DB queries
            // when multiple chunks come from the same file.
            let mut reindex_cache: HashMap<PathBuf, Option<i64>> = HashMap::new();
            chunks
                .into_iter()
                .filter(|c| {
                    if let Some(cached) = reindex_cache.get(&c.file) {
                        if let Some(mtime) = cached {
                            file_mtimes.entry(c.file.clone()).or_insert(*mtime);
                        }
                        return cached.is_some();
                    }
                    let abs_path = root.join(&c.file);
                    // needs_reindex returns Some(mtime) if reindex needed, None otherwise
                    match store.needs_reindex(&abs_path) {
                        Ok(Some(mtime)) => {
                            reindex_cache.insert(c.file.clone(), Some(mtime));
                            file_mtimes.insert(c.file.clone(), mtime);
                            true
                        }
                        Ok(None) => {
                            reindex_cache.insert(c.file.clone(), None);
                            false
                        }
                        Err(e) => {
                            tracing::warn!(file = %abs_path.display(), error = %e, "mtime check failed, reindexing");
                            true
                        }
                    }
                })
                .collect()
        };

        // Prune relationships to only include files that passed staleness filter
        let batch_rels = if force {
            batch_rels
        } else {
            // Build set of chunk IDs that survived the staleness filter
            let surviving_ids: std::collections::HashSet<&str> =
                chunks.iter().map(|c| c.id.as_str()).collect();
            RelationshipData {
                type_refs: batch_rels
                    .type_refs
                    .into_iter()
                    .filter(|(file, _)| file_mtimes.contains_key(file))
                    .collect(),
                function_calls: batch_rels
                    .function_calls
                    .into_iter()
                    .filter(|(file, _)| file_mtimes.contains_key(file))
                    .collect(),
                chunk_calls: batch_rels
                    .chunk_calls
                    .into_iter()
                    .filter(|(id, _)| surviving_ids.contains(id.as_str()))
                    .collect(),
            }
        };

        parsed_count.fetch_add(file_batch.len(), Ordering::Relaxed);

        if !chunks.is_empty() {
            // Send in embedding-sized batches with per-file mtimes and relationships.
            // Relationships are sent with the first batch only. Per-file data
            // (function_calls, type_refs) is safe. Per-chunk data (chunk_calls,
            // type_edges) is deferred in store_stage until all chunks are committed.
            //
            // PF-V1.25-18: drain owned chunks into each batch instead of
            // `chunks.chunks(n)` + `.to_vec()`, which would clone every Chunk
            // (deep copy of id/file/signature/content/content_hash/...).
            // We own `chunks` here and never reuse it after this loop, so
            // moving each window out is safe and saves one full Chunk copy
            // per indexed chunk per batch.
            let mut remaining_rels = Some(batch_rels);
            let mut chunks = chunks;
            while !chunks.is_empty() {
                let take = batch_size.min(chunks.len());
                // Compute mtimes from a borrow first; `drain` below will move
                // the same chunks out, so we can't borrow after that.
                let batch_mtimes: std::collections::HashMap<PathBuf, i64> = chunks[..take]
                    .iter()
                    .filter_map(|c| file_mtimes.get(&c.file).map(|&m| (c.file.clone(), m)))
                    .collect();
                let batch: Vec<cqs::Chunk> = chunks.drain(..take).collect();
                if parse_tx
                    .send(ParsedBatch {
                        chunks: batch,
                        relationships: remaining_rels.take().unwrap_or_default(),
                        file_mtimes: batch_mtimes,
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
    use super::*;
    use crossbeam_channel::unbounded;
    use std::collections::HashSet;

    /// PF-V1.25-18: fixture-driven regression test for the drain-based send
    /// loop. Builds a small fixture corpus, runs `parser_stage` end-to-end,
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
}
