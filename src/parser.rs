//! Code parsing with tree-sitter

use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;
use tree_sitter::StreamingIterator;

#[derive(Error, Debug)]
pub enum ParserError {
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),
    #[error("Failed to parse: {0}")]
    ParseFailed(String),
    #[error("Failed to compile query for {0}: {1}")]
    QueryCompileFailed(String, String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// Tree-sitter query patterns per language

/// Rust: functions, structs, enums, traits, constants
const RUST_QUERY: &str = r#"
(function_item
  name: (identifier) @name) @function

(struct_item
  name: (type_identifier) @name) @struct

(enum_item
  name: (type_identifier) @name) @enum

(trait_item
  name: (type_identifier) @name) @trait

(const_item
  name: (identifier) @name) @const

(static_item
  name: (identifier) @name) @const
"#;

/// Python: functions and classes
const PYTHON_QUERY: &str = r#"
(function_definition
  name: (identifier) @name) @function

(class_definition
  name: (identifier) @name) @class
"#;

/// TypeScript: functions, methods, arrow functions, classes, interfaces, enums
const TYPESCRIPT_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_definition
  name: (property_identifier) @name) @function

;; Arrow function assigned to variable: const foo = () => {}
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

;; Arrow function assigned with var/let
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

(class_declaration
  name: (type_identifier) @name) @class

(interface_declaration
  name: (type_identifier) @name) @interface

(enum_declaration
  name: (identifier) @name) @enum
"#;

/// JavaScript: functions, methods, arrow functions, classes
const JAVASCRIPT_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_definition
  name: (property_identifier) @name) @function

;; Arrow function assigned to variable: const foo = () => {}
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

;; Arrow function assigned with var/let
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

(class_declaration
  name: (identifier) @name) @class
"#;

/// Go: functions, methods, structs, interfaces, constants
const GO_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_declaration
  name: (field_identifier) @name) @function

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type))) @struct

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type))) @interface

(const_declaration
  (const_spec
    name: (identifier) @name)) @const
"#;

// Call extraction queries per language

/// Rust: function calls, method calls, macro invocations
const RUST_CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))

(call_expression
  function: (scoped_identifier
    name: (identifier) @callee))

(macro_invocation
  macro: (identifier) @callee)
"#;

/// Python: function and method calls
const PYTHON_CALL_QUERY: &str = r#"
(call
  function: (identifier) @callee)

(call
  function: (attribute
    attribute: (identifier) @callee))
"#;

/// TypeScript/JavaScript: function and method calls
const TS_JS_CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (member_expression
    property: (property_identifier) @callee))
"#;

/// Go: function and method calls
const GO_CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (selector_expression
    field: (field_identifier) @callee))
"#;

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
    /// Cached compiled queries per language (Query compilation is ~1ms, worth caching)
    queries: HashMap<Language, tree_sitter::Query>,
    /// Cached call extraction queries per language
    call_queries: HashMap<Language, tree_sitter::Query>,
}

impl Parser {
    /// Create a new parser with pre-compiled queries for all supported languages
    pub fn new() -> Result<Self, ParserError> {
        let mut queries = HashMap::new();
        let mut call_queries = HashMap::new();

        for lang in [
            Language::Rust,
            Language::Python,
            Language::TypeScript,
            Language::JavaScript,
            Language::Go,
        ] {
            let grammar = lang.grammar();

            // Chunk extraction queries
            let pattern = lang.query_pattern();
            let query = tree_sitter::Query::new(&grammar, pattern).map_err(|e| {
                ParserError::QueryCompileFailed(lang.to_string(), format!("{:?}", e))
            })?;
            queries.insert(lang, query);

            // Call extraction queries
            let call_pattern = lang.call_query_pattern();
            let call_query = tree_sitter::Query::new(&grammar, call_pattern).map_err(|e| {
                ParserError::QueryCompileFailed(format!("{}_calls", lang), format!("{:?}", e))
            })?;
            call_queries.insert(lang, call_query);
        }

        Ok(Self {
            queries,
            call_queries,
        })
    }

    /// Parse a source file and extract code chunks
    ///
    /// Returns an empty Vec for non-UTF8 files (with a warning logged).
    /// Returns an error for unsupported file types.
    pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>, ParserError> {
        // Gracefully handle non-UTF8 files
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!("Skipping non-UTF8 file: {}", path.display());
                return Ok(vec![]);
            }
            Err(e) => return Err(e.into()),
        };

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

        // Use cached query
        let query = self.queries.get(&language).ok_or_else(|| {
            ParserError::QueryCompileFailed(language.to_string(), "not found".into())
        })?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        let mut chunks = Vec::new();

        while let Some(m) = matches.next() {
            match self.extract_chunk(&source, m, query, language, path) {
                Ok(chunk) => {
                    // Skip chunks over 100 lines or 100KB (handles minified files)
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

        // Content hash for deduplication
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let id = format!("{}:{}:{}", path.display(), line_start, &content_hash[..8]);

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
        // Extract up to first { or : (language dependent)
        let sig_end = match language {
            Language::Rust | Language::Go | Language::TypeScript | Language::JavaScript => {
                content.find('{').unwrap_or(content.len())
            }
            Language::Python => content.find(':').unwrap_or(content.len()),
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
        // Walk backwards through siblings looking for comments
        let mut comments = Vec::new();
        let mut current = node.prev_sibling();

        while let Some(sibling) = current {
            let kind = sibling.kind();

            let is_doc = match language {
                Language::Rust => kind == "line_comment" || kind == "block_comment",
                Language::Python => kind == "string" || kind == "comment",
                Language::TypeScript | Language::JavaScript => kind == "comment",
                Language::Go => kind == "comment",
            };

            if is_doc {
                let text = &source[sibling.byte_range()];
                comments.push(text.to_string());
                current = sibling.prev_sibling();
            } else if kind.contains("comment") {
                // Keep looking
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
        // For Go, the node type itself determines function vs method
        if language == Language::Go {
            return if node.kind() == "method_declaration" {
                ChunkType::Method
            } else {
                ChunkType::Function
            };
        }

        // For other languages, check if function is inside a class/impl/struct body
        let mut current = node.parent();
        while let Some(parent) = current {
            let kind = parent.kind();
            let is_method_container = match language {
                Language::Rust => kind == "impl_item" || kind == "trait_item",
                Language::Python => kind == "class_definition",
                Language::TypeScript | Language::JavaScript => {
                    kind == "class_body" || kind == "class_declaration"
                }
                Language::Go => unreachable!(),
            };
            if is_method_container {
                return ChunkType::Method;
            }
            current = parent.parent();
        }
        ChunkType::Function
    }

    pub fn supported_extensions(&self) -> &[&str] {
        &[
            "rs", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go",
        ]
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

        let query = match self.call_queries.get(&language) {
            Some(q) => q,
            None => return vec![],
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        // Only match within the chunk's byte range
        cursor.set_byte_range(start_byte..end_byte);

        let mut calls = Vec::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee_name = source[cap.node.byte_range()].to_string();
                let line_number = cap.node.start_position().row as u32 + 1 - line_offset + 1;

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
}

/// Check if a callee name should be skipped (common noise)
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

impl Parser {
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

        // Get chunk query to find all functions
        let chunk_query = self.queries.get(&language).ok_or_else(|| {
            ParserError::QueryCompileFailed(language.to_string(), "not found".into())
        })?;

        // Get call query
        let call_query = self.call_queries.get(&language).ok_or_else(|| {
            ParserError::QueryCompileFailed(format!("{}_calls", language), "not found".into())
        })?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());

        let mut results = Vec::new();

        while let Some(m) = matches.next() {
            // Find function node
            let func_node = m.captures.iter().find(|c| {
                let name = chunk_query.capture_names()[c.index as usize];
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
            let mut call_cursor = tree_sitter::QueryCursor::new();
            call_cursor.set_byte_range(node.byte_range());

            let mut calls = Vec::new();
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
            let mut seen = std::collections::HashSet::new();
            calls.retain(|c| seen.insert(c.callee_name.clone()));

            if !calls.is_empty() {
                results.push(FunctionCalls {
                    name,
                    line_start,
                    calls,
                });
            }
        }

        Ok(results)
    }
}

// Note: Default impl intentionally omitted to prevent hidden panics.
// Use Parser::new() which returns Result for proper error handling.

/// A parsed code chunk (function, method, class, etc.)
///
/// Chunks are the basic unit of indexing and search in cqs.
/// Each chunk represents a single code element extracted by tree-sitter.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Unique identifier: `{file}:{line_start}:{content_hash}` or `{parent_id}:w{window_idx}`
    pub id: String,
    /// Source file path (relative to project root)
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

/// Supported programming languages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "py" | "pyi" => Some(Language::Python),
            "ts" | "tsx" => Some(Language::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "go" => Some(Language::Go),
            _ => None,
        }
    }

    pub fn grammar(&self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    pub fn query_pattern(&self) -> &'static str {
        match self {
            Language::Rust => RUST_QUERY,
            Language::Python => PYTHON_QUERY,
            Language::TypeScript => TYPESCRIPT_QUERY,
            Language::JavaScript => JAVASCRIPT_QUERY,
            Language::Go => GO_QUERY,
        }
    }

    pub fn call_query_pattern(&self) -> &'static str {
        match self {
            Language::Rust => RUST_CALL_QUERY,
            Language::Python => PYTHON_CALL_QUERY,
            Language::TypeScript | Language::JavaScript => TS_JS_CALL_QUERY,
            Language::Go => GO_CALL_QUERY,
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
            Language::TypeScript => write!(f, "typescript"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Go => write!(f, "go"),
        }
    }
}

impl std::str::FromStr for Language {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rust" => Ok(Language::Rust),
            "python" => Ok(Language::Python),
            "typescript" => Ok(Language::TypeScript),
            "javascript" => Ok(Language::JavaScript),
            "go" => Ok(Language::Go),
            _ => anyhow::bail!(
                "Unknown language: '{}'. Valid options: rust, python, typescript, javascript, go",
                s
            ),
        }
    }
}

/// Type of code element extracted by the parser
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkType {
    /// Standalone function
    Function,
    /// Method (function inside a class/struct/impl)
    Method,
    /// Class definition (Python, TypeScript, JavaScript)
    Class,
    /// Struct definition (Rust, Go)
    Struct,
    /// Enum definition
    Enum,
    /// Trait definition (Rust)
    Trait,
    /// Interface definition (TypeScript, Go)
    Interface,
    /// Constant or static variable
    Constant,
}

impl std::fmt::Display for ChunkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkType::Function => write!(f, "function"),
            ChunkType::Method => write!(f, "method"),
            ChunkType::Class => write!(f, "class"),
            ChunkType::Struct => write!(f, "struct"),
            ChunkType::Enum => write!(f, "enum"),
            ChunkType::Trait => write!(f, "trait"),
            ChunkType::Interface => write!(f, "interface"),
            ChunkType::Constant => write!(f, "constant"),
        }
    }
}

impl std::str::FromStr for ChunkType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "function" => Ok(ChunkType::Function),
            "method" => Ok(ChunkType::Method),
            "class" => Ok(ChunkType::Class),
            "struct" => Ok(ChunkType::Struct),
            "enum" => Ok(ChunkType::Enum),
            "trait" => Ok(ChunkType::Trait),
            "interface" => Ok(ChunkType::Interface),
            "constant" => Ok(ChunkType::Constant),
            _ => anyhow::bail!(
                "Unknown chunk type: '{}'. Valid options: function, method, class, struct, enum, trait, interface, constant",
                s
            ),
        }
    }
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
    }

    /// Test language detection
    mod language_tests {
        use super::*;

        #[test]
        fn test_from_extension() {
            assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
            assert_eq!(Language::from_extension("py"), Some(Language::Python));
            assert_eq!(Language::from_extension("pyi"), Some(Language::Python));
            assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
            assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
            assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
            assert_eq!(Language::from_extension("jsx"), Some(Language::JavaScript));
            assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
            assert_eq!(Language::from_extension("cjs"), Some(Language::JavaScript));
            assert_eq!(Language::from_extension("go"), Some(Language::Go));
            assert_eq!(Language::from_extension("unknown"), None);
        }

        #[test]
        fn test_from_str() {
            assert_eq!("rust".parse::<Language>().unwrap(), Language::Rust);
            assert_eq!("PYTHON".parse::<Language>().unwrap(), Language::Python);
            assert_eq!(
                "TypeScript".parse::<Language>().unwrap(),
                Language::TypeScript
            );
            assert!("invalid".parse::<Language>().is_err());
        }

        #[test]
        fn test_display() {
            assert_eq!(Language::Rust.to_string(), "rust");
            assert_eq!(Language::Python.to_string(), "python");
            assert_eq!(Language::TypeScript.to_string(), "typescript");
            assert_eq!(Language::JavaScript.to_string(), "javascript");
            assert_eq!(Language::Go.to_string(), "go");
        }
    }
}
