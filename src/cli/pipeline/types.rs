//! Pipeline message types, shared state, and tuning constants.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use cqs::embedder::ModelConfig;
use cqs::parser::{CallSite, ChunkTypeRefs, FunctionCalls};
use cqs::{Chunk, Embedding, Store};

/// Relationship data extracted during parsing, keyed by file path.
/// Threaded through the pipeline so store_stage can persist without re-reading files.
#[derive(Clone, Default)]
pub(super) struct RelationshipData {
    pub type_refs: HashMap<PathBuf, Vec<ChunkTypeRefs>>,
    pub function_calls: HashMap<PathBuf, Vec<FunctionCalls>>,
    /// Per-chunk call sites for the `calls` table (PERF-28: extracted during parse stage
    /// to avoid re-parsing in store_stage). Keyed by chunk ID.
    pub chunk_calls: Vec<(String, CallSite)>,
}

/// Message types for the pipelined indexer
pub(super) struct ParsedBatch {
    pub chunks: Vec<Chunk>,
    pub relationships: RelationshipData,
    pub file_mtimes: HashMap<PathBuf, i64>,
}

pub(super) struct EmbeddedBatch {
    pub chunk_embeddings: Vec<(Chunk, Embedding)>,
    pub relationships: RelationshipData,
    pub cached_count: usize,
    pub file_mtimes: HashMap<PathBuf, i64>,
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
pub(super) struct PreparedEmbedding {
    /// Chunks with existing embeddings (from cache)
    pub cached: Vec<(Chunk, Embedding)>,
    /// Chunks that need new embeddings
    pub to_embed: Vec<Chunk>,
    /// NL descriptions for chunks needing embedding
    pub texts: Vec<String>,
    /// Relationships extracted during parsing
    pub relationships: RelationshipData,
    /// File modification times (per-file)
    pub file_mtimes: HashMap<PathBuf, i64>,
}

/// Shared configuration for GPU and CPU embedding stages.
///
/// Groups the parameters common to both `gpu_embed_stage` and `cpu_embed_stage`,
/// avoiding long argument lists on each function.
pub(super) struct EmbedStageContext {
    pub store: Arc<Store>,
    pub embedded_count: Arc<AtomicUsize>,
    pub model_config: ModelConfig,
    pub global_cache: Option<Arc<cqs::cache::EmbeddingCache>>,
}

// Pipeline tuning constants

/// Files to parse per batch (bounded memory).
/// Configurable via `CQS_FILE_BATCH_SIZE` environment variable.
pub(super) fn file_batch_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| match std::env::var("CQS_FILE_BATCH_SIZE") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(batch_size = n, "CQS_FILE_BATCH_SIZE override");
                n
            }
            _ => {
                tracing::warn!(value = %val, "Invalid CQS_FILE_BATCH_SIZE, using default 5000");
                5_000
            }
        },
        Err(_) => 5_000,
    })
}
/// Parse channel depth — lightweight (chunk metadata only), can be deeper.
/// Configurable via `CQS_PARSE_CHANNEL_DEPTH` environment variable.
pub(super) fn parse_channel_depth() -> usize {
    static DEPTH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *DEPTH.get_or_init(|| match std::env::var("CQS_PARSE_CHANNEL_DEPTH") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(depth = n, "CQS_PARSE_CHANNEL_DEPTH override");
                n
            }
            _ => {
                tracing::warn!(value = %val, "Invalid CQS_PARSE_CHANNEL_DEPTH, using default 512");
                512
            }
        },
        Err(_) => 512,
    })
}

/// Embed channel depth — heavy (embedding vectors), smaller to bound memory.
/// Configurable via `CQS_EMBED_CHANNEL_DEPTH` environment variable.
pub(super) fn embed_channel_depth() -> usize {
    static DEPTH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *DEPTH.get_or_init(|| match std::env::var("CQS_EMBED_CHANNEL_DEPTH") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(depth = n, "CQS_EMBED_CHANNEL_DEPTH override");
                n
            }
            _ => {
                tracing::warn!(value = %val, "Invalid CQS_EMBED_CHANNEL_DEPTH, using default 64");
                64
            }
        },
        Err(_) => 64,
    })
}

/// Process-wide lock used by tests that mutate or depend on
/// `CQS_EMBED_BATCH_SIZE`. Shared across `pipeline::tests` and
/// `pipeline::parsing::tests` so env-var-sensitive tests do not race with
/// each other under `cargo test`'s default parallelism.
#[cfg(test)]
pub(super) static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Embedding batch size. Was 32 (backed off from 64 after an undiagnosed crash at 2%).
/// Restored to 64 with debug logging (PERF-45 investigation). If it crashes again,
/// run with RUST_LOG=debug to capture batch_size/max_char_len/total_chars at failure.
/// Configurable via `CQS_EMBED_BATCH_SIZE` environment variable.
///
/// Legacy entry point — kept for callers that don't have a `ModelConfig`
/// in scope. New sites should prefer [`embed_batch_size_for`] which scales
/// with the active model's `dim` and `max_seq_length`.
pub(crate) fn embed_batch_size() -> usize {
    match std::env::var("CQS_EMBED_BATCH_SIZE") {
        Ok(val) => match val.parse::<usize>() {
            Ok(size) if size > 0 => {
                tracing::info!(batch_size = size, "CQS_EMBED_BATCH_SIZE override");
                size
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_EMBED_BATCH_SIZE, using default 64"
                );
                64
            }
        },
        Err(_) => 64,
    }
}

/// P2.41 — scale the embed batch size with the active model's dim & seq.
///
/// BGE-large (1024 dim, 512 seq) at batch=64 ≈ 130 MB per forward-pass tensor
/// — the empirical sweet spot on RTX 4060 8 GB. Nomic-coderank (768 dim,
/// 2048 seq) at batch=64 OOMs the same GPU because the tensor blows up to
/// ~390 MB.
///
/// Holding the per-tensor footprint roughly constant across models:
///   batch * seq * dim * 4 bytes ≈ 130 MB
/// → `batch_baseline * (1024/dim) * (512/seq)` rounded to a power of 2,
/// clamped to `[2, 256]`. The env override `CQS_EMBED_BATCH_SIZE` takes
/// priority — operators with workloads they understand can pin a value.
#[allow(dead_code)] // P2.41: opt-in helper; pipeline migration is a follow-on PR.
pub(crate) fn embed_batch_size_for(model: &cqs::embedder::ModelConfig) -> usize {
    if let Ok(val) = std::env::var("CQS_EMBED_BATCH_SIZE") {
        if let Ok(size) = val.parse::<usize>() {
            if size > 0 {
                tracing::info!(batch_size = size, "CQS_EMBED_BATCH_SIZE override");
                return size;
            }
        }
        tracing::warn!(
            value = %val,
            "Invalid CQS_EMBED_BATCH_SIZE, falling back to model-derived default"
        );
    }
    let dim = model.dim.max(1) as f64;
    let seq = model.max_seq_length.max(1) as f64;
    let baseline = 64.0_f64;
    let dim_factor = 1024.0 / dim;
    let seq_factor = (512.0 / seq).max(0.25);
    let scaled = (baseline * dim_factor * seq_factor).max(1.0) as usize;
    let rounded = scaled.next_power_of_two().clamp(2, 256);
    tracing::debug!(
        dim = model.dim,
        seq = model.max_seq_length,
        scaled,
        rounded,
        "embed_batch_size_for: model-derived default"
    );
    rounded
}
