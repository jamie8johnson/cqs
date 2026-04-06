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
mod sql;
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
/// - v16: Current (composite PK on llm_summaries: content_hash + purpose)
/// - v15: 768-dim embeddings -- dropped sentiment dimension (SQ-9)
/// - v14: llm_summaries table for SQ-6
/// - v13: enrichment_hash for idempotent enrichment, hnsw_dirty flag
/// - v12: parent_type_name column for method->class association
/// - v11: type_edges table for type-level dependency tracking
/// - v10: sentiment in embeddings, call graph, notes
pub const CURRENT_SCHEMA_VERSION: i32 = 17;

/// Default model name for metadata checks (used by test-only `check_model_version`).
/// Canonical definition is `embedder::DEFAULT_MODEL_REPO`.
#[cfg(test)]
pub(crate) const DEFAULT_MODEL_NAME: &str = crate::embedder::DEFAULT_MODEL_REPO;

/// AD-52: ModelInfo moved to embedder::models where it logically belongs.
/// Re-exported here so `store::helpers::ModelInfo` and `store::ModelInfo` continue to work.
pub use crate::embedder::ModelInfo;
