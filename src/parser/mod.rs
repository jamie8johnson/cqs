//! Code parsing with tree-sitter
//!
//! Split into submodules:
//! - `types` — data structures and error types
//! - `chunk` — chunk extraction from parse trees
//! - `calls` — call site extraction for call graph
//! - `injection` — multi-grammar injection (HTML→JS/CSS via `set_included_ranges()`)
//! - `markdown` — heading-based Markdown parser with cross-reference extraction

mod calls;
mod chunk;
pub(crate) mod injection;
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

/// Maximum file size for parsing (50 MB). Files larger than this are skipped.
pub(crate) const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Combined parse result: chunks, function calls, and type references.
///
/// Returned by `parse_file_all()` and `parse_injected_all()` which extract
/// everything in a single file read + tree-sitter parse.
pub type ParseAllResult = (Vec<Chunk>, Vec<FunctionCalls>, Vec<ChunkTypeRefs>);

/// Maximum chunk content size (100 KB). Larger chunks are skipped.
pub(crate) const MAX_CHUNK_BYTES: usize = 100_000;

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
            let lang: Language = def.name.parse().unwrap_or_else(|_| {
                panic!(
                    "Language registry/enum mismatch: '{}' is registered but has no Language variant",
                    def.name
                )
            });
            // Grammar-less languages must not define injections (they'd silently produce nothing)
            debug_assert!(
                def.grammar.is_some() || def.injections.is_empty(),
                "Language '{}' has no grammar but defines injections — injections require tree-sitter",
                def.name
            );
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
                ParserError::QueryCompileFailed(language.to_string(), format!("{}", e))
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
                ParserError::QueryCompileFailed(format!("{}_calls", language), format!("{}", e))
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
                ParserError::QueryCompileFailed(format!("{}_types", language), format!("{}", e))
            })
        })
    }

    /// Parse a source file and extract code chunks
    ///
    /// Returns an empty Vec for non-UTF8 files (with a warning logged).
    /// Returns an error for unsupported file types.
    pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>, ParserError> {
        let _span = tracing::info_span!("parse_file", path = %path.display()).entered();

        // Check file size to prevent OOM on huge files
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

        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();

        let language = Language::from_extension(&ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        // Grammar-less languages (Markdown) use custom parsers
        if language.def().grammar.is_none() {
            return crate::parser::markdown::parse_markdown_chunks(&source, path);
        }

        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{}", e)))?;

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
                Ok(mut chunk) => {
                    // Skip chunks over 100KB (large functions are handled by windowing in the pipeline)
                    if chunk.content.len() > MAX_CHUNK_BYTES {
                        tracing::debug!(
                            "Skipping {} ({} bytes > {} max)",
                            chunk.id,
                            chunk.content.len(),
                            MAX_CHUNK_BYTES
                        );
                        continue;
                    }
                    // Apply post-process hook (e.g., HCL block reclassification)
                    if let Some(post_process) = language.def().post_process_chunk {
                        if let Some(node) = extract_definition_node(m, query) {
                            if !post_process(&mut chunk.name, &mut chunk.chunk_type, node, &source)
                            {
                                tracing::debug!(
                                    name = %chunk.name,
                                    file = %path.display(),
                                    "post_process_chunk: discarded"
                                );
                                continue;
                            }
                        }
                    }
                    chunks.push(chunk);
                }
                Err(e) => {
                    tracing::warn!("Failed to extract chunk from {}: {}", path.display(), e);
                }
            }
        }

        // --- Phase 2: Injection parsing (multi-grammar) ---
        let injections = language.def().injections;
        if !injections.is_empty() {
            // Release borrows on the outer tree before injection phase
            drop(matches);
            drop(cursor);

            let groups = injection::find_injection_ranges(&tree, &source, injections);

            // Free outer tree/parser memory before inner parse allocations
            drop(tree);
            drop(parser);

            for group in &groups {
                match self.parse_injected_chunks(&source, path, group, 0) {
                    Ok(inner_chunks) if !inner_chunks.is_empty() => {
                        let before = chunks.len();
                        // Remove outer chunks that overlap with injection containers
                        chunks.retain(|c| {
                            !injection::chunk_within_container(
                                c.line_start,
                                c.line_end,
                                &group.container_lines,
                            )
                        });
                        let removed = before - chunks.len();
                        tracing::debug!(
                            language = %group.language,
                            removed,
                            added = inner_chunks.len(),
                            "Replaced outer chunks with injection results"
                        );
                        chunks.extend(inner_chunks);
                    }
                    Ok(_) => {
                        // Zero inner chunks — keep outer chunks as-is (fallback)
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            language = %group.language,
                            "Injection parsing failed, keeping outer chunks"
                        );
                    }
                }
            }
        }

        Ok(chunks)
    }

    /// Parse a source file and extract chunks, calls, AND type references in one pass.
    ///
    /// Combines `parse_file()` and `parse_file_relationships()` to avoid double
    /// file read + double tree-sitter parse. Single file read, single outer parse,
    /// two query cursor passes on the same tree, single injection parse.
    ///
    /// Returns `(chunks, function_calls, chunk_type_refs)`.
    ///
    /// Used by `watch::reindex_files()` where the same files need both chunk
    /// extraction and relationship extraction. The streaming pipeline
    /// (pipeline.rs → index.rs) still uses separate calls because its
    /// parse → embed → store → extract architecture requires chunks to be
    /// stored before relationships reference them.
    pub fn parse_file_all(&self, path: &Path) -> Result<ParseAllResult, ParserError> {
        let _span = tracing::info_span!("parse_file_all", path = %path.display()).entered();

        // Check file size to prevent OOM on huge files
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_FILE_SIZE => {
                tracing::warn!(
                    "Skipping large file ({}MB > 50MB limit): {}",
                    meta.len() / (1024 * 1024),
                    path.display()
                );
                return Ok((vec![], vec![], vec![]));
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Read file once
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!("Skipping non-UTF8 file: {}", path.display());
                return Ok((vec![], vec![], vec![]));
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistent hashing across platforms
        let source = source.replace("\r\n", "\n");

        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();

        let language = Language::from_extension(&ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        // Grammar-less languages (Markdown) use custom parsers
        if language.def().grammar.is_none() {
            let chunks = crate::parser::markdown::parse_markdown_chunks(&source, path)?;
            let calls = crate::parser::markdown::parse_markdown_references(&source, path)?;
            return Ok((chunks, calls, vec![]));
        }

        // Single tree-sitter parse
        let grammar = language.grammar();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{}", e)))?;

        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| ParserError::ParseFailed(path.display().to_string()))?;

        // Get queries (chunk query needed for both passes, call/type for pass 2)
        let chunk_query = self.get_query(language)?;
        let call_query = self.get_call_query(language)?;

        // --- Pass 1: Chunk extraction ---
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());
        let mut chunks = Vec::new();

        while let Some(m) = matches.next() {
            match self.extract_chunk(&source, m, chunk_query, language, path) {
                Ok(mut chunk) => {
                    if chunk.content.len() > MAX_CHUNK_BYTES {
                        tracing::debug!(
                            "Skipping {} ({} bytes > {} max)",
                            chunk.id,
                            chunk.content.len(),
                            MAX_CHUNK_BYTES
                        );
                        continue;
                    }
                    if let Some(post_process) = language.def().post_process_chunk {
                        if let Some(node) = extract_definition_node(m, chunk_query) {
                            if !post_process(&mut chunk.name, &mut chunk.chunk_type, node, &source)
                            {
                                tracing::debug!(
                                    name = %chunk.name,
                                    file = %path.display(),
                                    "post_process_chunk: discarded"
                                );
                                continue;
                            }
                        }
                    }
                    chunks.push(chunk);
                }
                Err(e) => {
                    tracing::warn!("Failed to extract chunk from {}: {}", path.display(), e);
                }
            }
        }

        // --- Pass 2: Relationship extraction (calls + types) ---
        let mut cursor2 = tree_sitter::QueryCursor::new();
        let mut matches2 = cursor2.matches(chunk_query, tree.root_node(), source.as_bytes());

        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let capture_names = chunk_query.capture_names();
        let name_idx = chunk_query.capture_index_for_name("name");

        while let Some(m) = matches2.next() {
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                types::CHUNK_CAPTURE_NAMES.contains(&name)
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            let mut name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            if let Some(post_process) = language.def().post_process_chunk {
                let cap_name = capture_names
                    .get(func_capture.index as usize)
                    .copied()
                    .unwrap_or("");
                let mut ct =
                    types::capture_name_to_chunk_type(cap_name).unwrap_or(ChunkType::Function);
                if !post_process(&mut name, &mut ct, node, &source) {
                    continue;
                }
            }

            let line_start = node.start_position().row as u32 + 1;
            let byte_range = node.byte_range();

            // Call extraction
            call_cursor.set_byte_range(byte_range.clone());
            calls.clear();

            let mut call_matches =
                call_cursor.matches(call_query, tree.root_node(), source.as_bytes());

            while let Some(cm) = call_matches.next() {
                for cap in cm.captures {
                    let callee_name = source[cap.node.byte_range()].to_string();
                    let call_line = cap.node.start_position().row as u32 + 1;

                    if !calls::should_skip_callee(&callee_name) {
                        calls.push(CallSite {
                            callee_name,
                            line_number: call_line,
                        });
                    }
                }
            }

            seen.clear();
            calls.retain(|c| seen.insert(c.callee_name.clone()));

            if !calls.is_empty() {
                call_results.push(FunctionCalls {
                    name: name.clone(),
                    line_start,
                    calls: std::mem::take(&mut calls),
                });
            }

            // Type extraction
            let mut type_refs =
                self.extract_types(&source, &tree, language, byte_range.start, byte_range.end);
            type_refs.retain(|t| t.type_name != name);

            if !type_refs.is_empty() {
                type_results.push(ChunkTypeRefs {
                    name,
                    line_start,
                    type_refs,
                });
            }
        }

        // --- Phase 3: Injection (combined chunks + relationships) ---
        let injections = language.def().injections;
        if !injections.is_empty() {
            // Release borrows on the outer tree before injection phase
            drop(matches);
            drop(cursor);
            drop(matches2);
            drop(cursor2);

            let groups = injection::find_injection_ranges(&tree, &source, injections);

            // Free outer tree/parser memory before inner parse allocations
            drop(tree);
            drop(parser);

            for group in &groups {
                match self.parse_injected_all(&source, path, group, 0) {
                    Ok((inner_chunks, inner_calls, inner_types))
                        if !inner_chunks.is_empty()
                            || !inner_calls.is_empty()
                            || !inner_types.is_empty() =>
                    {
                        if !inner_chunks.is_empty() {
                            let before = chunks.len();
                            chunks.retain(|c| {
                                !injection::chunk_within_container(
                                    c.line_start,
                                    c.line_end,
                                    &group.container_lines,
                                )
                            });
                            let removed = before - chunks.len();
                            tracing::debug!(
                                language = %group.language,
                                removed,
                                added = inner_chunks.len(),
                                "Replaced outer chunks with injection results"
                            );
                            chunks.extend(inner_chunks);
                        }
                        if !inner_calls.is_empty() || !inner_types.is_empty() {
                            call_results.retain(|fc| {
                                !injection::chunk_within_container(
                                    fc.line_start,
                                    fc.line_start,
                                    &group.container_lines,
                                )
                            });
                            type_results.retain(|tr| {
                                !injection::chunk_within_container(
                                    tr.line_start,
                                    tr.line_start,
                                    &group.container_lines,
                                )
                            });
                            call_results.extend(inner_calls);
                            type_results.extend(inner_types);
                        }
                    }
                    Ok(_) => {
                        // Zero results — keep outer as-is
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            language = %group.language,
                            "Injection parsing failed, keeping outer"
                        );
                    }
                }
            }
        }

        Ok((chunks, call_results, type_results))
    }

    pub fn supported_extensions(&self) -> Vec<&'static str> {
        crate::language::REGISTRY.supported_extensions().collect()
    }
}

/// Find a direct child of a tree-sitter node by kind.
///
/// Shared helper used by injection parsing and HTML language definition.
#[allow(clippy::manual_find)]
pub(crate) fn find_child_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

/// Find the definition node (function/struct/class/etc.) from a query match's captures.
pub(crate) fn extract_definition_node<'c, 't>(
    m: &tree_sitter::QueryMatch<'c, 't>,
    query: &tree_sitter::Query,
) -> Option<tree_sitter::Node<'t>> {
    const DEF_CAPTURES: &[&str] = &[
        "function",
        "struct",
        "class",
        "enum",
        "trait",
        "interface",
        "const",
        "section",
        "module",
        "macro",
        "object",
        "typealias",
        "property",
        "delegate",
        "event",
    ];
    DEF_CAPTURES.iter().find_map(|name| {
        query
            .capture_index_for_name(name)
            .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
            .map(|c| c.node)
    })
}
