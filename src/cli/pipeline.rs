//! Indexing pipeline for parsing, embedding, and storing code chunks
//!
//! Provides a 3-stage concurrent pipeline:
//! 1. Parser: Parse files in parallel batches
//! 2. Embedder: Embed chunks (GPU with CPU fallback)
//! 3. Writer: Write to SQLite

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use cqs::{Chunk, Embedder, Embedding, Parser as CqParser, Store};

use super::check_interrupted;

// Windowing constants
//
// These values balance quality with memory/time constraints:
// - MAX_TOKENS_PER_WINDOW: E5-base-v2 has 512 token limit; we use 480 for safety
// - WINDOW_OVERLAP_TOKENS: 64 tokens overlap provides context continuity
const MAX_TOKENS_PER_WINDOW: usize = 480;
const WINDOW_OVERLAP_TOKENS: usize = 64;

/// Apply windowing to chunks that exceed the token limit.
/// Long chunks are split into overlapping windows; short chunks pass through unchanged.
fn apply_windowing(chunks: Vec<Chunk>, embedder: &Embedder) -> Vec<Chunk> {
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
                    });
                }
            }
            Err(e) => {
                // Tokenization failed - pass through unchanged and hope for the best
                tracing::warn!("Windowing failed for {}: {}, passing through", chunk.id, e);
                result.push(chunk);
            }
        }
    }

    result
}

/// Message types for the pipelined indexer
struct ParsedBatch {
    chunks: Vec<Chunk>,
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
}

struct EmbeddedBatch {
    chunk_embeddings: Vec<(Chunk, Embedding)>,
    cached_count: usize,
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
}

/// Stats returned from pipelined indexing
pub(crate) struct PipelineStats {
    pub total_embedded: usize,
    pub total_cached: usize,
    pub gpu_failures: usize,
    pub parse_errors: usize,
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
    let existing = store.get_embeddings_by_hashes(&hashes);

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
        file_mtimes: batch.file_mtimes,
    }
}

/// Create an EmbeddedBatch from cached and newly embedded chunks.
fn create_embedded_batch(
    cached: Vec<(Chunk, Embedding)>,
    to_embed: Vec<Chunk>,
    new_embeddings: Vec<Embedding>,
    file_mtimes: std::collections::HashMap<PathBuf, i64>,
) -> EmbeddedBatch {
    let cached_count = cached.len();
    let mut chunk_embeddings = cached;
    chunk_embeddings.extend(to_embed.into_iter().zip(new_embeddings));
    EmbeddedBatch {
        chunk_embeddings,
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
        if embed_tx
            .send(EmbeddedBatch {
                chunk_embeddings: prepared.cached,
                cached_count,
                file_mtimes: prepared.file_mtimes.clone(),
            })
            .is_err()
        {
            return false;
        }
    }
    if fail_tx
        .send(ParsedBatch {
            chunks: prepared.to_embed,
            file_mtimes: prepared.file_mtimes,
        })
        .is_err()
    {
        return false;
    }
    true
}

/// Run the indexing pipeline with 3 concurrent stages:
/// 1. Parser: Parse files in parallel batches
/// 2. Embedder: Embed chunks (GPU)
/// 3. Writer: Write to SQLite
pub(crate) fn run_index_pipeline(
    root: &Path,
    files: Vec<PathBuf>,
    store_path: &Path,
    force: bool,
    quiet: bool,
) -> Result<PipelineStats> {
    let batch_size = 32; // Embedding batch size (backed off from 64 - crashed at 2%)
    let file_batch_size = 5_000; // Files to parse per batch (bounded memory)
    let channel_depth = 256; // Pipeline buffer depth (larger = smoother utilization)

    // Channels
    let (parse_tx, parse_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) = bounded(channel_depth);
    let (embed_tx, embed_rx): (Sender<EmbeddedBatch>, Receiver<EmbeddedBatch>) =
        bounded(channel_depth);
    // GPU failure channel - GPU requeues failed batches here for CPU to handle async
    let (fail_tx, fail_rx): (Sender<ParsedBatch>, Receiver<ParsedBatch>) = bounded(channel_depth);

    // Shared state for progress
    let total_files = files.len();
    let parsed_count = Arc::new(AtomicUsize::new(0));
    let embedded_count = Arc::new(AtomicUsize::new(0));
    let gpu_failures = Arc::new(AtomicUsize::new(0));
    let parse_errors = Arc::new(AtomicUsize::new(0));

    // Create parser once and share via Arc (avoids re-creating ~1ms init per thread)
    let parser = Arc::new(CqParser::new().context("Failed to initialize parser")?);
    let parser_for_thread = Arc::clone(&parser);

    // Create store once and share via Arc (single runtime + connection pool)
    let store = Arc::new(Store::open(store_path).context("Failed to open store")?);
    let store_for_parser = Arc::clone(&store);
    let store_for_gpu = Arc::clone(&store);
    let store_for_cpu = Arc::clone(&store);

    // Clone for threads
    let root_clone = root.to_path_buf();
    let parsed_count_clone = Arc::clone(&parsed_count);
    let parse_errors_clone = Arc::clone(&parse_errors);

    // Stage 1: Parser thread - parse files in parallel batches
    let parser_handle = thread::spawn(move || -> Result<()> {
        let parser = parser_for_thread;
        let store = store_for_parser;
        let root = root_clone;

        for (batch_idx, file_batch) in files.chunks(file_batch_size).enumerate() {
            if check_interrupted() {
                break;
            }

            tracing::info!(
                batch = batch_idx + 1,
                files = file_batch.len(),
                "Processing file batch"
            );

            // Parse files in parallel
            let chunks: Vec<Chunk> = file_batch
                .par_iter()
                .flat_map(|rel_path| {
                    let abs_path = root.join(rel_path);
                    match parser.parse_file(&abs_path) {
                        Ok(mut chunks) => {
                            // Rewrite paths to be relative for storage
                            // Normalize path separators to forward slashes for cross-platform consistency
                            let path_str = rel_path.to_string_lossy().replace('\\', "/");
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
                            chunks
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse {}: {}", abs_path.display(), e);
                            parse_errors_clone.fetch_add(1, Ordering::Relaxed);
                            vec![]
                        }
                    }
                })
                .collect();

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
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        file_mtimes.insert(c.file.clone(), mtime);
                    }
                }
                chunks
            } else {
                chunks
                    .into_iter()
                    .filter(|c| {
                        let abs_path = root.join(&c.file);
                        // needs_reindex returns Some(mtime) if reindex needed, None otherwise
                        match store.needs_reindex(&abs_path) {
                            Ok(Some(mtime)) => {
                                file_mtimes.insert(c.file.clone(), mtime);
                                true
                            }
                            Ok(None) => false,
                            Err(e) => {
                                tracing::warn!(file = %abs_path.display(), error = %e, "mtime check failed, reindexing");
                                true
                            }
                        }
                    })
                    .collect()
            };

            parsed_count_clone.fetch_add(file_batch.len(), Ordering::Relaxed);

            if !chunks.is_empty() {
                // Send in embedding-sized batches with per-file mtimes
                for chunk_batch in chunks.chunks(batch_size) {
                    let batch_mtimes: std::collections::HashMap<PathBuf, i64> = chunk_batch
                        .iter()
                        .filter_map(|c| file_mtimes.get(&c.file).map(|&m| (c.file.clone(), m)))
                        .collect();
                    if parse_tx
                        .send(ParsedBatch {
                            chunks: chunk_batch.to_vec(),
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
    });

    // Clone for embedders (GPU and CPU run in parallel)
    let embedded_count_gpu = Arc::clone(&embedded_count);
    let embedded_count_cpu = Arc::clone(&embedded_count);
    let gpu_failures_clone = Arc::clone(&gpu_failures);
    let parse_rx_cpu = parse_rx.clone(); // CPU also grabs regular batches
    let embed_tx_cpu = embed_tx.clone();

    // Stage 2a: GPU Embedder thread - embed chunks, requeue failures to CPU
    let gpu_embedder_handle = thread::spawn(move || -> Result<()> {
        let embedder = Embedder::new().context("Failed to initialize GPU embedder")?;
        embedder.warm().context("Failed to warm GPU embedder")?;
        let store = store_for_gpu;

        for batch in parse_rx {
            if check_interrupted() {
                break;
            }

            // Use shared preparation logic (windowing + cache check + NL generation)
            let prepared = prepare_for_embedding(batch, &embedder, &store);

            if prepared.to_embed.is_empty() {
                // All cached, send directly
                let cached_count = prepared.cached.len();
                embedded_count_gpu.fetch_add(cached_count, Ordering::Relaxed);
                if embed_tx
                    .send(EmbeddedBatch {
                        chunk_embeddings: prepared.cached,
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
                if !flush_to_cpu(prepared, &embed_tx, &fail_tx, &embedded_count_gpu) {
                    break;
                }
                continue;
            }

            let text_refs: Vec<&str> = prepared.texts.iter().map(|s| s.as_str()).collect();
            match embedder.embed_documents(&text_refs) {
                Ok(embs) => {
                    let new_embeddings: Vec<Embedding> =
                        embs.into_iter().map(|e| e.with_sentiment(0.0)).collect();
                    let cached_count = prepared.cached.len();
                    let mut chunk_embeddings = prepared.cached;
                    chunk_embeddings.extend(prepared.to_embed.into_iter().zip(new_embeddings));
                    embedded_count_gpu.fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
                    if embed_tx
                        .send(EmbeddedBatch {
                            chunk_embeddings,
                            cached_count,
                            file_mtimes: prepared.file_mtimes.clone(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    // GPU failed - log details, then flush cached + requeue to CPU
                    gpu_failures_clone.fetch_add(prepared.to_embed.len(), Ordering::Relaxed);
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
                    if !flush_to_cpu(prepared, &embed_tx, &fail_tx, &embedded_count_gpu) {
                        break;
                    }
                }
            }
        }
        drop(fail_tx); // Signal CPU thread to finish when done
        tracing::debug!("GPU embedder thread finished");
        Ok(())
    });

    // Stage 2b: CPU Embedder thread - handles failures + overflow (GPU gets priority)
    // CPU embedder is lazy-initialized on first batch to save ~500MB when GPU handles everything.
    let cpu_embedder_handle = thread::spawn(move || -> Result<()> {
        let store = store_for_cpu;
        let mut embedder: Option<Embedder> = None;

        loop {
            if check_interrupted() {
                break;
            }

            // Race: GPU and CPU both grab from parse_rx, CPU also handles routed long batches
            let batch = select! {
                recv(fail_rx) -> msg => match msg {
                    Ok(b) => b,
                    Err(_) => match parse_rx_cpu.recv() {
                        Ok(b) => b,
                        Err(_) => break,
                    },
                },
                recv(parse_rx_cpu) -> msg => match msg {
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
                    let e = Embedder::new_cpu().context("Failed to initialize CPU embedder")?;
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
                    .into_iter()
                    .map(|e| e.with_sentiment(0.0))
                    .collect()
            };

            let embedded_batch = create_embedded_batch(
                prepared.cached,
                prepared.to_embed,
                new_embeddings,
                prepared.file_mtimes,
            );

            embedded_count_cpu.fetch_add(embedded_batch.chunk_embeddings.len(), Ordering::Relaxed);

            if embed_tx_cpu.send(embedded_batch).is_err() {
                break; // Receiver dropped
            }
        }
        tracing::debug!("CPU embedder thread finished");
        Ok(())
    });

    // Stage 3: Writer (main thread) - write to SQLite
    // Uses shared store created earlier (single runtime + connection pool)
    // Reuse shared parser for call graph extraction
    let mut total_embedded = 0;
    let mut total_cached = 0;

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

    for batch in embed_rx {
        if check_interrupted() {
            break;
        }

        // Extract call graph for this batch
        let all_calls: Vec<_> = batch
            .chunk_embeddings
            .iter()
            .flat_map(|(chunk, _)| {
                let calls = parser.extract_calls_from_chunk(chunk);
                if calls.is_empty() {
                    Vec::new()
                } else {
                    calls.into_iter().map(|c| (chunk.id.clone(), c)).collect()
                }
            })
            .collect();

        let batch_count = batch.chunk_embeddings.len();

        // Atomically upsert chunks + calls in a single transaction per file group
        if batch.file_mtimes.len() <= 1 {
            // Fast path: single file or no mtimes
            let mtime = batch.file_mtimes.values().next().copied();
            store.upsert_chunks_and_calls(&batch.chunk_embeddings, mtime, &all_calls)?;
        } else {
            // Multi-file batch: group by file and upsert with correct per-file mtime.
            // Consume chunk_embeddings to avoid cloning (Chunk + Embedding are large).
            let mut by_file: std::collections::HashMap<PathBuf, Vec<(Chunk, Embedding)>> =
                std::collections::HashMap::new();
            for (chunk, embedding) in batch.chunk_embeddings {
                by_file
                    .entry(chunk.file.clone())
                    .or_default()
                    .push((chunk, embedding));
            }

            // Build a set of chunk IDs per file for filtering calls
            for (file, pairs) in &by_file {
                let mtime = batch.file_mtimes.get(file.as_path()).copied();
                let chunk_ids: std::collections::HashSet<&str> =
                    pairs.iter().map(|(c, _)| c.id.as_str()).collect();
                let file_calls: Vec<_> = all_calls
                    .iter()
                    .filter(|(id, _)| chunk_ids.contains(id.as_str()))
                    .cloned()
                    .collect();
                store.upsert_chunks_and_calls(pairs, mtime, &file_calls)?;
            }
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

    progress.finish_with_message("done");

    // Wait for threads to finish
    parser_handle
        .join()
        .map_err(|e| anyhow::anyhow!("Parser thread panicked: {}", panic_message(&e)))??;
    gpu_embedder_handle
        .join()
        .map_err(|e| anyhow::anyhow!("GPU embedder thread panicked: {}", panic_message(&e)))??;
    cpu_embedder_handle
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
    };

    tracing::info!(
        total_embedded = stats.total_embedded,
        total_cached = stats.total_cached,
        gpu_failures = stats.gpu_failures,
        parse_errors = stats.parse_errors,
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
        let emb = Embedding::new(vec![0.0; 769]);
        let cached = vec![(chunk, emb)];

        let batch = create_embedded_batch(cached, vec![], vec![], test_mtimes(12345));
        assert_eq!(batch.chunk_embeddings.len(), 1);
        assert_eq!(batch.cached_count, 1);
        assert_eq!(batch.file_mtimes[&PathBuf::from("test.rs")], 12345);
    }

    #[test]
    fn test_create_embedded_batch_all_new() {
        let chunk = make_test_chunk("c1", "fn foo() {}");
        let emb = Embedding::new(vec![1.0; 769]);

        let batch = create_embedded_batch(vec![], vec![chunk], vec![emb], test_mtimes(99));
        assert_eq!(batch.chunk_embeddings.len(), 1);
        assert_eq!(batch.cached_count, 0);
        assert_eq!(batch.file_mtimes[&PathBuf::from("test.rs")], 99);
    }

    #[test]
    fn test_create_embedded_batch_mixed() {
        let cached_chunk = make_test_chunk("c1", "fn foo() {}");
        let cached_emb = Embedding::new(vec![0.0; 769]);
        let new_chunk = make_test_chunk("c2", "fn bar() {}");
        let new_emb = Embedding::new(vec![1.0; 769]);

        let batch = create_embedded_batch(
            vec![(cached_chunk, cached_emb)],
            vec![new_chunk],
            vec![new_emb],
            test_mtimes(12345),
        );
        assert_eq!(batch.chunk_embeddings.len(), 2);
        assert_eq!(batch.cached_count, 1);
    }

    #[test]
    fn test_create_embedded_batch_empty() {
        let batch = create_embedded_batch(vec![], vec![], vec![], std::collections::HashMap::new());
        assert_eq!(batch.chunk_embeddings.len(), 0);
        assert_eq!(batch.cached_count, 0);
    }

    #[test]
    fn test_create_embedded_batch_preserves_order() {
        let c1 = make_test_chunk("c1", "fn first() {}");
        let e1 = Embedding::new(vec![1.0; 769]);
        let c2 = make_test_chunk("c2", "fn second() {}");
        let e2 = Embedding::new(vec![2.0; 769]);
        let c3 = make_test_chunk("c3", "fn third() {}");
        let e3 = Embedding::new(vec![3.0; 769]);

        let batch =
            create_embedded_batch(vec![(c1, e1)], vec![c2, c3], vec![e2, e3], test_mtimes(0));

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
}
