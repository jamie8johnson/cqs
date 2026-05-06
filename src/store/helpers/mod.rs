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
pub(crate) use rows::{
    CandidateRow, ChunkRow, CHUNK_ROW_SELECT_COLUMNS, CHUNK_ROW_SELECT_COLUMNS_PREFIXED,
};

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

// ============ BM25 FTS5 column weights ============
// Single source of truth for the FTS5 `bm25(chunks_fts, name, sig, content, doc)`
// argument vector. Two production query paths (`store::search::search_by_name`
// and `chunks::query::search_by_names_batch`) need to agree byte-for-byte —
// hoisted here so a tuning sweep is a one-line edit instead of two-site grep.
// Order matches the chunks_fts column order in `schema.sql:73-77`.
//
// `name` weighted 10× to prefer definition matches over content mentions when
// callers pass a function/struct name. `signature`, `content`, `doc` get the
// FTS5 default weight (1.0) — no per-column rationale yet, so the relative
// weighting is the load-bearing knob.

/// Weight applied to the `name` column in `bm25()` ordering — heavy enough to
/// pin the definition of `parse_diff` above other chunks that mention it.
pub(crate) const BM25_NAME_WEIGHT: f32 = 10.0;
/// Weight applied to the `signature` column in `bm25()`.
pub(crate) const BM25_SIGNATURE_WEIGHT: f32 = 1.0;
/// Weight applied to the `content` column in `bm25()`.
pub(crate) const BM25_CONTENT_WEIGHT: f32 = 1.0;
/// Weight applied to the `doc` column in `bm25()`.
pub(crate) const BM25_DOC_WEIGHT: f32 = 1.0;

/// Render the `bm25(chunks_fts, ...)` ordering expression with the canonical
/// column weights. Both production sites that need the heavy-name weighting
/// must call this so a tuning sweep stays single-source.
pub(crate) fn bm25_ordering_expr() -> String {
    format!(
        "bm25(chunks_fts, {}, {}, {}, {})",
        BM25_NAME_WEIGHT, BM25_SIGNATURE_WEIGHT, BM25_CONTENT_WEIGHT, BM25_DOC_WEIGHT
    )
}

// Schema version constant
/// Schema version for database migrations
///
/// Increment this when changing the database schema. Store::open() checks this
/// against the stored version and returns StoreError::SchemaMismatch if different.
///
/// History:
/// - v26: composite index `idx_chunks_source_type_origin` covering the
///   `WHERE source_type = ? + DISTINCT origin` pattern in `list_stale_files`
///   (every reconcile + `cqs status --watch-fresh`) and `prune_missing_files`.
///   Pre-v26, SQLite probed `idx_chunks_source_type` then row-visited; with
///   the composite, both filter and DISTINCT walk satisfy from a single
///   index pass. ~50× speedup expected at 50k+ chunk corpora; index size
///   ~5-15% of the chunks table. PERF-V1.33-10 / #1371.
/// - v23: source_size INTEGER + source_content_hash BLOB columns on chunks for
///   the reconcile fingerprint (issue #1219 / EX-V1.30.1-6). Layer 2 periodic
///   reconciliation previously diverged on `disk_mtime != stored_mtime` only,
///   which (a) misses content-identical-but-mtime-bumped files (`git checkout`,
///   formatter passes) — every flip re-embeds ~3-5k chunks unnecessarily — and
///   (b) misses coarse-mtime collisions on FAT32/NTFS/HFS+/SMB where two saves
///   inside one second produce identical mtimes. Both columns are nullable so
///   pre-v23 rows stay valid until first re-embed populates them; the
///   `MtimeOrHash` policy uses hash as a tiebreaker on mtime equality.
/// - v22: umap_x / umap_y REAL columns on chunks for the `cqs serve` cluster
///   view (step 3 of `docs/plans/2026-04-22-cqs-serve-3d-progressive.md`).
///   Both nullable — the columns stay NULL until `cqs index --umap` runs and
///   writes 2D projections from the chunk embeddings via the umap-learn
///   Python script (`scripts/run_umap.py`). The /api/embed/2d endpoint
///   skips chunks whose coords are NULL, so the feature is fully optional.
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
pub const CURRENT_SCHEMA_VERSION: i32 = 26;

/// Default model name for metadata checks (used by test-only `check_model_version`).
/// Canonical definition is `embedder::DEFAULT_MODEL_REPO`.
#[cfg(test)]
pub(crate) const DEFAULT_MODEL_NAME: &str = crate::embedder::DEFAULT_MODEL_REPO;

/// AD-52: ModelInfo moved to embedder::models where it logically belongs.
/// Re-exported here so `store::helpers::ModelInfo` and `store::ModelInfo` continue to work.
pub use crate::embedder::ModelInfo;
