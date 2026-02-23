//! Store helper types and embedding conversion functions

use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

use crate::embedder::Embedding;
use crate::parser::{ChunkType, Language};

/// Schema version for database migrations
///
/// Increment this when changing the database schema. Store::open() checks this
/// against the stored version and returns StoreError::SchemaMismatch if different.
///
/// History:
/// - v11: Current (type_edges table for type-level dependency tracking)
/// - v10: sentiment in embeddings, call graph, notes
pub const CURRENT_SCHEMA_VERSION: i32 = 11;
pub const MODEL_NAME: &str = "intfloat/e5-base-v2";
/// Expected embedding dimensions — derived from crate::EMBEDDING_DIM
pub const EXPECTED_DIMENSIONS: u32 = crate::EMBEDDING_DIM as u32;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("System time error: file mtime before Unix epoch")]
    SystemTime,
    #[error("Runtime error: {0}")]
    /// Catch-all for errors that don't fit other variants: tokio runtime init,
    /// JSON serialization failures, and query resolution errors.
    /// No caller currently matches on this variant specifically.
    Runtime(String),
    #[error("Schema version mismatch in {0}: index is v{1}, cqs expects v{2}. Run 'cqs index --force' to rebuild.")]
    SchemaMismatch(String, i32, i32),
    #[error("Index created by newer cqs version (schema v{0}). Please upgrade cqs.")]
    SchemaNewerThanCq(i32),
    #[error("No migration path from schema v{0} to v{1}. Run 'cqs index --force' to rebuild.")]
    MigrationNotSupported(i32, i32),
    #[error(
        "Model mismatch: index uses '{0}', current is '{1}'. Run 'cqs index --force' to re-embed."
    )]
    ModelMismatch(String, String),
    #[error(
        "Dimension mismatch: index has {0}-dim embeddings, current model expects {1}. Run 'cqs index --force' to rebuild."
    )]
    DimensionMismatch(u32, u32),
    #[error("Database integrity check failed: {0}")]
    Corruption(String),
}

/// Raw row from chunks table (crate-internal, used by search module)
#[derive(Clone)]
pub(crate) struct ChunkRow {
    pub id: String,
    pub origin: String,
    pub language: String,
    pub chunk_type: String,
    pub name: String,
    pub signature: String,
    pub content: String,
    pub doc: Option<String>,
    pub line_start: u32,
    pub line_end: u32,
    pub parent_id: Option<String>,
}

impl ChunkRow {
    /// Construct from a SQLite row containing columns:
    /// id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id
    pub(crate) fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        use sqlx::Row;
        ChunkRow {
            id: row.get("id"),
            origin: row.get("origin"),
            language: row.get("language"),
            chunk_type: row.get("chunk_type"),
            name: row.get("name"),
            signature: row.get("signature"),
            content: row.get("content"),
            doc: row.get("doc"),
            line_start: clamp_line_number(row.get::<i64, _>("line_start")),
            line_end: clamp_line_number(row.get::<i64, _>("line_end")),
            parent_id: row.get("parent_id"),
        }
    }
}

/// Chunk metadata returned from search results
///
/// Contains all chunk information except the embedding vector.
#[derive(Debug, Clone)]
pub struct ChunkSummary {
    /// Unique identifier
    pub id: String,
    /// Source file path (typically absolute, as stored during indexing)
    pub file: PathBuf,
    /// Programming language
    pub language: Language,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Name of the function/class/etc.
    pub name: String,
    /// Function signature or declaration
    pub signature: String,
    /// Full source code
    pub content: String,
    /// Documentation comment if present
    pub doc: Option<String>,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Ending line number (1-indexed)
    pub line_end: u32,
    /// Parent chunk ID if this is a child chunk (table, windowed)
    pub parent_id: Option<String>,
}

impl From<ChunkRow> for ChunkSummary {
    fn from(row: ChunkRow) -> Self {
        let language = row.language.parse().unwrap_or_else(|_| {
            tracing::warn!(
                chunk_id = %row.id,
                stored_value = %row.language,
                "Failed to parse language from database, defaulting to Rust"
            );
            Language::Rust
        });
        let chunk_type = row.chunk_type.parse().unwrap_or_else(|_| {
            tracing::warn!(
                chunk_id = %row.id,
                stored_value = %row.chunk_type,
                "Failed to parse chunk_type from database, defaulting to Function"
            );
            ChunkType::Function
        });
        ChunkSummary {
            id: row.id,
            file: PathBuf::from(row.origin),
            language,
            chunk_type,
            name: row.name,
            signature: row.signature,
            content: row.content,
            doc: row.doc,
            line_start: row.line_start,
            line_end: row.line_end,
            parent_id: row.parent_id,
        }
    }
}

/// A search result with similarity score
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The matching chunk
    pub chunk: ChunkSummary,
    /// Similarity score (0.0 to 1.0, higher is better)
    pub score: f32,
}

impl SearchResult {
    /// Serialize to JSON with consistent field order and platform-normalized paths.
    ///
    /// Normalizes file paths to forward slashes for cross-platform consistency.
    /// Includes all chunk metadata plus score.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "file": self.chunk.file.to_string_lossy().replace('\\', "/"),
            "line_start": self.chunk.line_start,
            "line_end": self.chunk.line_end,
            "name": self.chunk.name,
            "signature": self.chunk.signature,
            "language": self.chunk.language.to_string(),
            "chunk_type": self.chunk.chunk_type.to_string(),
            "score": self.score,
            "content": self.chunk.content,
            "has_parent": self.chunk.parent_id.is_some(),
        })
    }

    /// Serialize to JSON with file paths relative to a project root.
    ///
    /// Strips the prefix and normalizes to forward slashes.
    pub fn to_json_relative(&self, root: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "file": crate::rel_display(&self.chunk.file, root),
            "line_start": self.chunk.line_start,
            "line_end": self.chunk.line_end,
            "name": self.chunk.name,
            "signature": self.chunk.signature,
            "language": self.chunk.language.to_string(),
            "chunk_type": self.chunk.chunk_type.to_string(),
            "score": self.score,
            "content": self.chunk.content,
            "has_parent": self.chunk.parent_id.is_some(),
        })
    }
}

/// Caller information from the full call graph
///
/// Unlike ChunkSummary, this doesn't require a chunk to exist -
/// it captures callers from large functions that exceed chunk size limits.
#[derive(Debug, Clone)]
pub struct CallerInfo {
    /// Function name
    pub name: String,
    /// Source file path
    pub file: PathBuf,
    /// Line where function starts
    pub line: u32,
}

/// Caller with call-site context for impact analysis
///
/// Enriches CallerInfo with the specific line where the call occurs,
/// enabling snippet extraction without reading the source file.
#[derive(Debug, Clone)]
pub struct CallerWithContext {
    /// Function name of the caller
    pub name: String,
    /// Source file path
    pub file: PathBuf,
    /// Line where the calling function starts
    pub line: u32,
    /// Line where the call to the target occurs
    pub call_line: u32,
}

/// In-memory call graph for BFS traversal
///
/// Built from a single scan of the `function_calls` table.
/// Both forward and reverse adjacency lists are included
/// to support trace (forward BFS) and impact/test-map (reverse BFS).
pub struct CallGraph {
    /// Forward edges: caller_name -> Vec<callee_name>
    pub forward: HashMap<String, Vec<String>>,
    /// Reverse edges: callee_name -> Vec<caller_name>
    pub reverse: HashMap<String, Vec<String>>,
}

/// Chunk identity for diff comparison
///
/// Minimal metadata needed to identify and match chunks across stores.
/// Does not include content or embeddings.
#[derive(Debug, Clone)]
pub struct ChunkIdentity {
    /// Unique chunk identifier
    pub id: String,
    /// Source file path
    pub origin: String,
    /// Function/class/etc. name
    pub name: String,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Programming language
    pub language: Language,
    /// Parent chunk ID (for windowed chunks)
    pub parent_id: Option<String>,
    /// Window index within parent (for long functions split into windows)
    pub window_idx: Option<u32>,
}

/// Note statistics (total count and categorized counts)
#[derive(Debug, Clone)]
pub struct NoteStats {
    /// Total number of notes
    pub total: u64,
    /// Notes with negative sentiment (warnings)
    pub warnings: u64,
    /// Notes with positive sentiment (patterns)
    pub patterns: u64,
}

/// Note metadata returned from search results
#[derive(Debug, Clone)]
pub struct NoteSummary {
    /// Unique identifier
    pub id: String,
    /// Note content
    pub text: String,
    /// Sentiment: -1.0 to +1.0
    pub sentiment: f32,
    /// Mentioned code paths/functions
    pub mentions: Vec<String>,
}

/// A note search result with similarity score
#[derive(Debug)]
pub struct NoteSearchResult {
    /// The matching note
    pub note: NoteSummary,
    /// Similarity score (0.0 to 1.0)
    pub score: f32,
}

impl NoteSearchResult {
    /// Serialize to JSON with consistent field order.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "note",
            "id": self.note.id,
            "text": self.note.text,
            "sentiment": self.note.sentiment,
            "mentions": self.note.mentions,
            "score": self.score,
        })
    }
}

/// A file in the index whose content has changed on disk
#[derive(Debug, Clone)]
pub struct StaleFile {
    /// Source file path (as stored in the index)
    pub origin: String,
    /// Mtime stored in the index (Unix seconds)
    pub stored_mtime: i64,
    /// Current mtime on disk (Unix seconds)
    pub current_mtime: i64,
}

/// Report of index freshness
#[derive(Debug)]
pub struct StaleReport {
    /// Files whose disk mtime is newer than stored mtime
    pub stale: Vec<StaleFile>,
    /// Files in the index that no longer exist on disk
    pub missing: Vec<String>,
    /// Total number of unique files in the index
    pub total_indexed: u64,
}

/// Parent context for expanded search results (small-to-big retrieval)
#[derive(Debug, Clone)]
pub struct ParentContext {
    /// Parent chunk name
    pub name: String,
    /// Parent content (full section text)
    pub content: String,
    /// Parent line range
    pub line_start: u32,
    pub line_end: u32,
}

/// Unified search result (code chunk or note)
///
/// Search can return both code chunks and notes. This enum allows
/// handling them uniformly while preserving type-specific data.
#[derive(Debug)]
pub enum UnifiedResult {
    /// A code chunk search result
    Code(SearchResult),
    /// A note search result
    Note(NoteSearchResult),
}

impl UnifiedResult {
    /// Get the similarity score
    pub fn score(&self) -> f32 {
        match self {
            UnifiedResult::Code(r) => r.score,
            UnifiedResult::Note(r) => r.score,
        }
    }

    /// Serialize to JSON with consistent field order.
    ///
    /// Code results include `"type": "code"` prefix; note results include `"type": "note"`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            UnifiedResult::Code(r) => {
                let mut json = r.to_json();
                json["type"] = serde_json::json!("code");
                json
            }
            UnifiedResult::Note(r) => r.to_json(),
        }
    }

    /// Serialize to JSON with file paths relative to a project root.
    pub fn to_json_relative(&self, root: &std::path::Path) -> serde_json::Value {
        match self {
            UnifiedResult::Code(r) => {
                let mut json = r.to_json_relative(root);
                json["type"] = serde_json::json!("code");
                json
            }
            UnifiedResult::Note(r) => r.to_json(),
        }
    }
}

/// Filter and scoring options for search.
///
/// Fields are public for direct construction via struct literals.
/// Builder methods ([`SearchFilter::new()`], [`SearchFilter::with_query()`])
/// are provided as convenience helpers for common patterns — both styles
/// are supported and equivalent.
///
/// All fields are optional. Unset filters match all chunks.
/// Use [`SearchFilter::validate()`] to check constraints before searching.
pub struct SearchFilter {
    /// Filter by programming language(s)
    pub languages: Option<Vec<Language>>,
    /// Filter by chunk type(s) (function, method, class, struct, enum, trait, interface, constant)
    pub chunk_types: Option<Vec<ChunkType>>,
    /// Filter by file path glob pattern (e.g., `src/**/*.rs`)
    pub path_pattern: Option<String>,
    /// Weight for name matching in hybrid search (0.0-1.0)
    ///
    /// 0.0 = pure embedding similarity (default)
    /// 1.0 = pure name matching
    /// 0.2 = recommended for balanced results
    pub name_boost: f32,
    /// Query text for name matching (required if name_boost > 0 or enable_rrf)
    pub query_text: String,
    /// Enable RRF (Reciprocal Rank Fusion) hybrid search
    ///
    /// When enabled, combines semantic search results with FTS5 keyword search
    /// using the formula: score = Σ 1/(k + rank), where k=60.
    /// This typically improves recall for identifier-heavy queries.
    pub enable_rrf: bool,
    /// Weight multiplier for note scores in unified search (0.0-1.0)
    ///
    /// 1.0 = notes scored equally with code (default)
    /// 0.5 = notes scored at half weight
    /// 0.0 = notes excluded from results
    pub note_weight: f32,
    /// When true, return only notes (skip code search entirely)
    pub note_only: bool,
    /// Apply search-time demotion for test functions and underscore-prefixed names.
    ///
    /// Test functions (`test_*`, `Test*`) get 0.90x multiplier.
    /// Underscore-prefixed private names (`_foo` but not `__dunder__`) get 0.95x.
    /// Disable with `--no-demote` CLI flag.
    pub enable_demotion: bool,
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self {
            languages: None,
            chunk_types: None,
            path_pattern: None,
            name_boost: 0.0,
            query_text: String::new(),
            enable_rrf: false,
            note_weight: 1.0, // Notes weighted equally by default
            note_only: false,
            enable_demotion: true, // Demote test functions by default
        }
    }
}

impl SearchFilter {
    /// Create a new SearchFilter with default values.
    ///
    /// Use struct literal syntax to customize:
    /// ```ignore
    /// let filter = SearchFilter {
    ///     languages: Some(vec![Language::Rust]),
    ///     path_pattern: Some("src/**/*.rs".to_string()),
    ///     query_text: "retry logic".to_string(),
    ///     ..SearchFilter::new()
    /// };
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the query text (required for name_boost > 0 or enable_rrf).
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query_text = query.into();
        self
    }

    /// Validate filter constraints
    ///
    /// Returns Ok(()) if valid, or Err with description of what's wrong.
    pub fn validate(&self) -> Result<(), &'static str> {
        // name_boost must be in [0.0, 1.0] (NaN-safe: NaN is not contained in any range)
        if !(0.0..=1.0).contains(&self.name_boost) {
            return Err("name_boost must be between 0.0 and 1.0");
        }

        // note_weight must be in [0.0, 1.0] (NaN-safe)
        if !(0.0..=1.0).contains(&self.note_weight) {
            return Err("note_weight must be between 0.0 and 1.0");
        }

        // note_only with note_weight=0 is contradictory
        if self.note_only && self.note_weight == 0.0 {
            return Err("note_only=true with note_weight=0.0 is contradictory");
        }

        // query_text required when name_boost > 0 or enable_rrf
        if (self.name_boost > 0.0 || self.enable_rrf) && self.query_text.is_empty() {
            return Err("query_text required when name_boost > 0 or enable_rrf is true");
        }

        // path_pattern must be valid glob syntax if provided
        if let Some(ref pattern) = self.path_pattern {
            if pattern.len() > 500 {
                return Err("path_pattern too long (max 500 chars)");
            }
            // Reject control characters (except tab/newline which glob might handle)
            if pattern
                .chars()
                .any(|c| c.is_control() && c != '\t' && c != '\n')
            {
                return Err("path_pattern contains invalid control characters");
            }
            // Limit brace nesting depth to prevent exponential expansion
            // e.g., "{a,{b,{c,{d,{e,...}}}}}" can cause O(2^n) expansion
            const MAX_BRACE_DEPTH: usize = 10;
            let mut depth = 0usize;
            for c in pattern.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        if depth > MAX_BRACE_DEPTH {
                            return Err("path_pattern has too many nested braces (max 10 levels)");
                        }
                    }
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if globset::Glob::new(pattern).is_err() {
                return Err("path_pattern is not a valid glob pattern");
            }
        }

        Ok(())
    }
}

/// Model metadata for index initialization
pub struct ModelInfo {
    pub name: String,
    pub dimensions: u32,
    pub version: String,
}

impl Default for ModelInfo {
    fn default() -> Self {
        ModelInfo {
            name: MODEL_NAME.to_string(),
            dimensions: 769,          // 768 from model + 1 sentiment
            version: "2".to_string(), // E5-base-v2
        }
    }
}

/// Index statistics
///
/// Provides overview information about the indexed codebase.
/// Retrieved via `Store::stats()`.
#[derive(Debug)]
pub struct IndexStats {
    /// Total number of code chunks indexed
    pub total_chunks: u64,
    /// Number of unique source files
    pub total_files: u64,
    /// Chunk count grouped by programming language
    pub chunks_by_language: HashMap<Language, u64>,
    /// Chunk count grouped by element type (function, class, etc.)
    pub chunks_by_type: HashMap<ChunkType, u64>,
    /// Database file size in bytes
    pub index_size_bytes: u64,
    /// ISO 8601 timestamp when index was created
    pub created_at: String,
    /// ISO 8601 timestamp of last update
    pub updated_at: String,
    /// Embedding model used (e.g., "intfloat/e5-base-v2")
    pub model_name: String,
    /// Database schema version
    pub schema_version: i32,
}

// ============ Name Scoring ============

/// Score a chunk name against a query for definition search (search_by_name).
///
/// Returns a score between 0.0 and 1.0:
/// - 1.0: exact match (case-insensitive)
/// - 0.9: prefix match
/// - 0.7: substring match
/// - 0.0: no name relationship
pub fn score_name_match(name: &str, query: &str) -> f32 {
    if query.is_empty() {
        return 0.0;
    }
    let name_lower = name.to_lowercase();
    let query_lower = query.to_lowercase();
    if name_lower == query_lower {
        1.0
    } else if name_lower.starts_with(&query_lower) {
        0.9
    } else if name_lower.contains(&query_lower) {
        0.7
    } else {
        0.0
    }
}

// ============ Line Number Conversion ============

/// Clamp i64 to valid u32 line number range (1-indexed)
///
/// SQLite returns i64, but line numbers are u32 and 1-indexed.
/// This safely clamps to avoid truncation issues on extreme values,
/// with minimum 1 since line 0 is invalid in 1-indexed systems.
#[inline]
pub fn clamp_line_number(n: i64) -> u32 {
    n.clamp(1, u32::MAX as i64) as u32
}

// ============ Embedding Serialization ============

/// Convert embedding to bytes for storage.
///
/// Returns an error if embedding is not exactly 769 dimensions (768 model + 1 sentiment).
/// Storing wrong-sized embeddings would corrupt the index.
pub fn embedding_to_bytes(embedding: &Embedding) -> Result<Vec<u8>, StoreError> {
    if embedding.len() != EXPECTED_DIMENSIONS as usize {
        return Err(StoreError::Runtime(format!(
            "Embedding dimension mismatch: expected {}, got {}. This indicates a bug in the embedder.",
            EXPECTED_DIMENSIONS,
            embedding.len()
        )));
    }
    Ok(bytemuck::cast_slice::<f32, u8>(embedding.as_slice()).to_vec())
}

/// Zero-copy view of embedding bytes as f32 slice (for hot paths)
///
/// Returns None if byte length doesn't match expected embedding size.
/// Uses trace level logging to avoid impacting search performance.
pub fn embedding_slice(bytes: &[u8]) -> Option<&[f32]> {
    const EXPECTED_BYTES: usize = crate::EMBEDDING_DIM * 4;
    if bytes.len() != EXPECTED_BYTES {
        tracing::trace!(
            expected = EXPECTED_BYTES,
            actual = bytes.len(),
            "Embedding byte length mismatch, skipping"
        );
        return None;
    }
    Some(bytemuck::cast_slice(bytes))
}

/// Convert embedding bytes to owned Vec (when ownership needed)
///
/// Returns None if byte length doesn't match expected embedding size (769 * 4 bytes).
/// This prevents silently using corrupted/truncated embeddings.
/// Uses trace level logging consistent with embedding_slice() since both are called on hot paths.
pub fn bytes_to_embedding(bytes: &[u8]) -> Option<Vec<f32>> {
    const EXPECTED_BYTES: usize = crate::EMBEDDING_DIM * 4;
    if bytes.len() != EXPECTED_BYTES {
        tracing::trace!(
            expected = EXPECTED_BYTES,
            actual = bytes.len(),
            "Embedding byte length mismatch, skipping"
        );
        return None;
    }
    Some(bytemuck::cast_slice::<u8, f32>(bytes).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== SearchFilter validation tests =====

    #[test]
    fn test_search_filter_valid_default() {
        let filter = SearchFilter::default();
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_valid_with_name_boost() {
        let filter = SearchFilter {
            name_boost: 0.2,
            query_text: "test".to_string(),
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_valid_with_rrf() {
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: "test".to_string(),
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_invalid_name_boost_negative() {
        let filter = SearchFilter {
            name_boost: -0.1,
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("name_boost"));
    }

    #[test]
    fn test_search_filter_invalid_name_boost_too_high() {
        let filter = SearchFilter {
            name_boost: 1.5,
            query_text: "test".to_string(),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
    }

    #[test]
    fn test_search_filter_invalid_missing_query_text() {
        let filter = SearchFilter {
            name_boost: 0.5,
            query_text: String::new(),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("query_text"));
    }

    #[test]
    fn test_search_filter_invalid_rrf_missing_query() {
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: String::new(),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
    }

    #[test]
    fn test_search_filter_valid_path_pattern() {
        let filter = SearchFilter {
            path_pattern: Some("src/**/*.rs".to_string()),
            ..Default::default()
        };
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_search_filter_invalid_path_pattern_syntax() {
        let filter = SearchFilter {
            path_pattern: Some("[invalid".to_string()),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("glob"));
    }

    #[test]
    fn test_search_filter_path_pattern_too_long() {
        let filter = SearchFilter {
            path_pattern: Some("a".repeat(501)),
            ..Default::default()
        };
        assert!(filter.validate().is_err());
        assert!(filter.validate().unwrap_err().contains("too long"));
    }

    // ===== clamp_line_number tests =====

    #[test]
    fn test_clamp_line_number_normal() {
        assert_eq!(clamp_line_number(1), 1);
        assert_eq!(clamp_line_number(100), 100);
    }

    #[test]
    fn test_clamp_line_number_negative() {
        // Line numbers are 1-indexed, so negative/zero clamps to 1
        assert_eq!(clamp_line_number(-1), 1);
        assert_eq!(clamp_line_number(-1000), 1);
        assert_eq!(clamp_line_number(0), 1);
    }

    #[test]
    fn test_clamp_line_number_overflow() {
        assert_eq!(clamp_line_number(i64::MAX), u32::MAX);
        assert_eq!(clamp_line_number(u32::MAX as i64 + 1), u32::MAX);
    }

    // ===== parent_id exposure tests =====

    fn make_chunk(name: &str, parent_id: Option<&str>) -> ChunkSummary {
        ChunkSummary {
            id: format!("id-{}", name),
            file: PathBuf::from(format!("src/{}.rs", name)),
            language: crate::parser::Language::Rust,
            chunk_type: crate::parser::ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: format!("fn {}() {{}}", name),
            doc: None,
            line_start: 1,
            line_end: 1,
            parent_id: parent_id.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_chunk_summary_includes_parent_id() {
        let chunk = make_chunk("child", Some("parent-id"));
        assert_eq!(chunk.parent_id.as_deref(), Some("parent-id"));

        let chunk_no_parent = make_chunk("standalone", None);
        assert!(chunk_no_parent.parent_id.is_none());
    }

    #[test]
    fn test_search_result_json_has_parent() {
        let result = SearchResult {
            chunk: make_chunk("child", Some("parent-id")),
            score: 0.85,
        };
        let json = result.to_json();
        assert_eq!(json["has_parent"], true);
    }

    #[test]
    fn test_search_result_json_no_parent() {
        let result = SearchResult {
            chunk: make_chunk("standalone", None),
            score: 0.85,
        };
        let json = result.to_json();
        assert_eq!(json["has_parent"], false);
    }

    // ===== score_name_match tests =====

    #[test]
    fn test_score_name_match_exact() {
        assert_eq!(score_name_match("parse_diff", "parse_diff"), 1.0);
        assert_eq!(score_name_match("Parse_Diff", "parse_diff"), 1.0);
    }

    #[test]
    fn test_score_name_match_prefix() {
        assert_eq!(score_name_match("parse_diff_hunks", "parse_diff"), 0.9);
    }

    #[test]
    fn test_score_name_match_substring() {
        assert_eq!(score_name_match("do_parse_diff", "parse_diff"), 0.7);
    }

    #[test]
    fn test_score_name_match_no_match_returns_zero() {
        assert_eq!(score_name_match("parse_diff", "reverse_bfs"), 0.0);
        assert_eq!(score_name_match("foo", "bar"), 0.0);
    }

    #[test]
    fn test_search_result_json_relative_has_parent() {
        let root = std::path::Path::new("src");
        let result = SearchResult {
            chunk: make_chunk("child", Some("parent-id")),
            score: 0.85,
        };
        let json = result.to_json_relative(root);
        assert_eq!(json["has_parent"], true);
    }

    #[test]
    fn test_score_name_match_empty_query() {
        assert_eq!(score_name_match("foo", ""), 0.0);
    }

    #[test]
    fn test_score_name_match_case_insensitive() {
        assert_eq!(score_name_match("FooBar", "foobar"), 1.0);
    }
}
