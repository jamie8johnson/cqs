//! Data types for the parser module

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
