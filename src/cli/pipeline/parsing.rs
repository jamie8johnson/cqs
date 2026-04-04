//! Stage 1: Parse files in parallel batches, filter by staleness, send to embedder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::Sender;
use rayon::prelude::*;

use cqs::{normalize_path, Parser as CqParser, Store};

use super::types::{embed_batch_size, ParsedBatch, RelationshipData, FILE_BATCH_SIZE};
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
    let file_batch_size = FILE_BATCH_SIZE;

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
                    match parser.parse_file_all(&abs_path) {
                        Ok((mut chunks, function_calls, chunk_type_refs)) => {
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
                            // PERF-28: Extract per-chunk calls here (rayon parallel)
                            // instead of sequentially in store_stage.
                            for chunk in &chunks {
                                let calls = parser.extract_calls_from_chunk(chunk);
                                for call in calls {
                                    all_rels.chunk_calls.push((chunk.id.clone(), call));
                                }
                            }
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
            let mut remaining_rels = Some(batch_rels);
            for chunk_batch in chunks.chunks(batch_size) {
                let batch_mtimes: std::collections::HashMap<PathBuf, i64> = chunk_batch
                    .iter()
                    .filter_map(|c| file_mtimes.get(&c.file).map(|&m| (c.file.clone(), m)))
                    .collect();
                if parse_tx
                    .send(ParsedBatch {
                        chunks: chunk_batch.to_vec(),
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
