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

/// Embed channel depth — heavy (embedding vectors), bounded for memory.
///
/// SHL-V1.36-8: pins a *byte budget* (~16 MB by default) instead of a fixed
/// depth so the channel holds the same amount of vector data regardless of
/// embedding dim. Pre-fix, `depth=64` × batch=64 × dim=1024 × 4 bytes = 16 MB
/// at BGE-large but ballooned/shrank linearly with dim. Now: depth scales
/// inversely with `(batch × dim × 4)` so the buffered byte total stays in
/// the same neighborhood.
///
/// `CQS_EMBED_CHANNEL_DEPTH` env override wins verbatim. With no override
/// and `dim == 0` (test paths) we fall back to the historic default of 64.
pub(super) fn embed_channel_depth(dim: usize, batch_size: usize) -> usize {
    static DEPTH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *DEPTH.get_or_init(|| match std::env::var("CQS_EMBED_CHANNEL_DEPTH") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(depth = n, "CQS_EMBED_CHANNEL_DEPTH override");
                n
            }
            _ => {
                tracing::warn!(value = %val, "Invalid CQS_EMBED_CHANNEL_DEPTH, using default budget");
                derive_depth_from_budget(dim, batch_size)
            }
        },
        Err(_) => derive_depth_from_budget(dim, batch_size),
    })
}

/// SHL-V1.36-8: derive channel depth from a 16 MB byte budget. Each
/// message ≈ `batch_size * dim * 4 bytes` of f32 vectors. Clamp `[16, 256]`.
fn derive_depth_from_budget(dim: usize, batch_size: usize) -> usize {
    const BYTE_BUDGET: usize = 16 * 1024 * 1024;
    if dim == 0 || batch_size == 0 {
        return 64; // historic default for test / unknown paths
    }
    let msg_bytes = batch_size.saturating_mul(dim).saturating_mul(4);
    if msg_bytes == 0 {
        return 64;
    }
    (BYTE_BUDGET / msg_bytes).clamp(16, 256)
}

/// Process-wide lock used by tests that mutate or depend on
/// `CQS_EMBED_BATCH_SIZE`. Shared across `pipeline::tests` and
/// `pipeline::parsing::tests` so env-var-sensitive tests do not race with
/// each other under `cargo test`'s default parallelism.
#[cfg(test)]
pub(super) static TEST_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Legacy fixed-batch helper kept ONLY for callers without a `ModelConfig`
/// in scope (currently: nothing in production, only the in-tree tests
/// `pipeline::tests::test_embed_batch_size` and the parser-stage drain
/// regression test). Production must use [`embed_batch_size_for`] which
/// scales batch with the active model's dim & seq — at batch=64 the
/// nomic-coderank preset (768 dim, 2048 seq) OOMs an 8 GB GPU.
///
/// Returns 64 with `CQS_EMBED_BATCH_SIZE` env override.
#[cfg(test)]
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

/// SHL-V1.30-1 / P2.41 — scale the embed batch size with the active model's
/// dim & seq.
///
/// CQ-V1.33.0-2: the implementation moved onto [`cqs::embedder::ModelConfig`]
/// so `Embedder::embed_documents` (which only has `&ModelConfig` in scope)
/// can use the same scaling rule. This thin wrapper is kept for cli-side
/// callers that already had the function path baked in.
pub(crate) fn embed_batch_size_for(model: &cqs::embedder::ModelConfig) -> usize {
    model.embed_batch_size()
}
