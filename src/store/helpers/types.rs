//! Domain types for store query results.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::parser::{Chunk, ChunkType, Language};

use super::rows::ChunkRow;

/// Chunk metadata returned from search results
///
/// Contains all chunk information except the embedding vector.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkSummary {
    /// Unique identifier
    pub id: String,
    /// Source file path (always forward-slash normalized, not OS-native).
    ///
    /// Paths are normalized by `normalize_path()` during indexing: backslashes
    /// are converted to forward slashes for consistent cross-platform storage and
    /// querying. The path itself is typically absolute.
    #[serde(serialize_with = "crate::serialize_path_normalized")]
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
    /// Content hash (blake3) for embedding cache and summary lookup
    pub content_hash: String,
    /// Window index (None = not windowed, 0 = first window, 1+ = subsequent)
    pub window_idx: Option<i32>,
    /// Parent chunk ID if this is a child chunk (table, windowed)
    pub parent_id: Option<String>,
    /// For methods: name of enclosing class/struct/impl
    pub parent_type_name: Option<String>,
}

impl From<&ChunkSummary> for Chunk {
    fn from(cs: &ChunkSummary) -> Self {
        Self {
            id: cs.id.clone(),
            file: cs.file.clone(),
            language: cs.language,
            chunk_type: cs.chunk_type,
            name: cs.name.clone(),
            signature: cs.signature.clone(),
            content: cs.content.clone(),
            doc: cs.doc.clone(),
            line_start: cs.line_start,
            line_end: cs.line_end,
            content_hash: cs.content_hash.clone(),
            parent_id: cs.parent_id.clone(),
            window_idx: cs.window_idx.map(|i| i as u32),
            parent_type_name: cs.parent_type_name.clone(),
        }
    }
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
            content_hash: row.content_hash,
            window_idx: row.window_idx,
            parent_id: row.parent_id,
            parent_type_name: row.parent_type_name,
        }
    }
}

/// A search result with similarity score.
///
/// Serialization uses explicit `to_json()` / `to_json_relative()` methods instead of
/// `derive(Serialize)` to produce a lean, stable field set: only user-visible fields
/// are included, with `has_parent` (bool) instead of raw `parent_id` (Option<String>),
/// and paths normalized to forward slashes. The derive was removed (AD-27) to avoid
/// two divergent serialization paths.
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
            "file": crate::normalize_path(&self.chunk.file),
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
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallerInfo {
    /// Function name
    pub name: String,
    /// Source file path
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    /// Line where function starts
    #[serde(rename = "line_start")]
    pub line: u32,
}

/// Caller with call-site context for impact analysis
///
/// Enriches CallerInfo with the specific line where the call occurs,
/// enabling snippet extraction without reading the source file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallerWithContext {
    /// Function name of the caller
    pub name: String,
    /// Source file path
    #[serde(serialize_with = "crate::serialize_path_normalized")]
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
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallGraph {
    /// Forward edges: caller_name -> Vec<callee_name>
    pub forward: HashMap<Arc<str>, Vec<Arc<str>>>,
    /// Reverse edges: callee_name -> Vec<caller_name>
    pub reverse: HashMap<Arc<str>, Vec<Arc<str>>>,
}

impl CallGraph {
    /// Construct from owned `String` maps, interning all strings into `Arc<str>`.
    ///
    /// Convenience for tests and ad-hoc graph construction. Production code uses
    /// the interner in `get_call_graph()` for shared allocation across maps.
    pub fn from_string_maps(
        forward: HashMap<String, Vec<String>>,
        reverse: HashMap<String, Vec<String>>,
    ) -> Self {
        let convert = |m: HashMap<String, Vec<String>>| -> HashMap<Arc<str>, Vec<Arc<str>>> {
            m.into_iter()
                .map(|(k, vs)| {
                    let k: Arc<str> = Arc::from(k.as_str());
                    let vs: Vec<Arc<str>> = vs.into_iter().map(|v| Arc::from(v.as_str())).collect();
                    (k, vs)
                })
                .collect()
        };
        Self {
            forward: convert(forward),
            reverse: convert(reverse),
        }
    }
}

/// Chunk identity for diff comparison
///
/// Minimal metadata needed to identify and match chunks across stores.
/// Does not include content or embeddings.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkIdentity {
    /// Unique chunk identifier
    pub id: String,
    /// Source file path
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
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
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteStats {
    /// Total number of notes
    pub total: u64,
    /// Notes with negative sentiment (warnings)
    pub warnings: u64,
    /// Notes with positive sentiment (patterns)
    pub patterns: u64,
}

/// Note metadata returned from search results
#[derive(Debug, Clone, serde::Serialize)]
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
///
/// No longer surfaced in unified search results (SQ-9).
/// `search_notes()` was removed; this type is retained for backward compatibility.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteSearchResult {
    /// The matching note
    #[serde(flatten)]
    pub note: NoteSummary,
    /// Similarity score (0.0 to 1.0)
    pub score: f32,
}

/// A file in the index whose content has changed on disk
#[derive(Debug, Clone, serde::Serialize)]
pub struct StaleFile {
    /// Source file path (as stored in the index)
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    /// Mtime stored in the index (Unix seconds)
    pub stored_mtime: i64,
    /// Current mtime on disk (Unix seconds)
    pub current_mtime: i64,
}

/// Report of index freshness
#[derive(Debug, Clone, serde::Serialize)]
pub struct StaleReport {
    /// Files whose disk mtime is newer than stored mtime
    pub stale: Vec<StaleFile>,
    /// Files in the index that no longer exist on disk
    pub missing: Vec<PathBuf>,
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

/// Unified search result (code-only after SQ-9 Phase 1).
///
/// Wraps a `SearchResult` to maintain API compatibility with callers
/// that previously handled both code and note results.
#[derive(Debug, Clone)]
pub enum UnifiedResult {
    /// A code chunk search result
    Code(SearchResult),
}

impl UnifiedResult {
    /// Retrieves the score from the unified result.
    pub fn score(&self) -> f32 {
        match self {
            UnifiedResult::Code(r) => r.score,
        }
    }

    /// Serialize to JSON with consistent field order.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            UnifiedResult::Code(r) => {
                let mut json = r.to_json();
                json["type"] = serde_json::json!("code");
                json
            }
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
        }
    }
}

/// Index statistics
///
/// Provides overview information about the indexed codebase.
/// Retrieved via `Store::stats()`.
#[derive(Debug, Clone, serde::Serialize)]
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
    /// Embedding model used (e.g., "BAAI/bge-large-en-v1.5")
    pub model_name: String,
    /// Database schema version
    pub schema_version: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
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

    // ===== HP-7: SearchResult::to_json field completeness =====

    /// Helper: build a SearchResult with distinct values for every field
    /// so assertions can verify each field maps to the correct source.
    fn make_detailed_result() -> SearchResult {
        SearchResult {
            chunk: ChunkSummary {
                id: "chunk-42".to_string(),
                file: PathBuf::from("src/engine/search.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::parser::ChunkType::Function,
                name: "search_filtered".to_string(),
                signature: "pub fn search_filtered(query: &str) -> Vec<Result>".to_string(),
                content: "pub fn search_filtered(query: &str) -> Vec<Result> { todo!() }"
                    .to_string(),
                doc: Some("Searches with filtering".to_string()),
                line_start: 10,
                line_end: 25,
                parent_id: Some("parent-impl".to_string()),
                parent_type_name: Some("SearchEngine".to_string()),
                content_hash: "abc123".to_string(),
                window_idx: None,
            },
            score: 0.9375,
        }
    }

    #[test]
    fn test_to_json_all_fields_present() {
        let result = make_detailed_result();
        let json = result.to_json();
        let obj = json.as_object().expect("to_json should return an object");

        // Exactly these 10 fields, no more, no fewer.
        let expected_keys: std::collections::HashSet<&str> = [
            "file",
            "line_start",
            "line_end",
            "name",
            "signature",
            "language",
            "chunk_type",
            "score",
            "content",
            "has_parent",
        ]
        .iter()
        .copied()
        .collect();

        let actual_keys: std::collections::HashSet<&str> = obj.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            expected_keys,
            actual_keys,
            "to_json field set mismatch: extra={:?}, missing={:?}",
            actual_keys.difference(&expected_keys).collect::<Vec<_>>(),
            expected_keys.difference(&actual_keys).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_to_json_field_values() {
        let result = make_detailed_result();
        let json = result.to_json();

        // File path normalized to forward slashes
        let file_str = json["file"].as_str().unwrap();
        assert!(
            file_str.contains("src/engine/search.rs"),
            "file should contain path, got: {file_str}"
        );
        assert!(!file_str.contains('\\'), "file should use forward slashes");

        assert_eq!(json["line_start"], 10);
        assert_eq!(json["line_end"], 25);
        assert_eq!(json["name"], "search_filtered");
        assert_eq!(
            json["signature"],
            "pub fn search_filtered(query: &str) -> Vec<Result>"
        );
        assert_eq!(json["language"], "rust");
        assert_eq!(json["chunk_type"], "function");
        assert_eq!(json["has_parent"], true);
        assert_eq!(
            json["content"],
            "pub fn search_filtered(query: &str) -> Vec<Result> { todo!() }"
        );

        // Score is f32 -> JSON number; check approximate equality
        let score = json["score"].as_f64().unwrap();
        assert!(
            (score - 0.9375).abs() < 1e-4,
            "score should be ~0.9375, got {score}"
        );
    }

    #[test]
    fn test_to_json_no_parent() {
        let result = SearchResult {
            chunk: make_chunk("standalone", None),
            score: 0.5,
        };
        let json = result.to_json();
        assert_eq!(json["has_parent"], false);
        // parent_id itself should NOT leak into JSON
        assert!(
            json.get("parent_id").is_none(),
            "parent_id should not appear in JSON output"
        );
    }

    #[test]
    fn test_to_json_relative_all_fields_present() {
        let root = std::path::Path::new("src/engine");
        let result = make_detailed_result();
        let json = result.to_json_relative(root);
        let obj = json
            .as_object()
            .expect("to_json_relative should return an object");

        // Same field set as to_json
        let expected_keys: std::collections::HashSet<&str> = [
            "file",
            "line_start",
            "line_end",
            "name",
            "signature",
            "language",
            "chunk_type",
            "score",
            "content",
            "has_parent",
        ]
        .iter()
        .copied()
        .collect();

        let actual_keys: std::collections::HashSet<&str> = obj.keys().map(|k| k.as_str()).collect();
        assert_eq!(expected_keys, actual_keys);
    }

    #[test]
    fn test_to_json_relative_strips_prefix() {
        let root = std::path::Path::new("src/engine");
        let result = make_detailed_result();
        let json = result.to_json_relative(root);

        let file_str = json["file"].as_str().unwrap();
        // Should strip the root prefix, leaving just "search.rs"
        assert!(
            !file_str.starts_with("src/engine/"),
            "relative path should strip root prefix, got: {file_str}"
        );
        assert!(
            file_str.contains("search.rs"),
            "relative path should still contain filename, got: {file_str}"
        );
    }

    #[test]
    fn test_to_json_different_chunk_types() {
        for (chunk_type, expected_str) in [
            (crate::parser::ChunkType::Function, "function"),
            (crate::parser::ChunkType::Struct, "struct"),
            (crate::parser::ChunkType::Method, "method"),
            (crate::parser::ChunkType::Trait, "trait"),
            (crate::parser::ChunkType::Enum, "enum"),
            (crate::parser::ChunkType::Module, "module"),
        ] {
            let result = SearchResult {
                chunk: ChunkSummary {
                    chunk_type,
                    ..make_chunk("test_fn", None)
                },
                score: 0.5,
            };
            let json = result.to_json();
            assert_eq!(
                json["chunk_type"], expected_str,
                "chunk_type mismatch for {:?}",
                chunk_type
            );
        }
    }

    #[test]
    fn test_to_json_different_languages() {
        for (lang, expected_str) in [
            (crate::parser::Language::Rust, "rust"),
            (crate::parser::Language::Python, "python"),
            (crate::parser::Language::TypeScript, "typescript"),
            (crate::parser::Language::Java, "java"),
            (crate::parser::Language::Go, "go"),
        ] {
            let result = SearchResult {
                chunk: ChunkSummary {
                    language: lang,
                    ..make_chunk("test_fn", None)
                },
                score: 0.5,
            };
            let json = result.to_json();
            assert_eq!(
                json["language"], expected_str,
                "language mismatch for {:?}",
                lang
            );
        }
    }

    #[test]
    fn test_to_json_score_boundary_values() {
        // Score = 0.0
        let result = SearchResult {
            chunk: make_chunk("zero", None),
            score: 0.0,
        };
        let json = result.to_json();
        let s = json["score"].as_f64().unwrap();
        assert!((s - 0.0).abs() < 1e-6, "score 0.0, got {s}");

        // Score = 1.0
        let result = SearchResult {
            chunk: make_chunk("perfect", None),
            score: 1.0,
        };
        let json = result.to_json();
        let s = json["score"].as_f64().unwrap();
        assert!((s - 1.0).abs() < 1e-6, "score 1.0, got {s}");
    }

    // ===== HP-7: UnifiedResult::to_json wrapping =====

    #[test]
    fn test_unified_result_to_json_adds_type_field() {
        let result = UnifiedResult::Code(make_detailed_result());
        let json = result.to_json();

        // UnifiedResult::Code adds a "type" field on top of SearchResult fields
        assert_eq!(json["type"], "code");
        // All SearchResult fields still present
        assert_eq!(json["name"], "search_filtered");
        assert_eq!(json["has_parent"], true);
        assert!(json["score"].as_f64().is_some());
    }

    #[test]
    fn test_unified_result_to_json_relative_adds_type_field() {
        let root = std::path::Path::new("src/engine");
        let result = UnifiedResult::Code(make_detailed_result());
        let json = result.to_json_relative(root);

        assert_eq!(json["type"], "code");
        assert_eq!(json["name"], "search_filtered");
        let file_str = json["file"].as_str().unwrap();
        assert!(
            !file_str.starts_with("src/engine/"),
            "relative path should strip root"
        );
    }

    #[test]
    fn test_unified_result_score() {
        let result = UnifiedResult::Code(SearchResult {
            chunk: make_chunk("test", None),
            score: 0.42,
        });
        let s = result.score();
        assert!((s - 0.42).abs() < 1e-6);
    }
}
