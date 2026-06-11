//! Pipeline message types, shared state, and tuning constants.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use cqs::embedder::ModelConfig;
use cqs::parser::{CallSite, ChunkTypeRefs, FunctionCalls};
use cqs::store::FileFingerprint;
use cqs::{Chunk, Embedding, Store};

/// Relationship data extracted during parsing, keyed by file path.
/// Threaded through the pipeline so store_stage can persist without re-reading files.
#[derive(Clone, Default)]
pub(super) struct RelationshipData {
    pub type_refs: HashMap<PathBuf, Vec<ChunkTypeRefs>>,
    pub function_calls: HashMap<PathBuf, Vec<FunctionCalls>>,
    /// Per-chunk call sites for the `calls` table, extracted during the parse
    /// stage to avoid re-parsing in store_stage. Keyed by chunk ID.
    pub chunk_calls: Vec<(String, CallSite)>,
}

/// Message types for the pipelined indexer
pub(super) struct ParsedBatch {
    pub chunks: Vec<Chunk>,
    pub relationships: RelationshipData,
    /// Disk fingerprint (mtime + size + BLAKE3) for the files whose **last**
    /// chunk rides in this batch. A file's chunks can straddle batches (the
    /// parser drain loop slices at `embed_batch_size`, and a GPU failure
    /// re-splits a batch into a cached half and a requeued half). Stamping a
    /// file's fingerprint only when its final chunk lands keeps the stamp
    /// strictly *after* every one of the file's chunks has been written, so a
    /// crash between two of a file's batch commits leaves the file unstamped
    /// and the staleness pre-filter reclassifies it STALE on the next run
    /// rather than skipping a half-indexed file permanently.
    pub file_fingerprints: HashMap<PathBuf, FileFingerprint>,
    /// Files that survived the staleness pre-filter (so they *were* indexed
    /// before and diverged) but parsed to **zero** chunks this run — e.g. a
    /// source file whose code was deleted leaving only comments. They carry no
    /// chunks, so they never appear in `chunks`/`file_fingerprints`; the store
    /// stage includes them in the phantom-prune pass with an empty live set so
    /// their stale chunks are removed instead of surviving forever.
    pub empty_file_fingerprints: HashMap<PathBuf, FileFingerprint>,
}

pub(super) struct EmbeddedBatch {
    pub chunk_embeddings: Vec<(Chunk, Embedding)>,
    pub relationships: RelationshipData,
    pub cached_count: usize,
    /// Per-file disk fingerprints, threaded through from `ParsedBatch`. Only
    /// the files whose **last** chunk is in this batch are present — see
    /// `ParsedBatch::file_fingerprints`.
    pub file_fingerprints: HashMap<PathBuf, FileFingerprint>,
    /// Zero-chunk files threaded through from `ParsedBatch` — the store stage
    /// prunes their stale chunks. See `ParsedBatch::empty_file_fingerprints`.
    pub empty_file_fingerprints: HashMap<PathBuf, FileFingerprint>,
    /// When `true`, the chunks past index `cached_count` carry
    /// **zero-vec sentinel embeddings** and must be routed to
    /// `upsert_embedded_batch`'s sentinel argument so they're stamped with
    /// `needs_embedding=1`. Cached chunks (indexes `0..cached_count`)
    /// always carry real embeddings (from the global cache) and go in the
    /// real-embedding argument of the same call.
    ///
    /// Set by the embed stages when `EmbedStageContext.skip_first_pass_embed`
    /// is `true` AND there were `to_embed` chunks (cache misses) in the
    /// batch. When all chunks were cache hits, the embed stages already
    /// short-circuit to a "send cached" branch with this flag `false`.
    pub uncached_need_embedding: bool,
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
    /// Per-file disk fingerprints (mtime + size + BLAKE3) for files whose last
    /// chunk is in this batch — see `ParsedBatch::file_fingerprints`.
    pub file_fingerprints: HashMap<PathBuf, FileFingerprint>,
    /// Zero-chunk files threaded through for the store stage's phantom prune —
    /// see `ParsedBatch::empty_file_fingerprints`.
    pub empty_file_fingerprints: HashMap<PathBuf, FileFingerprint>,
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
    /// When `true`, skip the actual `embed_documents()` call for
    /// cache-miss chunks and emit zero-vec sentinels stamped
    /// `needs_embedding=1` instead. The post-summary `enrichment_pass`
    /// will overwrite every chunk's embedding anyway, so the first-pass
    /// embed is wasted work under `--llm-summaries`. Cache hits still
    /// pass through with their real embeddings.
    pub skip_first_pass_embed: bool,
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
/// Parse channel depth — buffers `ParsedBatch` messages between the parse
/// and embed stages.
///
/// Default 256. Each `ParsedBatch` holds a `Vec<Chunk>` for one file_batch
/// slice; on a large repo (5000 files × 100 chunks/file × 5 KB content) a
/// 256-deep buffer caps at ~128 MB before the embed stage starts draining.
/// That stays well above the practical fill level (parse is faster than
/// embed, so the queue rarely exceeds ~10 messages even on cold builds). The
/// `CQS_PARSE_CHANNEL_DEPTH` env override lets operators set a deeper or
/// tighter cap on memory-constrained boxes.
pub(super) fn parse_channel_depth() -> usize {
    const DEFAULT_DEPTH: usize = 256;
    static DEPTH: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *DEPTH.get_or_init(|| match std::env::var("CQS_PARSE_CHANNEL_DEPTH") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(depth = n, "CQS_PARSE_CHANNEL_DEPTH override");
                n
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    default = DEFAULT_DEPTH,
                    "Invalid CQS_PARSE_CHANNEL_DEPTH, using default"
                );
                DEFAULT_DEPTH
            }
        },
        Err(_) => DEFAULT_DEPTH,
    })
}

/// Embed channel depth — heavy (embedding vectors), bounded for memory.
///
/// Pins a *byte budget* (~16 MB by default) instead of a fixed depth so the
/// channel holds the same amount of vector data regardless of embedding dim.
/// Depth scales inversely with `(batch × dim × 4)` so the buffered byte total
/// stays in the same neighborhood; a fixed depth would balloon or shrink
/// linearly with dim.
///
/// `CQS_EMBED_CHANNEL_DEPTH` env override wins verbatim. With no override
/// and `dim == 0` (test paths) we fall back to a default of 64.
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

/// Derive channel depth from a 16 MB byte budget. Each message ≈
/// `batch_size * dim * 4 bytes` of f32 vectors. Clamp `[16, 256]`.
fn derive_depth_from_budget(dim: usize, batch_size: usize) -> usize {
    const BYTE_BUDGET: usize = 16 * 1024 * 1024;
    if dim == 0 || batch_size == 0 {
        return 64; // default for test / unknown paths
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

/// Fixed-batch helper kept ONLY for callers without a `ModelConfig` in scope
/// (currently: nothing in production, only the in-tree tests
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

/// Scale the embed batch size with the active model's dim & seq.
///
/// The implementation lives on [`cqs::embedder::ModelConfig`] so
/// `Embedder::embed_documents` (which only has `&ModelConfig` in scope) can
/// use the same scaling rule. This thin wrapper is kept for cli-side callers
/// that already had the function path baked in.
pub(crate) fn embed_batch_size_for(model: &cqs::embedder::ModelConfig) -> usize {
    model.embed_batch_size()
}
