//! Embedding stages: GPU (2a) and CPU fallback (2b), plus shared preparation logic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use crossbeam_channel::{select, Receiver, Sender};

use cqs::{Chunk, Embedder, Embedding, Store};

use super::types::{
    EmbedStageContext, EmbeddedBatch, ParsedBatch, PreparedEmbedding, RelationshipData,
};
use super::windowing::apply_windowing;
use crate::cli::check_interrupted;

/// Prepare a batch for embedding: apply windowing, check caches, generate texts.
///
/// This consolidates the common logic between GPU and CPU embedder threads:
/// 1. Apply windowing to split long chunks
/// 2. Check global embedding cache (by content_hash + model_fingerprint)
/// 3. Check store for cached embeddings by content hash
/// 4. Separate into cached (reuse) vs to_embed (need new embedding)
/// 5. Generate NL descriptions for chunks needing embedding
pub(super) fn prepare_for_embedding(
    batch: ParsedBatch,
    embedder: &Embedder,
    store: &Store,
    global_cache: Option<&cqs::cache::EmbeddingCache>,
    model_fingerprint: Option<&str>,
) -> PreparedEmbedding {
    let _span = tracing::info_span!("prepare_for_embedding").entered();
    use cqs::generate_nl_description_with_seq_len;

    // Step 1: Apply windowing to split long chunks into overlapping windows
    let windowed_chunks = apply_windowing(batch.chunks, embedder);

    // Use the model-aware NL variant so the section-chunk content budget
    // scales with `model.max_seq_length` — a fixed 512 cap would limit
    // nomic-coderank (2048 seq) to 25% capacity.
    let model_max_seq_len = embedder.model_config().max_seq_length;

    // Step 2: Resolve embedding reuse (global cache → store cache → embed) via
    // the shared resolver, which owns the canonical-key logic, the
    // NULL/empty-canonical fallback, the dim-mismatch store-cache skip, and the
    // duplicate-key fallthrough contract. The watch incremental path
    // (`watch::reindex::reindex_files`) calls the same function — #1692 unified
    // the reuse DECISION so the canonical_hash key (and any future
    // reuse-semantics change) lives in exactly one place.
    let dim = embedder.embedding_dim();
    // A store-cache read failure is non-fatal on the bulk path: warn and
    // degrade to re-embedding the batch (the watch path, by contrast,
    // propagates the error so the daemon retries next tick — see
    // `resolve_reuse`'s error contract).
    let split = match crate::cli::pipeline::resolve_reuse(
        &windowed_chunks,
        store,
        global_cache,
        dim,
        model_fingerprint,
    ) {
        Ok(split) => split,
        Err(e) => {
            tracing::warn!(error = %e, "Embedding-reuse resolution failed; re-embedding batch");
            super::reuse::ReuseSplit {
                cached: Vec::new(),
                to_embed: (0..windowed_chunks.len()).collect(),
                global_hits: 0,
            }
        }
    };
    let global_hits_total = split.global_hits;

    // Step 3: Map the index split into this caller's owned output shape.
    // `resolve_reuse` returns indices into `windowed_chunks` so neither caller's
    // ownership model is forced on the other; here we take ownership of each
    // chunk by consuming the Vec via index lookup.
    //
    // `cached` indices are ascending (the resolver walks chunks in order), so a
    // single forward pass with a peekable cached-index cursor partitions the
    // chunks without per-chunk membership tests. Both `cached` and `to_embed`
    // preserve original chunk order.
    let mut cached: Vec<(Chunk, Embedding)> = Vec::with_capacity(split.cached.len());
    let mut to_embed: Vec<Chunk> = Vec::with_capacity(split.to_embed.len());
    let mut cached_iter = split.cached.into_iter().peekable();
    for (i, chunk) in windowed_chunks.into_iter().enumerate() {
        match cached_iter.peek() {
            Some((ci, _)) if *ci == i => {
                let (_, emb) = cached_iter.next().expect("peeked Some");
                cached.push((chunk, emb));
            }
            _ => to_embed.push(chunk),
        }
    }

    tracing::info!(
        total = cached.len() + to_embed.len(),
        global_hits = global_hits_total,
        store_hits = cached.len().saturating_sub(global_hits_total),
        to_embed = to_embed.len(),
        "Embedding cache stats"
    );

    // Step 4: Generate NL descriptions for chunks needing embedding
    let texts: Vec<String> = to_embed
        .iter()
        .map(|c| generate_nl_description_with_seq_len(c, model_max_seq_len))
        .collect();

    PreparedEmbedding {
        cached,
        to_embed,
        texts,
        relationships: batch.relationships,
        file_fingerprints: batch.file_fingerprints,
    }
}

/// Create an EmbeddedBatch from cached and newly embedded chunks.
pub(super) fn create_embedded_batch(
    cached: Vec<(Chunk, Embedding)>,
    to_embed: Vec<Chunk>,
    new_embeddings: Vec<Embedding>,
    relationships: RelationshipData,
    file_fingerprints: HashMap<std::path::PathBuf, cqs::store::FileFingerprint>,
) -> EmbeddedBatch {
    let cached_count = cached.len();
    let mut chunk_embeddings = cached;
    chunk_embeddings.extend(to_embed.into_iter().zip(new_embeddings));
    EmbeddedBatch {
        chunk_embeddings,
        relationships,
        cached_count,
        file_fingerprints,
        // Default: real embeddings throughout. The skip-first-pass path
        // builds EmbeddedBatch directly with `uncached_need_embedding: true`
        // (see `gpu_embed_stage` / `cpu_embed_stage`).
        uncached_need_embedding: false,
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
                file_fingerprints: prepared.file_fingerprints.clone(),
                uncached_need_embedding: false,
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
            file_fingerprints: prepared.file_fingerprints,
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
    ctx: EmbedStageContext,
    gpu_failures: Arc<AtomicUsize>,
) -> Result<()> {
    let _span = tracing::info_span!("embed_thread", mode = "gpu").entered();
    let embedder = Embedder::new(ctx.model_config).context("Failed to initialize GPU embedder")?;
    embedder.warm().context("Failed to warm GPU embedder")?;
    // The fingerprint's only consumer is the global cache, and its first
    // computation streams blake3 over the full ONNX model file — skip it
    // entirely when no cache is present.
    let fingerprint: Option<String> = ctx
        .global_cache
        .is_some()
        .then(|| embedder.model_fingerprint());

    for batch in parse_rx {
        if check_interrupted() {
            break;
        }

        // Use shared preparation logic (windowing + cache check + NL generation)
        let prepared = prepare_for_embedding(
            batch,
            &embedder,
            &ctx.store,
            ctx.global_cache.as_deref(),
            fingerprint.as_deref(),
        );

        if prepared.to_embed.is_empty() {
            // All cached, send directly
            let cached_count = prepared.cached.len();
            ctx.embedded_count
                .fetch_add(cached_count, Ordering::Relaxed);
            if embed_tx
                .send(EmbeddedBatch {
                    chunk_embeddings: prepared.cached,
                    relationships: prepared.relationships,
                    cached_count,
                    file_fingerprints: prepared.file_fingerprints,
                    uncached_need_embedding: false,
                })
                .is_err()
            {
                break;
            }
            continue;
        }

        // Skip-first-pass-embed short-circuit. When set, we do NOT call
        // `embed_documents()` for cache-miss chunks — instead we emit zero-vec
        // sentinels stamped `needs_embedding=1` so the post-summary
        // `enrichment_pass` can land their real vectors without the wasted GPU
        // time of a discarded first pass. Cache hits still pass through with
        // their real embeddings.
        if ctx.skip_first_pass_embed {
            let dim = embedder.embedding_dim();
            let zero_vec_count = prepared.to_embed.len();
            let zero_vecs: Vec<Embedding> = (0..zero_vec_count)
                .map(|_| Embedding::new(vec![0.0_f32; dim]))
                .collect();
            let cached_count = prepared.cached.len();
            let mut chunk_embeddings = prepared.cached;
            chunk_embeddings.extend(prepared.to_embed.into_iter().zip(zero_vecs));
            ctx.embedded_count
                .fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
            tracing::debug!(
                cache_hits = cached_count,
                stamped_unembedded = zero_vec_count,
                "skip-first-pass: emitted zero-vec batch"
            );
            if embed_tx
                .send(EmbeddedBatch {
                    chunk_embeddings,
                    relationships: prepared.relationships,
                    cached_count,
                    file_fingerprints: prepared.file_fingerprints,
                    uncached_need_embedding: true,
                })
                .is_err()
            {
                break;
            }
            continue;
        }

        let (max_len, total_chars) = prepared
            .texts
            .iter()
            .fold((0, 0), |(mx, sm), t| (mx.max(t.len()), sm + t.len()));
        let avg_len = if prepared.texts.is_empty() {
            0
        } else {
            total_chars / prepared.texts.len()
        };
        tracing::debug!(
            batch_size = prepared.texts.len(),
            max_char_len = max_len,
            avg_char_len = avg_len,
            total_chars,
            "embed_batch start"
        );

        // No pre-flight "filter long batches to CPU" check: both
        // `apply_windowing` (`src/cli/pipeline/windowing.rs`) and
        // `generate_nl_description_with_seq_len` already bound chunk text to
        // `model_max_seq_length` tokens, so such a filter (calibrated for
        // BERT-class 512-token models) would false-positive nearly every
        // windowed chunk on Gemma 2K and Qwen3-8B 8K presets and defeat
        // `CQS_DISABLE_CPU_WARM`. Genuine GPU failures (CUDNN seq-len limits,
        // OOM, etc.) still route to CPU via the `fail_tx` path inside
        // `embed_documents` below.

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

                // Write new embeddings to global cache (best-effort).
                //
                // Build with borrows so we don't clone every `content_hash`
                // and embedding vec into an owned tuple per batch. The borrowed
                // slices live until `write_batch` returns, well within the
                // chunk/embedding lifetimes here.
                if let (Some(cache), Some(fp)) =
                    (ctx.global_cache.as_deref(), fingerprint.as_deref())
                {
                    // Write under the canonical key (v28) so a later
                    // comment-only edit reuses this embedding — the shared
                    // `canon_key_ref` owns the empty-canonical fallback.
                    let entries: Vec<(&str, &[f32])> = prepared
                        .to_embed
                        .iter()
                        .zip(new_embeddings.iter())
                        .map(|(chunk, emb)| {
                            (crate::cli::pipeline::canon_key_ref(chunk), emb.as_slice())
                        })
                        .collect();
                    if let Err(e) = cache.write_batch(
                        &entries,
                        fp,
                        cqs::cache::CachePurpose::Embedding,
                        embedder.embedding_dim(),
                    ) {
                        tracing::warn!(error = %e, "Global cache write failed (best-effort)");
                    }
                }

                let cached_count = prepared.cached.len();
                let mut chunk_embeddings = prepared.cached;
                chunk_embeddings.extend(prepared.to_embed.into_iter().zip(new_embeddings));
                ctx.embedded_count
                    .fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
                if embed_tx
                    .send(EmbeddedBatch {
                        chunk_embeddings,
                        relationships: prepared.relationships,
                        cached_count,
                        file_fingerprints: prepared.file_fingerprints,
                        uncached_need_embedding: false,
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
                if !flush_to_cpu(prepared, &embed_tx, &fail_tx, &ctx.embedded_count) {
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
    ctx: EmbedStageContext,
) -> Result<()> {
    let _span = tracing::info_span!("embed_thread", mode = "cpu").entered();
    let mut embedder: Option<Embedder> = None;
    let mut fingerprint: Option<String> = None;

    // CQS_DISABLE_CPU_WARM=1: don't compete with GPU for parse_rx batches.
    // CPU still drains fail_rx for fault tolerance (GPU-failed chunks),
    // but if GPU handles every batch successfully the CPU embedder never
    // lazy-inits, saving the ONNX-mmap RSS. The motivating case is a large
    // embedder (e.g. Qwen3-Embedding-8B) whose session mmaps a 30 GB FP32
    // weights file: racing on parse_rx would keep both CPU and GPU sessions
    // alive at once, climbing to ~91 GB RSS inside WSL2 and forcing an early
    // kill. Default (env unset) takes the race / overflow path.
    let disable_cpu_warm = std::env::var("CQS_DISABLE_CPU_WARM")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if disable_cpu_warm {
        tracing::info!(
            "CQS_DISABLE_CPU_WARM=1: CPU embedder will only handle GPU-failed batches \
             (parse_rx race disabled)"
        );
    }

    loop {
        if check_interrupted() {
            break;
        }

        // Race: GPU and CPU both grab from parse_rx, CPU also handles routed long batches.
        // With CQS_DISABLE_CPU_WARM=1, only watch fail_rx — GPU has parse_rx to itself.
        let batch = if disable_cpu_warm {
            match fail_rx.recv() {
                Ok(b) => b,
                Err(_) => break,
            }
        } else {
            select! {
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
            }
        };

        // Lazy-init CPU embedder on first batch
        let emb = match &embedder {
            Some(e) => e,
            None => {
                let e = Embedder::new_cpu(ctx.model_config.clone())
                    .context("Failed to initialize CPU embedder")?;
                embedder.insert(e)
            }
        };

        // Compute fingerprint lazily (after embedder init), and only when a
        // global cache exists — the fingerprint's only consumer is the cache,
        // and its first computation streams blake3 over the full ONNX model.
        if fingerprint.is_none() && ctx.global_cache.is_some() {
            fingerprint = Some(emb.model_fingerprint());
        }

        // Prepare batch: windowing, cache check, text generation
        // (still useful in skip-first-pass mode — windowing splits long chunks
        // and cache lookup salvages real embeddings for hit chunks).
        let prepared = prepare_for_embedding(
            batch,
            emb,
            &ctx.store,
            ctx.global_cache.as_deref(),
            fingerprint.as_deref(),
        );

        // Skip-first-pass-embed short-circuit (CPU side). Mirrors the GPU
        // stage above — emit zero-vec sentinels for to_embed chunks stamped
        // `needs_embedding=1`. Cache hits still pass through with their real
        // embeddings.
        if ctx.skip_first_pass_embed && !prepared.to_embed.is_empty() {
            let dim = emb.embedding_dim();
            let zero_vec_count = prepared.to_embed.len();
            let zero_vecs: Vec<Embedding> = (0..zero_vec_count)
                .map(|_| Embedding::new(vec![0.0_f32; dim]))
                .collect();
            let cached_count = prepared.cached.len();
            let mut chunk_embeddings = prepared.cached;
            chunk_embeddings.extend(prepared.to_embed.into_iter().zip(zero_vecs));
            ctx.embedded_count
                .fetch_add(chunk_embeddings.len(), Ordering::Relaxed);
            tracing::debug!(
                cache_hits = cached_count,
                stamped_unembedded = zero_vec_count,
                "skip-first-pass: emitted zero-vec batch (cpu)"
            );
            if embed_tx
                .send(EmbeddedBatch {
                    chunk_embeddings,
                    relationships: prepared.relationships,
                    cached_count,
                    file_fingerprints: prepared.file_fingerprints,
                    uncached_need_embedding: true,
                })
                .is_err()
            {
                break;
            }
            continue;
        }

        // Embed new chunks (CPU only)
        let new_embeddings: Vec<Embedding> = if prepared.to_embed.is_empty() {
            vec![]
        } else {
            let text_refs: Vec<&str> = prepared.texts.iter().map(|s| s.as_str()).collect();
            let embs = emb.embed_documents(&text_refs).map_err(|e| {
                tracing::warn!(
                    error = %e,
                    chunks = prepared.to_embed.len(),
                    "CPU embedding failed"
                );
                e
            })?;

            // Write new embeddings to global cache (best-effort).
            //
            // Build with borrows so we don't clone every `content_hash` and
            // embedding vec into an owned tuple per batch.
            if let (Some(cache), Some(fp)) = (ctx.global_cache.as_deref(), fingerprint.as_deref()) {
                // Write under the canonical key (v28) — see GPU-stage note.
                let entries: Vec<(&str, &[f32])> = prepared
                    .to_embed
                    .iter()
                    .zip(embs.iter())
                    .map(|(chunk, e)| (crate::cli::pipeline::canon_key_ref(chunk), e.as_slice()))
                    .collect();
                if let Err(e) = cache.write_batch(
                    &entries,
                    fp,
                    cqs::cache::CachePurpose::Embedding,
                    emb.embedding_dim(),
                ) {
                    tracing::warn!(error = %e, "Global cache write failed (best-effort)");
                }
            }

            embs
        };

        let embedded_batch = create_embedded_batch(
            prepared.cached,
            prepared.to_embed,
            new_embeddings,
            prepared.relationships,
            prepared.file_fingerprints,
        );

        ctx.embedded_count
            .fetch_add(embedded_batch.chunk_embeddings.len(), Ordering::Relaxed);

        if embed_tx.send(embedded_batch).is_err() {
            break; // Receiver dropped
        }
    }
    tracing::debug!("CPU embedder thread finished");
    Ok(())
}
