//! Store helper types and embedding conversion functions

use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

use crate::embedder::Embedding;
use crate::parser::{ChunkType, Language};

// Schema version for migrations
pub const CURRENT_SCHEMA_VERSION: i32 = 10;
pub const MODEL_NAME: &str = "intfloat/e5-base-v2";

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
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
    /// Source file path (relative to project root)
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
        ChunkSummary {
            id: row.id,
            file: PathBuf::from(row.origin),
            language: row.language.parse().unwrap_or(Language::Rust),
            chunk_type: row.chunk_type.parse().unwrap_or(ChunkType::Function),
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
#[derive(Debug)]
pub enum UnifiedResult {
    Code(SearchResult),
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
#[derive(Default)]
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
}

impl SearchFilter {
    /// Validate filter constraints
    ///
    /// Returns Ok(()) if valid, or Err with description of what's wrong.
    pub fn validate(&self) -> Result<(), &'static str> {
        // name_boost must be in [0.0, 1.0]
        if self.name_boost < 0.0 || self.name_boost > 1.0 {
            return Err("name_boost must be between 0.0 and 1.0");
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
            dimensions: 769, // 768 from model + 1 sentiment
            version: "1.5".to_string(),
        }
    }
}

/// Index statistics
#[derive(Debug)]
pub struct IndexStats {
    pub total_chunks: u64,
    pub total_files: u64,
    pub chunks_by_language: HashMap<Language, u64>,
    pub chunks_by_type: HashMap<ChunkType, u64>,
    pub index_size_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
    pub model_name: String,
    pub schema_version: i32,
}

// ============ Line Number Conversion ============

/// Clamp i64 to valid u32 line number range
///
/// SQLite returns i64, but line numbers are u32. This safely clamps
/// to avoid truncation issues on extreme values.
#[inline]
pub fn clamp_line_number(n: i64) -> u32 {
    n.clamp(0, u32::MAX as i64) as u32
}

// ============ Embedding Serialization ============

/// Convert embedding to bytes for storage
pub fn embedding_to_bytes(embedding: &Embedding) -> Vec<u8> {
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
            "embedding byte length mismatch"
        );
        return None;
    }
    Some(bytemuck::cast_slice(bytes))
}

/// Convert embedding bytes to owned Vec (when ownership needed)
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    embedding_slice(bytes)
        .map(|s| s.to_vec())
        .unwrap_or_else(|| {
            tracing::warn!(
                "Embedding byte length mismatch: expected {}, got {} (possible corruption)",
                769 * 4,
                bytes.len()
            );
            bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("4 bytes")))
                .collect()
        })
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
        assert_eq!(clamp_line_number(-1), 0);
        assert_eq!(clamp_line_number(-1000), 0);
    }

    #[test]
    fn test_clamp_line_number_overflow() {
        assert_eq!(clamp_line_number(i64::MAX), u32::MAX);
        assert_eq!(clamp_line_number(u32::MAX as i64 + 1), u32::MAX);
    }
}
