//! Pipeline message types, shared state, and tuning constants.

use std::collections::HashMap;
use std::path::PathBuf;

use cqs::parser::{CallSite, ChunkTypeRefs, FunctionCalls};
use cqs::{Chunk, Embedding};

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

// Pipeline tuning constants

/// Files to parse per batch (bounded memory)
pub(super) const FILE_BATCH_SIZE: usize = 5_000;
/// Parse channel depth — lightweight (chunk metadata only), can be deeper
pub(super) const PARSE_CHANNEL_DEPTH: usize = 512;
/// Embed channel depth — heavy (embedding vectors), smaller to bound memory
pub(super) const EMBED_CHANNEL_DEPTH: usize = 64;

/// Embedding batch size. Was 32 (backed off from 64 after an undiagnosed crash at 2%).
/// Restored to 64 with debug logging (PERF-45 investigation). If it crashes again,
/// run with RUST_LOG=debug to capture batch_size/max_char_len/total_chars at failure.
/// Configurable via `CQS_EMBED_BATCH_SIZE` environment variable.
pub(super) fn embed_batch_size() -> usize {
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
