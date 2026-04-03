//! Embedding stages: GPU (2a) and CPU fallback (2b), plus shared preparation logic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use crossbeam_channel::{select, Receiver, Sender};

use cqs::embedder::ModelConfig;
use cqs::{Chunk, Embedder, Embedding, Store};

use super::types::{EmbeddedBatch, ParsedBatch, PreparedEmbedding, RelationshipData};
use super::windowing::apply_windowing;
use crate::cli::check_interrupted;

/// Prepare a batch for embedding: apply windowing, check cache, generate texts.
///
/// This consolidates the common logic between GPU and CPU embedder threads:
/// 1. Apply windowing to split long chunks
/// 2. Check store for cached embeddings by content hash
/// 3. Separate into cached (reuse) vs to_embed (need new embedding)
/// 4. Generate NL descriptions for chunks needing embedding
pub(super) fn prepare_for_embedding(
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
pub(super) fn create_embedded_batch(
    cached: Vec<(Chunk, Embedding)>,
    to_embed: Vec<Chunk>,
    new_embeddings: Vec<Embedding>,
    relationships: RelationshipData,
    file_mtimes: HashMap<std::path::PathBuf, i64>,
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

/// Stage 2a: GPU embedder — embed chunks, requeue failures to CPU fallback.
pub(super) fn gpu_embed_stage(
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
        let avg_len = if prepared.texts.is_empty() {
            0
        } else {
            prepared.texts.iter().map(|t| t.len()).sum::<usize>() / prepared.texts.len()
        };
        tracing::debug!(
            batch_size = prepared.texts.len(),
            max_char_len = max_len,
            avg_char_len = avg_len,
            total_chars = prepared.texts.iter().map(|t| t.len()).sum::<usize>(),
            "embed_batch start"
        );

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
        let embed_start = std::time::Instant::now();
        match embedder.embed_documents(&text_refs) {
            Ok(embs) => {
                tracing::debug!(
                    elapsed_ms = embed_start.elapsed().as_millis() as u64,
                    count = embs.len(),
                    "embed_batch ok"
                );
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
pub(super) fn cpu_embed_stage(
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
