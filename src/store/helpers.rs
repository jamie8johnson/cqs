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
/// - v10: Current (sentiment in embeddings, call graph, notes)
pub const CURRENT_SCHEMA_VERSION: i32 = 10;
pub const MODEL_NAME: &str = "intfloat/e5-base-v2";
/// Expected embedding dimensions (768 from model + 1 sentiment)
pub const EXPECTED_DIMENSIONS: u32 = 769;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("System time error: file mtime before Unix epoch")]
    SystemTime,
    #[error("Runtime error: {0}")]
    Runtime(String),
    #[error("Schema version mismatch: index is v{0}, cq expects v{1}. Run 'cq index --force' to rebuild.")]
    SchemaMismatch(i32, i32),
    #[error("Index created by newer cq version (schema v{0}). Please upgrade cq.")]
    SchemaNewerThanCq(i32),
    #[error(
        "Model mismatch: index uses '{0}', current is '{1}'. Run 'cq index --force' to re-embed."
    )]
    ModelMismatch(String, String),
    #[error(
        "Dimension mismatch: index has {0}-dim embeddings, current model expects {1}. Run 'cq index --force' to rebuild."
    )]
    DimensionMismatch(u32, u32),
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
        }
    }
}

/// A search result with similarity score
#[derive(Debug)]
pub struct SearchResult {
    /// The matching chunk
    pub chunk: ChunkSummary,
    /// Similarity score (0.0 to 1.0, higher is better)
    pub score: f32,
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
}

/// Filter and scoring options for search
///
/// All fields are optional. Unset filters match all chunks.
/// Use `validate()` to check constraints before searching.
pub struct SearchFilter {
    /// Filter by programming language(s)
    pub languages: Option<Vec<Language>>,
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
}

impl Default for SearchFilter {
    fn default() -> Self {
        Self {
            languages: None,
            path_pattern: None,
            name_boost: 0.0,
            query_text: String::new(),
            enable_rrf: false,
            note_weight: 1.0, // Notes weighted equally by default
        }
    }
}

impl SearchFilter {
    /// Create a new SearchFilter with default values.
    ///
    /// Use builder methods to customize:
    /// ```ignore
    /// let filter = SearchFilter::new()
    ///     .with_language(Language::Rust)
    ///     .with_path_pattern("src/**/*.rs")
    ///     .with_query("retry logic");
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter results to a specific programming language.
    pub fn with_language(mut self, lang: Language) -> Self {
        self.languages = Some(vec![lang]);
        self
    }

    /// Filter results to multiple programming languages.
    pub fn with_languages(mut self, langs: Vec<Language>) -> Self {
        self.languages = Some(langs);
        self
    }

    /// Filter results by file path glob pattern.
    pub fn with_path_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.path_pattern = Some(pattern.into());
        self
    }

    /// Set the query text (required for name_boost > 0 or enable_rrf).
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query_text = query.into();
        self
    }

    /// Set name boost weight for hybrid search (0.0-1.0).
    pub fn with_name_boost(mut self, boost: f32) -> Self {
        self.name_boost = boost;
        self
    }

    /// Enable or disable RRF hybrid search.
    pub fn with_rrf(mut self, enabled: bool) -> Self {
        self.enable_rrf = enabled;
        self
    }

    /// Set note weight multiplier (0.0-1.0).
    pub fn with_note_weight(mut self, weight: f32) -> Self {
        self.note_weight = weight;
        self
    }

    /// Validate filter constraints
    ///
    /// Returns Ok(()) if valid, or Err with description of what's wrong.
    pub fn validate(&self) -> Result<(), &'static str> {
        // name_boost must be in [0.0, 1.0]
        if self.name_boost < 0.0 || self.name_boost > 1.0 {
            return Err("name_boost must be between 0.0 and 1.0");
        }

        // note_weight must be in [0.0, 1.0]
        if self.note_weight < 0.0 || self.note_weight > 1.0 {
            return Err("note_weight must be between 0.0 and 1.0");
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
/// # Panics
/// Panics if embedding is not exactly 769 dimensions (768 model + 1 sentiment).
/// This is intentional - storing wrong-sized embeddings corrupts the index.
pub fn embedding_to_bytes(embedding: &Embedding) -> Vec<u8> {
    assert_eq!(
        embedding.len(),
        EXPECTED_DIMENSIONS as usize,
        "Embedding dimension mismatch: expected {}, got {}. This indicates a bug in the embedder.",
        EXPECTED_DIMENSIONS,
        embedding.len()
    );
    embedding
        .as_slice()
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Zero-copy view of embedding bytes as f32 slice (for hot paths)
///
/// Returns None if byte length doesn't match expected embedding size.
/// Uses trace level logging to avoid impacting search performance.
pub fn embedding_slice(bytes: &[u8]) -> Option<&[f32]> {
    const EXPECTED_BYTES: usize = 769 * 4; // 768 model + 1 sentiment
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
    const EXPECTED_BYTES: usize = 769 * 4;
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
}
