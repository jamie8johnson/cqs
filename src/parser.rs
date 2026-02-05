//! Code parsing with tree-sitter

use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;
use tree_sitter::StreamingIterator;

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
}

impl Parser {
    /// Create a new parser (queries are compiled lazily on first use)
    pub fn new() -> Result<Self, ParserError> {
        let mut queries = HashMap::new();
        let mut call_queries = HashMap::new();

        // Initialize empty OnceCells for each registered language
        for def in crate::language::REGISTRY.all() {
            let lang: Language = def.name.parse().expect("registry/enum mismatch");
            queries.insert(lang, OnceCell::new());
            call_queries.insert(lang, OnceCell::new());
        }

        Ok(Self {
            queries,
            call_queries,
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
    fn get_call_query(&self, language: Language) -> Result<&tree_sitter::Query, ParserError> {
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
                    // Large functions are too noisy for semantic search (diluted embeddings)
                    // and are covered by the full call graph for caller/callee queries
                    let lines = chunk.line_end - chunk.line_start;
                    const MAX_CHUNK_BYTES: usize = 100_000; // 100KB handles minified code
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

    fn extract_chunk(
        &self,
        source: &str,
        m: &tree_sitter::QueryMatch<'_, '_>,
        query: &tree_sitter::Query,
        language: Language,
        path: &Path,
    ) -> Result<Chunk, ParserError> {
        // Map capture names to chunk types
        let capture_types: &[(&str, ChunkType)] = &[
            ("function", ChunkType::Function),
            ("struct", ChunkType::Struct),
            ("class", ChunkType::Class),
            ("enum", ChunkType::Enum),
            ("trait", ChunkType::Trait),
            ("interface", ChunkType::Interface),
            ("const", ChunkType::Constant),
        ];

        // Find which definition capture matched and get its node
        let (node, base_chunk_type) = capture_types
            .iter()
            .find_map(|(name, chunk_type)| {
                query
                    .capture_index_for_name(name)
                    .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                    .map(|c| (c.node, *chunk_type))
            })
            .ok_or_else(|| {
                ParserError::ParseFailed("No definition capture found in match".into())
            })?;

        // Get name capture
        let name_idx = query.capture_index_for_name("name");
        let name = name_idx
            .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
            .map(|c| source[c.node.byte_range()].to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());

        // Extract content
        let content = source[node.byte_range()].to_string();

        // Line numbers (1-indexed for display)
        let line_start = node.start_position().row as u32 + 1;
        let line_end = node.end_position().row as u32 + 1;

        // Extract signature
        let signature = self.extract_signature(&content, language);

        // Extract doc comments
        let doc = self.extract_doc_comment(node, source, language);

        // Determine chunk type - only infer for functions (to detect methods)
        let chunk_type = if base_chunk_type == ChunkType::Function {
            self.infer_chunk_type(node, language)
        } else {
            base_chunk_type
        };

        // Content hash for deduplication (BLAKE3 produces 64 hex chars)
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let hash_prefix = content_hash.get(..8).unwrap_or(&content_hash);
        let id = format!("{}:{}:{}", path.display(), line_start, hash_prefix);

        Ok(Chunk {
            id,
            file: path.to_path_buf(),
            language,
            chunk_type,
            name,
            signature,
            content,
            doc,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
            window_idx: None,
        })
    }

    fn extract_signature(&self, content: &str, language: Language) -> String {
        let sig_end = match language.def().signature_style {
            SignatureStyle::UntilBrace => content.find('{').unwrap_or(content.len()),
            SignatureStyle::UntilColon => content.find(':').unwrap_or(content.len()),
        };
        let sig = &content[..sig_end];
        // Normalize whitespace
        sig.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn extract_doc_comment(
        &self,
        node: tree_sitter::Node,
        source: &str,
        language: Language,
    ) -> Option<String> {
        let doc_nodes = language.def().doc_nodes;

        // Walk backwards through siblings looking for comments
        let mut comments = Vec::new();
        let mut current = node.prev_sibling();

        while let Some(sibling) = current {
            let kind = sibling.kind();

            if doc_nodes.contains(&kind) {
                let text = &source[sibling.byte_range()];
                comments.push(text.to_string());
                current = sibling.prev_sibling();
            } else if kind.contains("comment") {
                // Keep looking past non-doc comments
                current = sibling.prev_sibling();
            } else {
                break;
            }
        }

        if comments.is_empty() {
            // For Python, also check for docstring as first statement in body
            if language == Language::Python {
                if let Some(body) = node.child_by_field_name("body") {
                    if let Some(first) = body.named_child(0) {
                        if first.kind() == "expression_statement" {
                            if let Some(string) = first.named_child(0) {
                                if string.kind() == "string" {
                                    return Some(source[string.byte_range()].to_string());
                                }
                            }
                        }
                    }
                }
            }
            return None;
        }

        comments.reverse();
        Some(comments.join("\n"))
    }

    fn infer_chunk_type(&self, node: tree_sitter::Node, language: Language) -> ChunkType {
        let def = language.def();

        // Check if the node itself is a method kind (e.g., Go's "method_declaration")
        if def.method_node_kinds.contains(&node.kind()) {
            return ChunkType::Method;
        }

        // Walk parents looking for method containers (e.g., impl blocks, class bodies)
        let mut current = node.parent();
        while let Some(parent) = current {
            if def.method_containers.contains(&parent.kind()) {
                return ChunkType::Method;
            }
            current = parent.parent();
        }

        ChunkType::Function
    }

    pub fn supported_extensions(&self) -> Vec<&'static str> {
        crate::language::REGISTRY.supported_extensions().collect()
    }

    /// Extract function calls from a chunk's source code
    ///
    /// Returns call sites found within the given byte range of the source.
    pub fn extract_calls(
        &self,
        source: &str,
        language: Language,
        start_byte: usize,
        end_byte: usize,
        line_offset: u32,
    ) -> Vec<CallSite> {
        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&grammar).is_err() {
            return vec![];
        }

        let tree = match parser.parse(source, None) {
            Some(t) => t,
            None => return vec![],
        };

        let query = match self.get_call_query(language) {
            Ok(q) => q,
            Err(_) => return vec![],
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        // Only match within the chunk's byte range
        cursor.set_byte_range(start_byte..end_byte);

        let mut calls = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee_name = source[cap.node.byte_range()].to_string();
                // saturating_sub prevents underflow if line_offset > position
                // .max(1) ensures we never produce line 0 (line numbers are 1-indexed)
                let line_number = (cap.node.start_position().row as u32 + 1)
                    .saturating_sub(line_offset)
                    .max(1);

                // Skip common noise (self, this, super, etc.)
                if !should_skip_callee(&callee_name) {
                    calls.push(CallSite {
                        callee_name,
                        line_number,
                    });
                }
            }
        }

        // Deduplicate calls to the same function (keep first occurrence)
        let mut seen = std::collections::HashSet::new();
        calls.retain(|c| seen.insert(c.callee_name.clone()));

        calls
    }

    /// Extract function calls from a parsed chunk
    ///
    /// Convenience method that extracts calls from the chunk's content.
    pub fn extract_calls_from_chunk(&self, chunk: &Chunk) -> Vec<CallSite> {
        self.extract_calls(
            &chunk.content,
            chunk.language,
            0,
            chunk.content.len(),
            0, // No line offset since we're parsing the content directly
        )
    }

    /// Extract all function calls from a file, ignoring size limits
    ///
    /// Returns calls for every function in the file, including those >100 lines
    /// that would normally be skipped during chunk extraction.
    pub fn parse_file_calls(&self, path: &Path) -> Result<Vec<FunctionCalls>, ParserError> {
        // Read file
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(vec![]);
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistency
        let source = source.replace("\r\n", "\n");

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = Language::from_extension(ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{:?}", e)))?;

        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| ParserError::ParseFailed(path.display().to_string()))?;

        // Get or compile queries (lazy initialization)
        let chunk_query = self.get_query(language)?;
        let call_query = self.get_call_query(language)?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());

        let mut results = Vec::new();
        // Reuse these allocations across iterations
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let capture_names = chunk_query.capture_names();

        while let Some(m) = matches.next() {
            // Find function node
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                matches!(
                    name,
                    "function" | "struct" | "class" | "enum" | "trait" | "interface" | "const"
                )
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            // Get function name
            let name_idx = chunk_query.capture_index_for_name("name");
            let name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            let line_start = node.start_position().row as u32 + 1;

            // Extract calls within this function (no size limit!)
            call_cursor.set_byte_range(node.byte_range());
            calls.clear();

            let mut call_matches =
                call_cursor.matches(call_query, tree.root_node(), source.as_bytes());

            while let Some(cm) = call_matches.next() {
                for cap in cm.captures {
                    let callee_name = source[cap.node.byte_range()].to_string();
                    let call_line = cap.node.start_position().row as u32 + 1;

                    if !should_skip_callee(&callee_name) {
                        calls.push(CallSite {
                            callee_name,
                            line_number: call_line,
                        });
                    }
                }
            }

            // Deduplicate
            seen.clear();
            calls.retain(|c| seen.insert(c.callee_name.clone()));

            if !calls.is_empty() {
                results.push(FunctionCalls {
                    name,
                    line_start,
                    calls: std::mem::take(&mut calls),
                });
            }
        }

        Ok(results)
    }
}

// Note: Default impl intentionally omitted to prevent hidden panics.
// Use Parser::new() which returns Result for proper error handling.

/// Check if a callee name should be skipped (common noise)
///
/// These are filtered because they don't provide meaningful call graph information:
/// - `self`, `this`, `Self`, `super`: Object references, not real function calls
/// - `new`: Constructor pattern, not a named function
/// - `toString`, `valueOf`: Ubiquitous JS/TS methods that add noise
///
/// Case-sensitive to avoid false positives (e.g., "This" as a variable name).
fn should_skip_callee(name: &str) -> bool {
    matches!(
        name,
        "self" | "this" | "super" | "Self" | "new" | "toString" | "valueOf"
    )
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

/// A parsed code chunk (function, method, class, etc.)
///
/// Chunks are the basic unit of indexing and search in cqs.
/// Each chunk represents a single code element extracted by tree-sitter.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Unique identifier: `{file}:{line_start}:{content_hash}` or `{parent_id}:w{window_idx}`
    pub id: String,
    /// Source file path (typically absolute during indexing, stored as provided)
    pub file: std::path::PathBuf,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Test should_skip_callee filtering
    mod skip_callee_tests {
        use super::*;

        #[test]
        fn test_skips_self_variants() {
            assert!(should_skip_callee("self"));
            assert!(should_skip_callee("Self"));
            assert!(should_skip_callee("this"));
            assert!(should_skip_callee("super"));
        }

        #[test]
        fn test_skips_common_noise() {
            assert!(should_skip_callee("new"));
            assert!(should_skip_callee("toString"));
            assert!(should_skip_callee("valueOf"));
        }

        #[test]
        fn test_allows_normal_functions() {
            assert!(!should_skip_callee("process"));
            assert!(!should_skip_callee("calculate"));
            assert!(!should_skip_callee("Self_")); // Not exact match
            assert!(!should_skip_callee("myself"));
            assert!(!should_skip_callee("newValue"));
        }

        #[test]
        fn test_case_sensitive() {
            // These are exact matches only
            assert!(!should_skip_callee("SELF"));
            assert!(!should_skip_callee("This"));
            assert!(!should_skip_callee("NEW"));
        }
    }

    /// Test signature extraction
    mod signature_tests {
        use super::*;

        fn parser() -> Parser {
            Parser::new().unwrap()
        }

        #[test]
        fn test_rust_signature_stops_at_brace() {
            let p = parser();
            let content = "fn process(x: i32) -> Result<(), Error> {\n    body\n}";
            let sig = p.extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn process(x: i32) -> Result<(), Error>");
        }

        #[test]
        fn test_rust_signature_normalizes_whitespace() {
            let p = parser();
            let content = "fn   process(  x: i32  )   -> i32 {";
            let sig = p.extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn process( x: i32 ) -> i32");
        }

        #[test]
        fn test_python_signature_stops_at_colon() {
            let p = parser();
            let content = "def calculate(x, y):\n    return x + y";
            let sig = p.extract_signature(content, Language::Python);
            assert_eq!(sig, "def calculate(x, y)");
        }

        #[test]
        fn test_go_signature_stops_at_brace() {
            let p = parser();
            let content = "func (s *Server) Handle(r Request) error {\n\treturn nil\n}";
            let sig = p.extract_signature(content, Language::Go);
            assert_eq!(sig, "func (s *Server) Handle(r Request) error");
        }

        #[test]
        fn test_typescript_signature_stops_at_brace() {
            let p = parser();
            let content = "function processData(input: string): Promise<Result> {\n  return ok;\n}";
            let sig = p.extract_signature(content, Language::TypeScript);
            assert_eq!(sig, "function processData(input: string): Promise<Result>");
        }

        #[test]
        fn test_signature_without_terminator() {
            let p = parser();
            // No brace - returns whole content normalized
            let content = "fn abstract_decl()";
            let sig = p.extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn abstract_decl()");
        }
    }

    /// Test chunk parsing integration
    mod parse_tests {
        use super::*;

        fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
            let mut file = tempfile::Builder::new()
                .suffix(&format!(".{}", ext))
                .tempfile()
                .unwrap();
            file.write_all(content.as_bytes()).unwrap();
            file.flush().unwrap();
            file
        }

        #[test]
        fn test_parse_rust_function() {
            let content = r#"
/// Adds two numbers
fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "add");
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0].doc.as_ref().unwrap().contains("Adds two numbers"));
        }

        #[test]
        fn test_parse_rust_method_in_impl() {
            let content = r#"
struct Counter { value: i32 }

impl Counter {
    fn increment(&mut self) {
        self.value += 1;
    }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            // Should have struct and method
            let method = chunks.iter().find(|c| c.name == "increment").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_python_class_method() {
            let content = r#"
class Calculator:
    """A simple calculator."""

    def add(self, a, b):
        """Add two numbers."""
        return a + b
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_go_method_vs_function() {
            let content = r#"
package main

func standalone() {
    println("standalone")
}

func (s *Server) method() {
    println("method")
}
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let standalone = chunks.iter().find(|c| c.name == "standalone").unwrap();
            assert_eq!(standalone.chunk_type, ChunkType::Function);

            let method = chunks.iter().find(|c| c.name == "method").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_typescript_interface() {
            let content = r#"
interface User {
    name: string;
    age: number;
}
"#;
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "User");
            assert_eq!(chunks[0].chunk_type, ChunkType::Interface);
        }

        #[test]
        fn test_parse_c_function() {
            let content = r#"
/* Adds two integers */
int add(int a, int b) {
    return a + b;
}
"#;
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "add");
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0]
                .doc
                .as_ref()
                .unwrap()
                .contains("Adds two integers"));
        }

        #[test]
        fn test_parse_c_struct_and_enum() {
            let content = r#"
struct Point {
    int x;
    int y;
};

enum Color {
    RED,
    GREEN,
    BLUE
};
"#;
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let point = chunks.iter().find(|c| c.name == "Point").unwrap();
            assert_eq!(point.chunk_type, ChunkType::Struct);

            let color = chunks.iter().find(|c| c.name == "Color").unwrap();
            assert_eq!(color.chunk_type, ChunkType::Enum);
        }

        #[test]
        fn test_parse_java_class_with_method() {
            let content = r#"
public class Calculator {
    /**
     * Adds two numbers
     */
    public int add(int a, int b) {
        return a + b;
    }
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert!(method.doc.as_ref().unwrap().contains("Adds two numbers"));
        }

        #[test]
        fn test_parse_java_interface_and_enum() {
            let content = r#"
interface Printable {
    void print();
}

enum Direction {
    NORTH,
    SOUTH,
    EAST,
    WEST
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
            assert_eq!(iface.chunk_type, ChunkType::Interface);

            let dir = chunks.iter().find(|c| c.name == "Direction").unwrap();
            assert_eq!(dir.chunk_type, ChunkType::Enum);
        }
    }

    /// Test call extraction
    mod call_extraction_tests {
        use super::*;

        fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
            let mut file = tempfile::Builder::new()
                .suffix(&format!(".{}", ext))
                .tempfile()
                .unwrap();
            file.write_all(content.as_bytes()).unwrap();
            file.flush().unwrap();
            file
        }

        #[test]
        fn test_extract_rust_calls() {
            let content = r#"
fn caller() {
    helper();
    other.method();
    Module::function();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(names.contains(&"helper"));
            assert!(names.contains(&"method"));
            assert!(names.contains(&"function"));
        }

        #[test]
        fn test_extract_python_calls() {
            let content = r#"
def caller():
    helper()
    obj.method()
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            assert!(names.contains(&"helper"));
            assert!(names.contains(&"method"));
        }

        #[test]
        fn test_skips_self_calls() {
            let content = r#"
fn example() {
    self.method();
    this.other();
    real_function();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let calls = parser.extract_calls_from_chunk(&chunks[0]);

            let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            // self/this should be filtered, but method/other should remain
            assert!(!names.contains(&"self"));
            assert!(!names.contains(&"this"));
            assert!(names.contains(&"method"));
            assert!(names.contains(&"other"));
            assert!(names.contains(&"real_function"));
        }

        #[test]
        fn test_parse_file_calls() {
            let content = r#"
fn caller() {
    helper();
    other_func();
}

fn another() {
    third();
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let function_calls = parser.parse_file_calls(file.path()).unwrap();

            // Should return calls for both functions
            assert_eq!(function_calls.len(), 2);

            // First function
            let caller = function_calls
                .iter()
                .find(|fc| fc.name == "caller")
                .unwrap();
            let caller_names: Vec<_> = caller
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(caller_names.contains(&"helper"));
            assert!(caller_names.contains(&"other_func"));

            // Second function
            let another = function_calls
                .iter()
                .find(|fc| fc.name == "another")
                .unwrap();
            let another_names: Vec<_> = another
                .calls
                .iter()
                .map(|c| c.callee_name.as_str())
                .collect();
            assert!(another_names.contains(&"third"));
        }

        #[test]
        fn test_parse_file_calls_unsupported_extension() {
            let file = write_temp_file("not code", "txt");
            let parser = Parser::new().unwrap();
            let result = parser.parse_file_calls(file.path());
            assert!(result.is_err());
        }

        #[test]
        fn test_parse_file_calls_empty_file() {
            let file = write_temp_file("", "rs");
            let parser = Parser::new().unwrap();
            let function_calls = parser.parse_file_calls(file.path()).unwrap();
            assert!(function_calls.is_empty());
        }
    }
}
