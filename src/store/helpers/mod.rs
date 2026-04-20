//! Store helper types and embedding conversion functions.
//!
//! Submodules by responsibility:
//! - `error` - Store error types
//! - `rows` - Database row-to-struct conversions
//! - `types` - Domain types (ChunkSummary, SearchResult, CallerInfo, etc.)
//! - `search_filter` - Search filter and scoring options
//! - `scoring` - Name scoring functions
//! - `sql` - SQL placeholder generation
//! - `embeddings` - Embedding serialization/deserialization

mod embeddings;
mod error;
mod rows;
mod scoring;
mod search_filter;
pub(crate) mod sql;
mod types;

// ============ Re-exports ============
// All public items remain accessible at their original paths
// (e.g., `crate::store::helpers::StoreError`).

// Error types
pub use error::StoreError;

// Row types (crate-internal)
pub(crate) use rows::{CandidateRow, ChunkRow};

// Line number helper (used by rows and other store modules)
pub use rows::clamp_line_number;

// Domain types
pub use types::{
    CallGraph, CallerInfo, CallerWithContext, ChunkIdentity, ChunkSummary, IndexStats,
    NoteSearchResult, NoteStats, NoteSummary, ParentContext, SearchResult, StaleFile, StaleReport,
    UnifiedResult,
};

// Search filter
pub use search_filter::{SearchFilter, DEFAULT_NAME_BOOST};

// Scoring functions
pub use scoring::{score_name_match, score_name_match_pre_lower};

// SQL helpers (crate-internal)
pub(crate) use sql::make_placeholders;

// Embedding serialization
pub use embeddings::{bytes_to_embedding, embedding_slice, embedding_to_bytes};

// Schema version constant
/// Schema version for database migrations
///
/// Increment this when changing the database schema. Store::open() checks this
/// against the stored version and returns StoreError::SchemaMismatch if different.
///
/// History:
/// - v21: parser_version column on chunks (v1.28.0 audit P2 #29 — incremental
///   UPSERT now refreshes rows whose `content_hash` is unchanged but whose
///   `parser_version` bumped, e.g. when `extract_doc_fallback_for_short_chunk`
///   logic changes the value of `doc` for a previously-indexed chunk. Without
///   this column the watch path's content-hash short-circuit silently
///   discards the new `doc`. See `parser::chunk::PARSER_VERSION` for the
///   in-memory value and `chunks/async_helpers.rs::batch_insert_chunks` for
///   the corresponding `OR parser_version != excluded.parser_version` UPSERT
///   filter.)
/// - v20: AFTER DELETE trigger on chunks bumps splade_generation in metadata
///   (v1.22.0 audit DS-W2 / OB-22 / PB-NEW-6 — `cqs watch` never touched
///   SPLADE, so deletes that cascade to sparse_vectors left the persisted
///   `splade.index.bin` stale. The trigger fires once per deleted chunk
///   (1-200 fires per watch cycle, tolerable) and invalidates the cached
///   index without requiring instrumentation at every chunks-delete site.)
/// - v19: sparse_vectors gains FK(chunk_id) → chunks(id) ON DELETE CASCADE
///   (v1.22.0 audit DS-W3 — orphan sparse rows previously leaked on every
///   chunks-delete path; CASCADE makes the invariant structural)
/// - v18: embedding_base column on chunks (Phase 5 dual embeddings)
/// - v17: sparse_vectors table + enrichment_version column
/// - v16: composite PK on llm_summaries (content_hash + purpose)
/// - v15: 768-dim embeddings -- dropped sentiment dimension (SQ-9)
/// - v14: llm_summaries table for SQ-6
/// - v13: enrichment_hash for idempotent enrichment, hnsw_dirty flag
/// - v12: parent_type_name column for method->class association
/// - v11: type_edges table for type-level dependency tracking
/// - v10: sentiment in embeddings, call graph, notes
pub const CURRENT_SCHEMA_VERSION: i32 = 21;

/// Default model name for metadata checks (used by test-only `check_model_version`).
/// Canonical definition is `embedder::DEFAULT_MODEL_REPO`.
#[cfg(test)]
pub(crate) const DEFAULT_MODEL_NAME: &str = crate::embedder::DEFAULT_MODEL_REPO;

/// AD-52: ModelInfo moved to embedder::models where it logically belongs.
/// Re-exported here so `store::helpers::ModelInfo` and `store::ModelInfo` continue to work.
pub use crate::embedder::ModelInfo;
