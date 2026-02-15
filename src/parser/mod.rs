//! Code parsing with tree-sitter
//!
//! Split into submodules:
//! - `types` — data structures and error types
//! - `chunk` — chunk extraction from parse trees
//! - `calls` — call site extraction for call graph

mod calls;
mod chunk;
pub mod markdown;
pub mod types;

pub use types::{
    CallSite, Chunk, ChunkType, ChunkTypeRefs, FunctionCalls, Language, ParserError,
    SignatureStyle, TypeEdgeKind, TypeRef,
};

use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::StreamingIterator;

/// Code parser using tree-sitter grammars
///
/// Extracts functions, methods, classes, and other code elements
/// from source files in supported languages.
///
/// # Example
///
/// ```no_run
/// use cqs::Parser;
///
/// let parser = Parser::new()?;
/// let chunks = parser.parse_file(std::path::Path::new("src/main.rs"))?;
/// for chunk in chunks {
///     println!("{}: {} ({})", chunk.file.display(), chunk.name, chunk.chunk_type);
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Parser {
    /// Lazily compiled queries per language (compiled on first use)
    queries: HashMap<Language, OnceCell<tree_sitter::Query>>,
    /// Lazily compiled call extraction queries per language
    call_queries: HashMap<Language, OnceCell<tree_sitter::Query>>,
    /// Lazily compiled type extraction queries per language
    type_queries: HashMap<Language, OnceCell<tree_sitter::Query>>,
}

// Note: Default impl intentionally omitted to prevent hidden panics.
// Use Parser::new() which returns Result for proper error handling.

impl Parser {
    /// Create a new parser (queries are compiled lazily on first use)
    pub fn new() -> Result<Self, ParserError> {
        let mut queries = HashMap::new();
        let mut call_queries = HashMap::new();
        let mut type_queries = HashMap::new();

        // Initialize empty OnceCells for each registered language
        // (skip grammar-less languages like Markdown — they use custom parsers)
        for def in crate::language::REGISTRY.all() {
            let lang: Language = def.name.parse().expect("registry/enum mismatch");
            if def.grammar.is_some() {
                queries.insert(lang, OnceCell::new());
                call_queries.insert(lang, OnceCell::new());
                if def.type_query.is_some() {
                    type_queries.insert(lang, OnceCell::new());
                }
            }
        }

        Ok(Self {
            queries,
            call_queries,
            type_queries,
        })
    }

    /// Get or compile the chunk extraction query for a language
    fn get_query(&self, language: Language) -> Result<&tree_sitter::Query, ParserError> {
        let cell = self.queries.get(&language).ok_or_else(|| {
            ParserError::QueryCompileFailed(language.to_string(), "not found".into())
        })?;

        cell.get_or_try_init(|| {
            let grammar = language.grammar();
            let pattern = language.query_pattern();
            tree_sitter::Query::new(&grammar, pattern).map_err(|e| {
                ParserError::QueryCompileFailed(language.to_string(), format!("{:?}", e))
            })
        })
    }

    /// Get or compile the call extraction query for a language
    pub(crate) fn get_call_query(
        &self,
        language: Language,
    ) -> Result<&tree_sitter::Query, ParserError> {
        let cell = self.call_queries.get(&language).ok_or_else(|| {
            ParserError::QueryCompileFailed(format!("{}_calls", language), "not found".into())
        })?;

        cell.get_or_try_init(|| {
            let grammar = language.grammar();
            let pattern = language.call_query_pattern();
            tree_sitter::Query::new(&grammar, pattern).map_err(|e| {
                ParserError::QueryCompileFailed(format!("{}_calls", language), format!("{:?}", e))
            })
        })
    }

    /// Get or compile the type extraction query for a language
    pub(crate) fn get_type_query(
        &self,
        language: Language,
    ) -> Result<&tree_sitter::Query, ParserError> {
        let cell = self.type_queries.get(&language).ok_or_else(|| {
            ParserError::QueryCompileFailed(format!("{}_types", language), "not found".into())
        })?;

        cell.get_or_try_init(|| {
            let grammar = language.grammar();
            let pattern = language.type_query_pattern();
            tree_sitter::Query::new(&grammar, pattern).map_err(|e| {
                ParserError::QueryCompileFailed(format!("{}_types", language), format!("{:?}", e))
            })
        })
    }

    /// Parse a source file and extract code chunks
    ///
    /// Returns an empty Vec for non-UTF8 files (with a warning logged).
    /// Returns an error for unsupported file types.
    pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>, ParserError> {
        let _span = tracing::info_span!("parse_file", path = %path.display()).entered();

        // Check file size to prevent OOM on huge files (limit: 50MB)
        const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_FILE_SIZE => {
                tracing::warn!(
                    "Skipping large file ({}MB > 50MB limit): {}",
                    meta.len() / (1024 * 1024),
                    path.display()
                );
                return Ok(vec![]);
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Gracefully handle non-UTF8 files
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!("Skipping non-UTF8 file: {}", path.display());
                return Ok(vec![]);
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistent hashing across platforms
        let source = source.replace("\r\n", "\n");

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let language = Language::from_extension(ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        // Grammar-less languages (Markdown) use custom parsers
        if language.def().grammar.is_none() {
            return crate::parser::markdown::parse_markdown_chunks(&source, path);
        }

        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{:?}", e)))?;

        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| ParserError::ParseFailed(path.display().to_string()))?;

        // Get or compile query (lazy initialization)
        let query = self.get_query(language)?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        let mut chunks = Vec::new();

        while let Some(m) = matches.next() {
            match self.extract_chunk(&source, m, query, language, path) {
                Ok(chunk) => {
                    // Skip chunks over 100 lines or 100KB
                    let lines = chunk.line_end - chunk.line_start;
                    const MAX_CHUNK_BYTES: usize = 100_000;
                    if lines > 100 {
                        tracing::debug!("Skipping {} ({} lines > 100 max)", chunk.id, lines);
                        continue;
                    }
                    if chunk.content.len() > MAX_CHUNK_BYTES {
                        tracing::debug!(
                            "Skipping {} ({} bytes > {} max)",
                            chunk.id,
                            chunk.content.len(),
                            MAX_CHUNK_BYTES
                        );
                        continue;
                    }
                    chunks.push(chunk);
                }
                Err(e) => {
                    tracing::warn!("Failed to extract chunk from {}: {}", path.display(), e);
                }
            }
        }

        Ok(chunks)
    }

    pub fn supported_extensions(&self) -> Vec<&'static str> {
        crate::language::REGISTRY.supported_extensions().collect()
    }
}
