//! Indexing pipeline for parsing, embedding, and storing code chunks
//!
//! Provides a 3-stage concurrent pipeline:
//! 1. Parser: Parse files in parallel batches
//! 2. Embedder: Embed chunks (GPU with CPU fallback)
//! 3. Writer: Write to SQLite

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use cqs::embedder::ModelConfig;
use cqs::parser::{CallSite, ChunkTypeRefs, FunctionCalls};
use cqs::{normalize_path, Chunk, Embedder, Embedding, Parser as CqParser, Store};

use super::check_interrupted;

// Windowing constants
//
// These values balance quality with memory/time constraints:
// - MAX_TOKENS_PER_WINDOW: E5-base-v2 has 512 token limit; we use 480 for safety
// - WINDOW_OVERLAP_TOKENS: 64 tokens overlap provides context continuity
pub(crate) const MAX_TOKENS_PER_WINDOW: usize = 480;
pub(crate) const WINDOW_OVERLAP_TOKENS: usize = 64;

// Pipeline tuning constants
/// Embedding batch size (backed off from 64 — crashed at 2%)
const EMBED_BATCH_SIZE: usize = 32;
/// Files to parse per batch (bounded memory)
const FILE_BATCH_SIZE: usize = 5_000;
/// Parse channel depth — lightweight (chunk metadata only), can be deeper
const PARSE_CHANNEL_DEPTH: usize = 512;
/// Embed channel depth — heavy (embedding vectors), smaller to bound memory
const EMBED_CHANNEL_DEPTH: usize = 64;

/// Apply windowing to chunks that exceed the token limit.
/// Long chunks are split into overlapping windows; short chunks pass through unchanged.
pub(crate) fn apply_windowing(chunks: Vec<Chunk>, embedder: &Embedder) -> Vec<Chunk> {
    let _span = tracing::info_span!("apply_windowing", chunk_count = chunks.len()).entered();
    let mut result = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        match embedder.split_into_windows(
            &chunk.content,
            MAX_TOKENS_PER_WINDOW,
            WINDOW_OVERLAP_TOKENS,
        ) {
            Ok(windows) if windows.len() == 1 => {
                // Fits in one window - pass through unchanged
                result.push(chunk);
            }
            Ok(windows) => {
                // Split into multiple windows
                let parent_id = chunk.id.clone();
                for (window_content, window_idx) in windows {
                    let window_hash = blake3::hash(window_content.as_bytes()).to_hex().to_string();
                    result.push(Chunk {
                        id: format!("{}:w{}", parent_id, window_idx),
                        file: chunk.file.clone(),
                        language: chunk.language,
                        chunk_type: chunk.chunk_type,
                        name: chunk.name.clone(),
                        signature: chunk.signature.clone(),
                        content: window_content,
                        doc: if window_idx == 0 {
                            chunk.doc.clone()
                        } else {
                            None
                        }, // Doc only on first window
                        line_start: chunk.line_start,
                        line_end: chunk.line_end,
                        content_hash: window_hash,
                        parent_id: Some(parent_id.clone()),
                        window_idx: Some(window_idx),
                        parent_type_name: chunk.parent_type_name.clone(),
                    });
                }
            }
            Err(e) => {
                // Tokenization failed - pass through unchanged and hope for the best
                tracing::warn!(chunk_id = %chunk.id, error = %e, "Windowing failed, passing through");
                result.push(chunk);
            }
        }
    }

    result
}

/// Relationship data extracted during parsing, keyed by file path.
/// Threaded through the pipeline so store_stage can persist without re-reading files.
#[derive(Clone, Default)]
struct RelationshipData {
    type_refs: HashMap<PathBuf, Vec<ChunkTypeRefs>>,
    function_calls: HashMap<PathBuf, Vec<FunctionCalls>>,
    /// Per-chunk call sites for the `calls` table (PERF-28: extracted during parse stage
    /// to avoid re-parsing in store_stage). Keyed by chunk ID.
    chunk_calls: Vec<(String, CallSite)>,
}

/// Message types for the pipelined indexer
struct ParsedBatch {
    chunks: Vec<Chunk>,
    relationships: RelationshipData,
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
}

struct EmbeddedBatch {
    chunk_embeddings: Vec<(Chunk, Embedding)>,
    relationships: RelationshipData,
    cached_count: usize,
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
}

/// Stats returned from pipelined indexing
pub(crate) struct PipelineStats {
    pub total_embedded: usize,
    pub total_cached: usize,
    pub gpu_failures: usize,
    pub parse_errors: usize,
    pub total_type_edges: usize,
    pub total_calls: usize,
}

/// Result of preparing a batch for embedding.
///
/// Separates chunks into those with cached embeddings vs those needing embedding.
struct PreparedEmbedding {
    /// Chunks with existing embeddings (from cache)
    cached: Vec<(Chunk, Embedding)>,
    /// Chunks that need new embeddings
    to_embed: Vec<Chunk>,
    /// NL descriptions for chunks needing embedding
    texts: Vec<String>,
    /// Relationships extracted during parsing
    relationships: RelationshipData,
    /// File modification times (per-file)
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
}

/// Prepare a batch for embedding: apply windowing, check cache, generate texts.
///
/// This consolidates the common logic between GPU and CPU embedder threads:
/// 1. Apply windowing to split long chunks
/// 2. Check store for cached embeddings by content hash
/// 3. Separate into cached (reuse) vs to_embed (need new embedding)
/// 4. Generate NL descriptions for chunks needing embedding
fn prepare_for_embedding(
    batch: ParsedBatch,
    embedder: &Embedder,
    store: &Store,
) -> PreparedEmbedding {
    use cqs::generate_nl_description;

    // Step 1: Apply windowing to split long chunks into overlapping windows
    let windowed_chunks = apply_windowing(batch.chunks, embedder);

    // Step 2: Check for existing embeddings by content hash
    let hashes: Vec<&str> = windowed_chunks
        .iter()
        .map(|c| c.content_hash.as_str())
        .collect();
    let existing = match store.get_embeddings_by_hashes(&hashes) {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch cached embeddings by hash");
            HashMap::new()
        }
    };

    // Step 3: Separate into cached vs to_embed
    let mut to_embed: Vec<Chunk> = Vec::new();
    let mut cached: Vec<(Chunk, Embedding)> = Vec::new();

    for chunk in windowed_chunks {
        if let Some(emb) = existing.get(&chunk.content_hash) {
            cached.push((chunk, emb.clone()));
        } else {
            to_embed.push(chunk);
        }
    }

    // Step 4: Generate NL descriptions for chunks needing embedding
    let texts: Vec<String> = to_embed.iter().map(generate_nl_description).collect();

    PreparedEmbedding {
        cached,
        to_embed,
        texts,
        relationships: batch.relationships,
        file_mtimes: batch.file_mtimes,
    }
}

/// Create an EmbeddedBatch from cached and newly embedded chunks.
fn create_embedded_batch(
    cached: Vec<(Chunk, Embedding)>,
    to_embed: Vec<Chunk>,
    new_embeddings: Vec<Embedding>,
    relationships: RelationshipData,
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
) -> EmbeddedBatch {
    let cached_count = cached.len();
    let mut chunk_embeddings = cached;
    chunk_embeddings.extend(to_embed.into_iter().zip(new_embeddings));
    EmbeddedBatch {
        chunk_embeddings,
        relationships,
        cached_count,
        file_mtimes,
    }
}

/// Flush a GPU-rejected batch to CPU: send cached results to the writer channel,
/// requeue un-embedded chunks to the CPU fallback channel.
///
/// Returns `false` if either channel send fails (receiver dropped), signaling
/// the caller to break out of its loop.
fn flush_to_cpu(
    prepared: PreparedEmbedding,
    embed_tx: &Sender<EmbeddedBatch>,
    fail_tx: &Sender<ParsedBatch>,
    embedded_count: &AtomicUsize,
) -> bool {
    if !prepared.cached.is_empty() {
        let cached_count = prepared.cached.len();
        embedded_count.fetch_add(cached_count, Ordering::Relaxed);
        // Send relationships with cached batch only if there's nothing to requeue
        let rels = if prepared.to_embed.is_empty() {
            prepared.relationships.clone()
        } else {
            RelationshipData::default()
        };
        if embed_tx
            .send(EmbeddedBatch {
                chunk_embeddings: prepared.cached,
                relationships: rels,
                cached_count,
                file_mtimes: prepared.file_mtimes.clone(),
            })
            .is_err()
        {
            return false;
        }
    }
    // Send relationships with the requeued batch so they reach store_stage via CPU path
    let rels = if prepared.to_embed.is_empty() {
        RelationshipData::default()
    } else {
        prepared.relationships
    };
    if fail_tx
        .send(ParsedBatch {
            chunks: prepared.to_embed,
            relationships: rels,
            file_mtimes: prepared.file_mtimes,
        })
        .is_err()
    {
        return false;
    }
    true
}

/// Stage 1: Parse files in parallel batches, filter by staleness, and send to embedder channels.
#[allow(clippy::too_many_arguments)]
fn parser_stage(
    files: Vec<PathBuf>,
    root: PathBuf,
    force: bool,
    parser: Arc<CqParser>,
    store: Arc<Store>,
    parsed_count: Arc<AtomicUsize>,
    parse_errors: Arc<AtomicUsize>,
    parse_tx: Sender<ParsedBatch>,
) -> Result<()> {
    let batch_size = EMBED_BATCH_SIZE;
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
        let (chunks, batch_rels): (Vec<Chunk>, RelationshipData) = file_batch
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
                            // Build a map of old IDs → new IDs for parent_id fixup
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
        let chunks: Vec<Chunk> = if force {
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

/// Stage 2a: GPU embedder — embed chunks, requeue failures to CPU fallback.
fn gpu_embed_stage(
    parse_rx: Receiver<ParsedBatch>,
    embed_tx: Sender<EmbeddedBatch>,
    fail_tx: Sender<ParsedBatch>,
    store: Arc<Store>,
    embedded_count: Arc<AtomicUsize>,
    gpu_failures: Arc<AtomicUsize>,
    model_config: ModelConfig,
) -> Result<()> {
    let _span = tracing::info_span!("embed_thread", mode = "gpu").entered();
    let embedder = Embedder::new(model_config).context("Failed to initialize GPU embedder")?;
    embedder.warm().context("Failed to warm GPU embedder")?;

    for batch in parse_rx {
        if check_interrupted() {
            break;
        }

        // Use shared preparation logic (windowing + cache check + NL generation)
        let prepared = prepare_for_embedding(batch, &embedder, &store);

        if prepared.to_embed.is_empty() {
            // All cached, send directly
            let cached_count = prepared.cached.len();
            embedded_count.fetch_add(cached_count, Ordering::Relaxed);
            if embed_tx
                .send(EmbeddedBatch {
                    chunk_embeddings: prepared.cached,
                    relationships: prepared.relationships,
                    cached_count,
                    file_mtimes: prepared.file_mtimes,
                })
                .is_err()
            {
                break;
            }
            continue;
        }

        let max_len = prepared.texts.iter().map(|t| t.len()).max().unwrap_or(0);

        // Pre-filter long batches to CPU (GPU hits CUDNN limits >8k chars)
        if max_len > 8000 {
            tracing::warn!(
                chunks = prepared.to_embed.len(),
                max_len,
                "Routing long batch to CPU (GPU CUDNN limit)"
            );
            if !flush_to_cpu(prepared, &embed_tx, &fail_tx, &embedded_count) {
                break;
            }
            continue;
        }

        let text_refs: Vec<&str> = prepared.texts.iter().map(|s| s.as_str()).collect();
        match embedder.embed_documents(&text_refs) {
            Ok(embs) => {
                let new_embeddings: Vec<Embedding> = embs;
                let cached_count = prepared.cached.len();
                let mut chunk_embeddings = prepared.cached;
                chunk_embeddings.extend(prepared.to_embed.into_iter().zip(new_embeddings));
                embedded_count.fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
                if embed_tx
                    .send(EmbeddedBatch {
                        chunk_embeddings,
                        relationships: prepared.relationships,
                        cached_count,
                        file_mtimes: prepared.file_mtimes,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Err(e) => {
                // GPU failed - log details, then flush cached + requeue to CPU
                gpu_failures.fetch_add(prepared.to_embed.len(), Ordering::Relaxed);
                let files: Vec<_> = prepared
                    .to_embed
                    .iter()
                    .map(|c| c.file.display().to_string())
                    .collect();
                tracing::warn!(
                    error = %e,
                    chunks = prepared.to_embed.len(),
                    max_len,
                    ?files,
                    "GPU embedding failed, requeueing to CPU"
                );
                if !flush_to_cpu(prepared, &embed_tx, &fail_tx, &embedded_count) {
                    break;
                }
            }
        }
    }
    drop(fail_tx); // Signal CPU thread to finish when done
    tracing::debug!("GPU embedder thread finished");
    Ok(())
}

/// Stage 2b: CPU embedder — handles GPU failures + overflow (GPU gets priority).
///
/// CPU embedder is lazy-initialized on first batch to save ~500MB when GPU handles everything.
fn cpu_embed_stage(
    parse_rx: Receiver<ParsedBatch>,
    fail_rx: Receiver<ParsedBatch>,
    embed_tx: Sender<EmbeddedBatch>,
    store: Arc<Store>,
    embedded_count: Arc<AtomicUsize>,
    model_config: ModelConfig,
) -> Result<()> {
    let _span = tracing::info_span!("embed_thread", mode = "cpu").entered();
    let mut embedder: Option<Embedder> = None;

    loop {
        if check_interrupted() {
            break;
        }

        // Race: GPU and CPU both grab from parse_rx, CPU also handles routed long batches
        let batch = select! {
            recv(fail_rx) -> msg => match msg {
                Ok(b) => b,
                Err(_) => match parse_rx.recv() {
                    Ok(b) => b,
                    Err(_) => break,
                },
            },
            recv(parse_rx) -> msg => match msg {
                Ok(b) => b,
                Err(_) => match fail_rx.recv() {
                    Ok(b) => b,
                    Err(_) => break,
                },
            },
        };

        // Lazy-init CPU embedder on first batch
        let emb = match &embedder {
            Some(e) => e,
            None => {
                let e = Embedder::new_cpu(model_config.clone())
                    .context("Failed to initialize CPU embedder")?;
                embedder.insert(e)
            }
        };

        // Prepare batch: windowing, cache check, text generation
        let prepared = prepare_for_embedding(batch, emb, &store);

        // Embed new chunks (CPU only)
        let new_embeddings: Vec<Embedding> = if prepared.to_embed.is_empty() {
            vec![]
        } else {
            let text_refs: Vec<&str> = prepared.texts.iter().map(|s| s.as_str()).collect();
            emb.embed_documents(&text_refs)?
        };

        let embedded_batch = create_embedded_batch(
            prepared.cached,
            prepared.to_embed,
            new_embeddings,
            prepared.relationships,
            prepared.file_mtimes,
        );

        embedded_count.fetch_add(embedded_batch.chunk_embeddings.len(), Ordering::Relaxed);

        if embed_tx.send(embedded_batch).is_err() {
            break; // Receiver dropped
        }
    }
    tracing::debug!("CPU embedder thread finished");
    Ok(())
}

/// Stage 3: Write embedded chunks to SQLite with call graph, function calls, and type edges.
///
/// Returns `(total_embedded, total_cached, total_type_edges, total_calls)` counts.
fn store_stage(
    embed_rx: Receiver<EmbeddedBatch>,
    store: &Store,
    parsed_count: &AtomicUsize,
    embedded_count: &AtomicUsize,
    progress: &ProgressBar,
) -> Result<(usize, usize, usize, usize)> {
    let mut total_embedded = 0;
    let mut total_cached = 0;
    let mut total_type_edges = 0;
    let mut total_calls = 0;
    let mut deferred_type_edges: Vec<(PathBuf, Vec<ChunkTypeRefs>)> = Vec::new();
    let mut deferred_chunk_calls: Vec<(String, CallSite)> = Vec::new();

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
        let no_calls: Vec<(String, CallSite)> = Vec::new();

        // Upsert chunks WITHOUT calls (calls are deferred)
        if batch.file_mtimes.len() <= 1 {
            // Fast path: single file or no mtimes
            let mtime = batch.file_mtimes.values().next().copied();
            store.upsert_chunks_and_calls(&batch.chunk_embeddings, mtime, &no_calls)?;
        } else {
            // Multi-file batch: group by file and upsert with correct per-file mtime.
            let mut by_file: std::collections::HashMap<PathBuf, Vec<(Chunk, Embedding)>> =
                std::collections::HashMap::new();
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
    }

    // Insert deferred chunk calls now that all chunks are in the DB.
    // chunk_calls reference caller_id with FK on chunks(id), so they
    // must be inserted after all chunks across all batches are committed.
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

    // Insert deferred type edges now that all chunks are in the DB.
    // Type edges reference source_chunk_id with a FK constraint, so they
    // must be inserted after all chunks across all batches are committed.
    // PERF-26: Single transaction for all files instead of per-file transactions.
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

/// Run the indexing pipeline with 3 concurrent stages:
/// 1. Parser: Parse files in parallel batches
/// 2. Embedder: Embed chunks (GPU with CPU fallback)
/// 3. Writer: Write to SQLite
pub(crate) fn run_index_pipeline(
    root: &Path,
    files: Vec<PathBuf>,
    store: Arc<Store>,
    force: bool,
    quiet: bool,
    model_config: ModelConfig,
) -> Result<PipelineStats> {
    let _span = tracing::info_span!("run_index_pipeline", file_count = files.len()).entered();
    let total_files = files.len();

    // Channels
    let (parse_tx, parse_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) =
        bounded(PARSE_CHANNEL_DEPTH);
    let (embed_tx, embed_rx): (Sender<EmbeddedBatch>, Receiver<EmbeddedBatch>) =
        bounded(EMBED_CHANNEL_DEPTH);
    let (fail_tx, fail_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) =
        bounded(EMBED_CHANNEL_DEPTH);

    // Shared state
    let parser = Arc::new(CqParser::new().context("Failed to initialize parser")?);
    let parsed_count = Arc::new(AtomicUsize::new(0));
    let embedded_count = Arc::new(AtomicUsize::new(0));
    let gpu_failures = Arc::new(AtomicUsize::new(0));
    let parse_errors = Arc::new(AtomicUsize::new(0));

    // CPU embedder also races on parse_rx
    let parse_rx_cpu = parse_rx.clone();
    let embed_tx_cpu = embed_tx.clone();

    // Stage 1: Parser thread
    let parser_handle = {
        let parser = Arc::clone(&parser);
        let store = Arc::clone(&store);
        let parsed_count = Arc::clone(&parsed_count);
        let parse_errors = Arc::clone(&parse_errors);
        let root = root.to_path_buf();
        thread::spawn(move || {
            parser_stage(
                files,
                root,
                force,
                parser,
                store,
                parsed_count,
                parse_errors,
                parse_tx,
            )
        })
    };

    // Stage 2a: GPU embedder thread
    let gpu_model = model_config.clone();
    let gpu_handle = {
        let store = Arc::clone(&store);
        let embedded_count = Arc::clone(&embedded_count);
        let gpu_failures = Arc::clone(&gpu_failures);
        thread::spawn(move || {
            gpu_embed_stage(
                parse_rx,
                embed_tx,
                fail_tx,
                store,
                embedded_count,
                gpu_failures,
                gpu_model,
            )
        })
    };

    // Stage 2b: CPU embedder thread
    let cpu_model = model_config;
    let cpu_handle = {
        let store = Arc::clone(&store);
        let embedded_count = Arc::clone(&embedded_count);
        thread::spawn(move || {
            cpu_embed_stage(
                parse_rx_cpu,
                fail_rx,
                embed_tx_cpu,
                store,
                embedded_count,
                cpu_model,
            )
        })
    };

    // Stage 3: Writer (main thread)
    let progress = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(total_files as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {msg}")
                .unwrap_or_else(|e| {
                    tracing::warn!("Progress template error: {}, using default", e);
                    ProgressStyle::default_bar()
                }),
        );
        pb
    };

    let (total_embedded, total_cached, total_type_edges, total_calls) =
        store_stage(embed_rx, &store, &parsed_count, &embedded_count, &progress)?;

    progress.finish_with_message("done");

    // Wait for threads to finish
    parser_handle
        .join()
        .map_err(|e| anyhow::anyhow!("Parser thread panicked: {}", panic_message(&e)))??;
    gpu_handle
        .join()
        .map_err(|e| anyhow::anyhow!("GPU embedder thread panicked: {}", panic_message(&e)))??;
    cpu_handle
        .join()
        .map_err(|e| anyhow::anyhow!("CPU embedder thread panicked: {}", panic_message(&e)))??;

    // Update the "updated_at" metadata timestamp
    if let Err(e) = store.touch_updated_at() {
        tracing::warn!(error = %e, "Failed to update timestamp");
    }

    let stats = PipelineStats {
        total_embedded,
        total_cached,
        gpu_failures: gpu_failures.load(Ordering::Relaxed),
        parse_errors: parse_errors.load(Ordering::Relaxed),
        total_type_edges,
        total_calls,
    };

    tracing::info!(
        total_embedded = stats.total_embedded,
        total_cached = stats.total_cached,
        gpu_failures = stats.gpu_failures,
        parse_errors = stats.parse_errors,
        total_type_edges = stats.total_type_edges,
        total_calls = stats.total_calls,
        "Pipeline indexing complete"
    );

    Ok(stats)
}

/// Extract a human-readable message from a thread panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::language::{ChunkType, Language};

    /// Creates a test Chunk with minimal configuration for testing purposes.
    ///
    /// # Arguments
    ///
    /// * `id` - A string identifier for the chunk, used as both the chunk ID and name
    /// * `content` - The source code content to be stored in the chunk
    ///
    /// # Returns
    ///
    /// A new `Chunk` instance with:
    /// - File path set to "test.rs"
    /// - Language set to Rust
    /// - Chunk type set to Function
    /// - Content hash computed from the provided content
    /// - Line range from 1 to 10
    /// - All optional fields set to None or empty
    fn make_test_chunk(id: &str, content: &str) -> Chunk {
        Chunk {
            id: id.to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: id.to_string(),
            signature: String::new(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 10,
            content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    fn test_mtimes(mtime: i64) -> std::collections::HashMap<PathBuf, i64> {
        let mut m = std::collections::HashMap::new();
        m.insert(PathBuf::from("test.rs"), mtime);
        m
    }

    #[test]
    fn test_create_embedded_batch_all_cached() {
        let chunk = make_test_chunk("c1", "fn foo() {}");
        let emb = Embedding::new(vec![0.0; 768]);
        let cached = vec![(chunk, emb)];

        let batch = create_embedded_batch(
            cached,
            vec![],
            vec![],
            RelationshipData::default(),
            test_mtimes(12345),
        );
        assert_eq!(batch.chunk_embeddings.len(), 1);
        assert_eq!(batch.cached_count, 1);
        assert_eq!(batch.file_mtimes[&PathBuf::from("test.rs")], 12345);
    }

    #[test]
    fn test_create_embedded_batch_all_new() {
        let chunk = make_test_chunk("c1", "fn foo() {}");
        let emb = Embedding::new(vec![1.0; 768]);

        let batch = create_embedded_batch(
            vec![],
            vec![chunk],
            vec![emb],
            RelationshipData::default(),
            test_mtimes(99),
        );
        assert_eq!(batch.chunk_embeddings.len(), 1);
        assert_eq!(batch.cached_count, 0);
        assert_eq!(batch.file_mtimes[&PathBuf::from("test.rs")], 99);
    }

    #[test]
    fn test_create_embedded_batch_mixed() {
        let cached_chunk = make_test_chunk("c1", "fn foo() {}");
        let cached_emb = Embedding::new(vec![0.0; 768]);
        let new_chunk = make_test_chunk("c2", "fn bar() {}");
        let new_emb = Embedding::new(vec![1.0; 768]);

        let batch = create_embedded_batch(
            vec![(cached_chunk, cached_emb)],
            vec![new_chunk],
            vec![new_emb],
            RelationshipData::default(),
            test_mtimes(12345),
        );
        assert_eq!(batch.chunk_embeddings.len(), 2);
        assert_eq!(batch.cached_count, 1);
    }

    #[test]
    fn test_create_embedded_batch_empty() {
        let batch = create_embedded_batch(
            vec![],
            vec![],
            vec![],
            RelationshipData::default(),
            std::collections::HashMap::new(),
        );
        assert_eq!(batch.chunk_embeddings.len(), 0);
        assert_eq!(batch.cached_count, 0);
    }

    #[test]
    fn test_create_embedded_batch_preserves_order() {
        let c1 = make_test_chunk("c1", "fn first() {}");
        let e1 = Embedding::new(vec![1.0; 768]);
        let c2 = make_test_chunk("c2", "fn second() {}");
        let e2 = Embedding::new(vec![2.0; 768]);
        let c3 = make_test_chunk("c3", "fn third() {}");
        let e3 = Embedding::new(vec![3.0; 768]);

        let batch = create_embedded_batch(
            vec![(c1, e1)],
            vec![c2, c3],
            vec![e2, e3],
            RelationshipData::default(),
            test_mtimes(0),
        );

        assert_eq!(batch.chunk_embeddings.len(), 3);
        // Cached come first, then new in order
        assert_eq!(batch.chunk_embeddings[0].0.id, "c1");
        assert_eq!(batch.chunk_embeddings[1].0.id, "c2");
        assert_eq!(batch.chunk_embeddings[2].0.id, "c3");
    }

    #[test]
    fn test_windowing_constants() {
        // Verify constants are sensible (const blocks for compile-time checks)
        const { assert!(MAX_TOKENS_PER_WINDOW <= 512) };
        const { assert!(WINDOW_OVERLAP_TOKENS < MAX_TOKENS_PER_WINDOW) };
        const { assert!(WINDOW_OVERLAP_TOKENS > 0) };
    }

    #[test]
    #[ignore] // Requires model
    fn test_apply_windowing_empty() {
        let embedder = Embedder::new_cpu(ModelConfig::resolve(None, None)).unwrap();
        let result = apply_windowing(vec![], &embedder);
        assert!(result.is_empty());
    }

    #[test]
    #[ignore] // Requires model
    fn test_apply_windowing_short_chunk() {
        let embedder = Embedder::new_cpu(ModelConfig::resolve(None, None)).unwrap();
        let mut chunk = make_test_chunk("short1", "fn foo() {}");
        chunk.doc = Some("A short function".to_string());

        let result = apply_windowing(vec![chunk], &embedder);

        assert_eq!(result.len(), 1);
        let c = &result[0];
        assert_eq!(c.id, "short1");
        assert_eq!(c.name, "short1");
        assert_eq!(c.doc, Some("A short function".to_string()));
        assert_eq!(c.parent_id, None, "short chunk should not have parent_id");
        assert_eq!(c.window_idx, None, "short chunk should not have window_idx");
        assert_eq!(c.file, PathBuf::from("test.rs"));
        assert_eq!(c.language, Language::Rust);
        assert_eq!(c.chunk_type, ChunkType::Function);
        assert_eq!(c.content, "fn foo() {}");
    }

    #[test]
    #[ignore] // Requires model
    fn test_apply_windowing_long_chunk() {
        let embedder = Embedder::new_cpu(ModelConfig::resolve(None, None)).unwrap();

        // Build content that exceeds 480 tokens. Each line is a unique function body.
        // ~500 lines of "let varN = N;\n" should comfortably exceed the token limit.
        let long_content: String = (0..500)
            .map(|i| format!("    let variable_{i} = {i};\n"))
            .collect();
        let content = format!("fn big_function() {{\n{long_content}}}");

        let mut chunk = make_test_chunk("long1", &content);
        chunk.doc = Some("A very long function".to_string());
        chunk.line_start = 10;
        chunk.line_end = 520;
        chunk.parent_type_name = Some("MyStruct".to_string());

        let original_id = chunk.id.clone();
        let result = apply_windowing(vec![chunk], &embedder);

        assert!(
            result.len() > 1,
            "Expected multiple windows, got {}",
            result.len()
        );

        for (i, window) in result.iter().enumerate() {
            let idx = i as u32;

            // ID format: "{parent_id}:w{idx}"
            assert_eq!(
                window.id,
                format!("{original_id}:w{idx}"),
                "window {i} has wrong id"
            );

            // parent_id set on all windows
            assert_eq!(
                window.parent_id,
                Some(original_id.clone()),
                "window {i} missing parent_id"
            );

            // window_idx set correctly
            assert_eq!(
                window.window_idx,
                Some(idx),
                "window {i} has wrong window_idx"
            );

            // Shared fields from parent
            assert_eq!(window.file, PathBuf::from("test.rs"));
            assert_eq!(window.language, Language::Rust);
            assert_eq!(window.chunk_type, ChunkType::Function);
            assert_eq!(window.name, "long1");
            assert_eq!(window.line_start, 10);
            assert_eq!(window.line_end, 520);
            assert_eq!(window.parent_type_name, Some("MyStruct".to_string()));

            // Content hash is blake3 of the window content
            let expected_hash = blake3::hash(window.content.as_bytes()).to_hex().to_string();
            assert_eq!(
                window.content_hash, expected_hash,
                "window {i} hash mismatch"
            );

            // Non-empty content
            assert!(!window.content.is_empty(), "window {i} has empty content");
        }

        // First window gets doc, subsequent windows do not
        assert_eq!(
            result[0].doc,
            Some("A very long function".to_string()),
            "first window should preserve doc"
        );
        for window in &result[1..] {
            assert_eq!(window.doc, None, "non-first window should have doc = None");
        }
    }
}
