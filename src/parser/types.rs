//! Data types for the parser module

use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;

// Re-export from language module (source of truth)
pub use crate::language::{ChunkType, Language, SignatureStyle};

/// Errors that can occur during code parsing
#[derive(Error, Debug)]
pub enum ParserError {
    /// File extension not recognized as a supported language
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),
    /// Tree-sitter failed to parse the file contents
    #[error("Failed to parse: {0}")]
    ParseFailed(String),
    /// Tree-sitter query compilation failed (indicates bug in query string)
    #[error("Failed to compile query for {0}: {1}")]
    QueryCompileFailed(String, String),
    /// File read error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// A parsed code chunk (function, method, class, etc.)
///
/// Chunks are the basic unit of indexing and search in cqs.
/// Each chunk represents a single code element extracted by tree-sitter.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Unique identifier: `{file}:{line_start}:{content_hash}` or `{parent_id}:w{window_idx}`
    pub id: String,
    /// Source file path (typically absolute during indexing, stored as provided)
    pub file: PathBuf,
    /// Programming language
    pub language: Language,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Name of the function/class/etc.
    pub name: String,
    /// Function signature or declaration line
    pub signature: String,
    /// Full source code content (may be windowed portion of original)
    pub content: String,
    /// Documentation comment if present
    pub doc: Option<String>,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Ending line number (1-indexed)
    pub line_end: u32,
    /// BLAKE3 hash of content for change detection
    pub content_hash: String,
    /// Parent chunk ID if this is a windowed portion of a larger chunk
    pub parent_id: Option<String>,
    /// Window index (0, 1, 2...) if this is a windowed portion
    pub window_idx: Option<u32>,
    /// Parent type name for methods (e.g., "CircuitBreaker" for `impl CircuitBreaker { ... }`)
    pub parent_type_name: Option<String>,
}

/// A function call site extracted from code
#[derive(Debug, Clone)]
pub struct CallSite {
    /// Name of the called function/method
    pub callee_name: String,
    /// Line number where the call occurs (1-indexed)
    pub line_number: u32,
}

/// A function with its call sites (for full call graph coverage)
#[derive(Debug, Clone)]
pub struct FunctionCalls {
    /// Function name
    pub name: String,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Function calls made by this function
    pub calls: Vec<CallSite>,
}

/// Classification of how a type is referenced in code.
///
/// Used for type-level dependency tracking (Phase 2b of moonshot).
/// Stored as string in SQLite `type_edges.edge_kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum TypeEdgeKind {
    /// Function/method parameter type: `fn foo(x: Config)`
    Param,
    /// Function/method return type: `fn foo() -> Config`
    Return,
    /// Struct/class field type: `struct Foo { config: Config }`
    Field,
    /// impl target, class extends/implements, interface embedding
    Impl,
    /// Trait/type parameter bound: `where T: Display`, `<T extends Foo>`
    Bound,
    /// Type alias target: `type Alias = Concrete`, typedef
    Alias,
}

impl TypeEdgeKind {
    /// String representation for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            TypeEdgeKind::Param => "Param",
            TypeEdgeKind::Return => "Return",
            TypeEdgeKind::Field => "Field",
            TypeEdgeKind::Impl => "Impl",
            TypeEdgeKind::Bound => "Bound",
            TypeEdgeKind::Alias => "Alias",
        }
    }
}

impl std::fmt::Display for TypeEdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TypeEdgeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Param" => Ok(TypeEdgeKind::Param),
            "Return" => Ok(TypeEdgeKind::Return),
            "Field" => Ok(TypeEdgeKind::Field),
            "Impl" => Ok(TypeEdgeKind::Impl),
            "Bound" => Ok(TypeEdgeKind::Bound),
            "Alias" => Ok(TypeEdgeKind::Alias),
            other => Err(format!("Unknown TypeEdgeKind: '{other}'")),
        }
    }
}

/// A type reference extracted from source code.
///
/// Captured by tree-sitter type queries with classified edge kinds.
/// The catch-all pattern captures types inside generics with `kind = None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    /// Name of the referenced type (e.g., "Config", "Store", "SqlitePool")
    pub type_name: String,
    /// Line number where the reference occurs (1-indexed)
    pub line_number: u32,
    /// Edge classification, or None for types only found by catch-all (inside generics, etc.)
    pub kind: Option<TypeEdgeKind>,
}

/// A code element with its type references (for full-file type graph).
///
/// One entry per chunk (function/struct/enum/trait/class) in a file.
/// Produced by `Parser::parse_file_relationships()`.
#[derive(Debug, Clone)]
pub struct ChunkTypeRefs {
    /// Chunk name (function/struct/enum/trait/class)
    pub name: String,
    /// Starting line number (1-indexed)
    pub line_start: u32,
    /// Type references used by this chunk
    pub type_refs: Vec<TypeRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_edge_kind_round_trip() {
        for kind in [
            TypeEdgeKind::Param,
            TypeEdgeKind::Return,
            TypeEdgeKind::Field,
            TypeEdgeKind::Impl,
            TypeEdgeKind::Bound,
            TypeEdgeKind::Alias,
        ] {
            let s = kind.to_string();
            let parsed: TypeEdgeKind = s.parse().unwrap();
            assert_eq!(kind, parsed, "Round-trip failed for {s}");
        }
    }
}
