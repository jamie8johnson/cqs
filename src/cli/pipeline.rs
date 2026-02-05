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
    file_mtime: i64,
}

struct EmbeddedBatch {
    chunk_embeddings: Vec<(Chunk, Embedding)>,
    cached_count: usize,
    file_mtime: i64,
}

/// Stats returned from pipelined indexing
pub(crate) struct PipelineStats {
    pub total_embedded: usize,
    pub total_cached: usize,
    pub gpu_failures: usize,
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
    /// File modification time for the batch
    file_mtime: i64,
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
    use cqs::nl::generate_nl_description;

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
        file_mtime: batch.file_mtime,
    }
}

/// Create an EmbeddedBatch from cached and newly embedded chunks.
fn create_embedded_batch(
    cached: Vec<(Chunk, Embedding)>,
    to_embed: Vec<Chunk>,
    new_embeddings: Vec<Embedding>,
    file_mtime: i64,
) -> EmbeddedBatch {
    let cached_count = cached.len();
    let mut chunk_embeddings = cached;
    chunk_embeddings.extend(to_embed.into_iter().zip(new_embeddings));
    EmbeddedBatch {
        chunk_embeddings,
        cached_count,
        file_mtime,
    }
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
    use cqs::nl::generate_nl_description;

    let batch_size = 32; // Embedding batch size (backed off from 64 - crashed at 2%)
    let file_batch_size = 100_000; // Files to parse per batch (all at once)
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

    // Stage 1: Parser thread - parse files in parallel batches
    let parser_handle = thread::spawn(move || -> Result<()> {
        let parser = parser_for_thread;
        let store = store_for_parser;
        let root = root_clone;

        for file_batch in files.chunks(file_batch_size) {
            if check_interrupted() {
                break;
            }

            // Parse files in parallel
            let chunks: Vec<Chunk> = file_batch
                .par_iter()
                .flat_map(|rel_path| {
                    let abs_path = root.join(rel_path);
                    match parser.parse_file(&abs_path) {
                        Ok(mut chunks) => {
                            // Rewrite paths to be relative for storage
                            for chunk in &mut chunks {
                                chunk.file = rel_path.clone();
                                let hash_prefix =
                                    chunk.content_hash.get(..8).unwrap_or(&chunk.content_hash);
                                // Normalize path separators to forward slashes for cross-platform consistency
                                let path_str = rel_path.to_string_lossy().replace('\\', "/");
                                chunk.id =
                                    format!("{}:{}:{}", path_str, chunk.line_start, hash_prefix);
                            }
                            chunks
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse {}: {}", abs_path.display(), e);
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
                            Err(_) => true, // Reindex on error
                        }
                    })
                    .collect()
            };

            parsed_count_clone.fetch_add(file_batch.len(), Ordering::Relaxed);

            if !chunks.is_empty() {
                // Use cached mtime from first chunk's file (already computed above)
                let file_mtime = chunks
                    .first()
                    .and_then(|c| file_mtimes.get(&c.file))
                    .copied()
                    .unwrap_or(0);

                // Send in embedding-sized batches
                for chunk_batch in chunks.chunks(batch_size) {
                    if parse_tx
                        .send(ParsedBatch {
                            chunks: chunk_batch.to_vec(),
                            file_mtime,
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

            // Apply windowing to split long chunks into overlapping windows
            let windowed_chunks = apply_windowing(batch.chunks, &embedder);
            let batch = ParsedBatch {
                chunks: windowed_chunks,
                file_mtime: batch.file_mtime,
            };

            // Check for existing embeddings by content hash
            let hashes: Vec<&str> = batch
                .chunks
                .iter()
                .map(|c| c.content_hash.as_str())
                .collect();
            let existing = store.get_embeddings_by_hashes(&hashes);

            // Separate into cached vs to_embed
            let mut to_embed: Vec<&Chunk> = Vec::new();
            let mut cached: Vec<(Chunk, Embedding)> = Vec::new();

            for chunk in &batch.chunks {
                if let Some(emb) = existing.get(&chunk.content_hash) {
                    cached.push((chunk.clone(), emb.clone()));
                } else {
                    to_embed.push(chunk);
                }
            }

            // Embed new chunks on GPU
            if to_embed.is_empty() {
                // All cached, send directly
                let cached_count = cached.len();
                embedded_count_gpu.fetch_add(cached_count, Ordering::Relaxed);
                if embed_tx
                    .send(EmbeddedBatch {
                        chunk_embeddings: cached,
                        cached_count,
                        file_mtime: batch.file_mtime,
                    })
                    .is_err()
                {
                    break;
                }
            } else {
                let texts: Vec<String> = to_embed
                    .iter()
                    .map(|c| generate_nl_description(c))
                    .collect();
                let max_len = texts.iter().map(|t| t.len()).max().unwrap_or(0);

                // Pre-filter long batches to CPU (GPU hits CUDNN limits >8k chars)
                if max_len > 8000 {
                    tracing::warn!(
                        chunks = to_embed.len(),
                        max_len,
                        "Routing long batch to CPU (GPU CUDNN limit)"
                    );
                    if fail_tx.send(batch).is_err() {
                        break;
                    }
                    continue;
                }

                let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                match embedder.embed_documents(&text_refs) {
                    Ok(embs) => {
                        let new_embeddings: Vec<Embedding> =
                            embs.into_iter().map(|e| e.with_sentiment(0.0)).collect();
                        let cached_count = cached.len();
                        let mut chunk_embeddings = cached;
                        chunk_embeddings.extend(to_embed.into_iter().cloned().zip(new_embeddings));
                        embedded_count_gpu.fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
                        if embed_tx
                            .send(EmbeddedBatch {
                                chunk_embeddings,
                                cached_count,
                                file_mtime: batch.file_mtime,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        // GPU failed - log details and requeue to CPU
                        gpu_failures_clone.fetch_add(batch.chunks.len(), Ordering::Relaxed);
                        let max_len = texts.iter().map(|t| t.len()).max().unwrap_or(0);
                        let files: Vec<_> = to_embed
                            .iter()
                            .map(|c| c.file.display().to_string())
                            .collect();
                        tracing::warn!(
                            error = %e,
                            chunks = batch.chunks.len(),
                            max_len,
                            ?files,
                            "GPU embedding failed, requeueing to CPU"
                        );
                        if fail_tx.send(batch).is_err() {
                            break; // CPU thread gone
                        }
                    }
                }
            }
        }
        drop(fail_tx); // Signal CPU thread to finish when done
        Ok(())
    });

    // Stage 2b: CPU Embedder thread - handles failures + overflow (GPU gets priority)
    let cpu_embedder_handle = thread::spawn(move || -> Result<()> {
        let embedder = Embedder::new_cpu().context("Failed to initialize CPU embedder")?;
        let store = store_for_cpu;

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

            // Prepare batch: windowing, cache check, text generation
            let prepared = prepare_for_embedding(batch, &embedder, &store);

            // Embed new chunks (CPU only)
            let new_embeddings: Vec<Embedding> = if prepared.to_embed.is_empty() {
                vec![]
            } else {
                let text_refs: Vec<&str> = prepared.texts.iter().map(|s| s.as_str()).collect();
                embedder
                    .embed_documents(&text_refs)?
                    .into_iter()
                    .map(|e| e.with_sentiment(0.0))
                    .collect()
            };

            let embedded_batch = create_embedded_batch(
                prepared.cached,
                prepared.to_embed,
                new_embeddings,
                prepared.file_mtime,
            );

            embedded_count_cpu.fetch_add(embedded_batch.chunk_embeddings.len(), Ordering::Relaxed);

            if embed_tx_cpu.send(embedded_batch).is_err() {
                break; // Receiver dropped
            }
        }
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

        store.upsert_chunks_batch(&batch.chunk_embeddings, Some(batch.file_mtime))?;

        // Extract and store function calls
        for (chunk, _) in &batch.chunk_embeddings {
            let calls = parser.extract_calls_from_chunk(chunk);
            if !calls.is_empty() {
                store.upsert_calls(&chunk.id, &calls)?;
            }
        }

        total_embedded += batch.chunk_embeddings.len();
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
        .map_err(|_| anyhow::anyhow!("Parser thread panicked"))??;
    gpu_embedder_handle
        .join()
        .map_err(|_| anyhow::anyhow!("GPU embedder thread panicked"))??;
    cpu_embedder_handle
        .join()
        .map_err(|_| anyhow::anyhow!("CPU embedder thread panicked"))??;

    Ok(PipelineStats {
        total_embedded,
        total_cached,
        gpu_failures: gpu_failures.load(Ordering::Relaxed),
    })
}
