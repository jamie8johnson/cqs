//! Code parsing with tree-sitter
//!
//! Split into submodules:
//! - `types` — data structures and error types
//! - `chunk` — chunk extraction from parse trees
//! - `calls` — call site extraction for call graph
//! - `injection` — multi-grammar injection (HTML→JS/CSS via `set_included_ranges()`)
//! - `markdown` — heading-based Markdown parser with cross-reference extraction
//! - `aspx` — ASP.NET Web Forms parser (delegates to C#/VB.NET grammars)

pub mod aspx;
mod calls;
pub(crate) mod chunk;
pub(crate) mod injection;
pub mod l5x;
pub mod markdown;
pub mod types;

pub use types::{
    CallSite, Chunk, ChunkType, ChunkTypeRefs, FieldStyle, FunctionCalls, Language, ParserError,
    SignatureStyle, TypeEdgeKind, TypeRef,
};

use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::StreamingIterator;

/// Default per-file size cap for parsing (50 MiB). Files larger than this
/// are skipped at parse time. Override at runtime via
/// `CQS_PARSER_MAX_FILE_SIZE`. P3 #104: distinct from `CQS_MAX_FILE_SIZE`
/// (the file-discovery gate in `lib.rs::max_file_size`) so per-stage
/// knobs stay independent — bumping one doesn't silently bump the other.
///
/// Production paths read [`crate::limits::parser_max_file_size`]; this
/// constant exists for the legacy `tc35_max_file_size_is_50mb` test pin
/// and for downstream crates that still re-export it.
#[allow(dead_code)]
pub(crate) const MAX_FILE_SIZE: u64 = crate::limits::PARSER_MAX_FILE_SIZE;

/// Combined parse result: chunks, function calls, and type references.
/// Returned by `parse_file_all()` and `parse_injected_all()` which extract
/// everything in a single file read + tree-sitter parse.
pub type ParseAllResult = (Vec<Chunk>, Vec<FunctionCalls>, Vec<ChunkTypeRefs>);

/// Combined parse result PLUS per-chunk call sites.
/// Returned by [`Parser::parse_file_all_with_chunk_calls`] and used by the
/// indexing pipeline (`pipeline::parser_stage()`) to populate the per-chunk
/// `calls` table without re-parsing each chunk's body. The fourth element
/// pairs each extracted [`CallSite`] with the originating chunk's id (using
/// the path-based id format produced by `extract_chunk`; the pipeline rewrites
/// these to relative-path ids before persisting). See P2 #63 in
/// `docs/audit-findings.md`.
pub type ParseAllWithChunkCallsResult = (
    Vec<Chunk>,
    Vec<FunctionCalls>,
    Vec<ChunkTypeRefs>,
    Vec<(String, CallSite)>,
);

/// Default per-chunk byte cap (100 KiB). Larger chunks are dropped at
/// parse time before windowing sees them. Override at runtime via
/// `CQS_PARSER_MAX_CHUNK_BYTES`. P3 #105.
///
/// Production paths read [`crate::limits::parser_max_chunk_bytes`]; this
/// constant is kept for crate-external references and any future test
/// pins.
#[allow(dead_code)]
pub(crate) const MAX_CHUNK_BYTES: usize = crate::limits::PARSER_MAX_CHUNK_BYTES;

/// Code parser using tree-sitter grammars
/// Extracts functions, methods, classes, and other code elements
/// from source files in supported languages.
/// # Example
/// ```no_run
/// use cqs::Parser;
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
            let grammar = language.try_grammar().ok_or_else(|| {
                ParserError::QueryCompileFailed(
                    language.to_string(),
                    "no tree-sitter grammar".into(),
                )
            })?;
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
            let grammar = language.try_grammar().ok_or_else(|| {
                ParserError::QueryCompileFailed(
                    format!("{}_calls", language),
                    "no tree-sitter grammar".into(),
                )
            })?;
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
            let grammar = language.try_grammar().ok_or_else(|| {
                ParserError::QueryCompileFailed(
                    format!("{}_types", language),
                    "no tree-sitter grammar".into(),
                )
            })?;
            let pattern = language.type_query_pattern();
            tree_sitter::Query::new(&grammar, pattern).map_err(|e| {
                ParserError::QueryCompileFailed(format!("{}_types", language), format!("{}", e))
            })
        })
    }

    /// Parse a source file and extract code chunks
    /// Returns an empty Vec for non-UTF8 files (with a warning logged).
    /// Returns an error for unsupported file types.
    pub fn parse_file(&self, path: &Path) -> Result<Vec<Chunk>, ParserError> {
        let _span = tracing::info_span!("parse_file", path = %path.display()).entered();

        // P3 #104: cap is env-overridable (CQS_PARSER_MAX_FILE_SIZE).
        let max_file_size = crate::limits::parser_max_file_size();
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > max_file_size => {
                tracing::warn!(
                    size_mb = meta.len() / (1024 * 1024),
                    cap_mb = max_file_size / (1024 * 1024),
                    path = %path.display(),
                    "Skipping large file; bump CQS_PARSER_MAX_FILE_SIZE if needed"
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
                tracing::warn!(path = %path.display(), "Skipping non-UTF8 file");
                return Ok(vec![]);
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistent hashing across
        // platforms. PF-V1.29-5: the unconditional `replace` allocated a full
        // copy of every source file even on Unix-native repos that never had
        // CRLF. The `contains` probe keeps the Windows-origin path correct
        // while saving one string allocation per file in the common case.
        let source = if source.contains("\r\n") {
            source.replace("\r\n", "\n")
        } else {
            source
        };

        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();

        // Rockwell PLC exports need custom ST extraction
        if ext == "l5x" {
            return l5x::parse_l5x_chunks(&source, path, self);
        }
        if ext == "l5k" {
            return l5x::parse_l5k_chunks(&source, path, self);
        }

        let language = Language::from_extension(&ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        self.parse_source(&source, language, path)
    }

    /// Parse in-memory source code and extract code chunks.
    /// Like `parse_file`, but operates on already-read source content with a
    /// known language. The `path` is used only for chunk origin metadata
    /// (`Chunk.file` field), not for filesystem access.
    /// Used by `train_data` to parse `git show` output without writing temp files.
    pub fn parse_source(
        &self,
        source: &str,
        language: Language,
        path: &Path,
    ) -> Result<Vec<Chunk>, ParserError> {
        let _span = tracing::info_span!("parse_source", path = %path.display()).entered();

        // Grammar-less languages use custom parsers (issue #954):
        // route via LanguageDef function pointers, not match arms. Adding
        // a new grammar-less language without setting `custom_chunk_parser`
        // falls through to the markdown default — same as before, but now
        // the routing is declarative and centralized in the language row.
        if language.def().grammar.is_none() {
            return match language.def().custom_chunk_parser {
                Some(f) => f(source, path, self),
                None => {
                    // Markdown (and any future grammar-less language
                    // that opts into the default line-based parser)
                    let mut chunks = crate::parser::markdown::parse_markdown_chunks(source, path)?;
                    let fenced = crate::parser::markdown::extract_fenced_blocks(source);
                    chunks.extend(self.parse_fenced_blocks(&fenced, source, path));
                    Ok(chunks)
                }
            };
        }

        let grammar = language.try_grammar().ok_or_else(|| {
            ParserError::ParseFailed(format!("{} has no tree-sitter grammar", language))
        })?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{}", e)))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| ParserError::ParseFailed(path.display().to_string()))?;

        // Get or compile query (lazy initialization)
        let query = self.get_query(language)?;

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        let mut chunks = Vec::new();
        // P3 #105: env-overridable per-chunk byte cap. Track drops so we
        // can emit a single summary warn at end-of-file rather than one
        // log line per skipped chunk.
        let max_chunk_bytes = crate::limits::parser_max_chunk_bytes();
        let mut dropped_oversized = 0usize;

        while let Some(m) = matches.next() {
            match self.extract_chunk(source, m, query, language, path) {
                Ok(mut chunk) => {
                    // P3 #105: chunks above the cap are dropped. The
                    // pipeline's windowing stage only sees chunks that pass
                    // here — windowing is downstream of this gate, not a
                    // mitigation for it (the previous "handled by windowing"
                    // comment was wrong).
                    if chunk.content.len() > max_chunk_bytes {
                        tracing::debug!(
                            "Skipping {} ({} bytes > {} max)",
                            chunk.id,
                            chunk.content.len(),
                            max_chunk_bytes
                        );
                        dropped_oversized += 1;
                        continue;
                    }
                    // Apply post-process hook (e.g., HCL block reclassification)
                    if let Some(post_process) = language.def().post_process_chunk {
                        if let Some(node) = extract_definition_node(m, query) {
                            if !post_process(&mut chunk.name, &mut chunk.chunk_type, node, source) {
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
                    tracing::warn!(path = %path.display(), error = %e, "Failed to extract chunk");
                }
            }
        }

        // --- Phase 2: Injection parsing (multi-grammar) ---
        let injections = language.def().injections;
        if !injections.is_empty() {
            // Release borrows on the outer tree before injection phase
            drop(matches);
            drop(cursor);

            let groups = injection::find_injection_ranges(&tree, source, injections);

            // Free outer tree/parser memory before inner parse allocations
            drop(tree);
            drop(parser);

            for group in &groups {
                match self.parse_injected_chunks(source, path, group, 0) {
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

        // P3 #105: per-file summary of oversized-chunk drops, surfaced at
        // warn level so users discover CQS_PARSER_MAX_CHUNK_BYTES instead of
        // wondering why their indexed corpus has gaps.
        if dropped_oversized > 0 {
            tracing::warn!(
                path = %path.display(),
                dropped = dropped_oversized,
                cap_bytes = max_chunk_bytes,
                "Dropped oversized chunks; bump CQS_PARSER_MAX_CHUNK_BYTES if needed"
            );
        }

        Ok(chunks)
    }

    /// Parse a source file and extract chunks, calls, AND type references in one pass.
    /// Combines `parse_file()` and `parse_file_relationships()` to avoid double
    /// file read + double tree-sitter parse. Single file read, single outer parse,
    /// two query cursor passes on the same tree, single injection parse.
    /// Returns `(chunks, function_calls, chunk_type_refs)`.
    /// Used by `watch::reindex_files()` for incremental updates. The indexing
    /// pipeline calls [`Self::parse_file_all_with_chunk_calls`] instead so it
    /// can populate the per-chunk `calls` table from the same Pass 2 walk.
    pub fn parse_file_all(&self, path: &Path) -> Result<ParseAllResult, ParserError> {
        let (chunks, calls, types, _chunk_calls) = self.parse_file_all_inner(path, false)?;
        Ok((chunks, calls, types))
    }

    /// Like [`Self::parse_file_all`], but also returns one
    /// `(chunk_id, CallSite)` pair for every call extracted during Pass 2.
    /// Eliminates the redundant `extract_calls_from_chunk(chunk)` loop the
    /// indexing pipeline used to run after `parse_file_all` (one extra
    /// tree-sitter parse per chunk; ~14k extra parses per cqs reindex). See
    /// P2 #63 in `docs/audit-findings.md`.
    ///
    /// The chunk ids returned here use the absolute-path format produced by
    /// `extract_chunk` (`{path}:{line_start}:{hash_prefix}`). The indexing
    /// pipeline rewrites these to relative-path ids via the same id_map it
    /// already builds for the chunks themselves, so the chunk_calls list and
    /// the chunks list stay in sync.
    ///
    /// **Duplication note (P2 #63):** the body delegates to
    /// `parse_file_all_inner` with `want_chunk_calls = true`, which also
    /// powers `parse_file_all`. Watch (`src/cli/watch.rs`) still uses
    /// `parse_file_all` and runs its own `extract_calls_from_chunk` per chunk;
    /// collapsing that into this method is a separate refactor — Watch's
    /// parser stage is rayon-parallel and changing its call shape requires
    /// more work than the indexing-pipeline path.
    pub fn parse_file_all_with_chunk_calls(
        &self,
        path: &Path,
    ) -> Result<ParseAllWithChunkCallsResult, ParserError> {
        self.parse_file_all_inner(path, true)
    }

    /// Shared implementation for [`Self::parse_file_all`] and
    /// [`Self::parse_file_all_with_chunk_calls`]. The `want_chunk_calls` flag
    /// guards the per-chunk-id bookkeeping (a `(start_byte, end_byte)` →
    /// `chunk.id` map plus the per-call emit) so callers that only need the
    /// existing 3-element shape pay nothing extra. When `want_chunk_calls` is
    /// `false` the returned 4th element is always empty.
    fn parse_file_all_inner(
        &self,
        path: &Path,
        want_chunk_calls: bool,
    ) -> Result<ParseAllWithChunkCallsResult, ParserError> {
        let _span = tracing::info_span!("parse_file_all", path = %path.display()).entered();

        // P3 #104: env-overridable cap (CQS_PARSER_MAX_FILE_SIZE).
        let max_file_size = crate::limits::parser_max_file_size();
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > max_file_size => {
                tracing::warn!(
                    size_mb = meta.len() / (1024 * 1024),
                    cap_mb = max_file_size / (1024 * 1024),
                    path = %path.display(),
                    "Skipping large file; bump CQS_PARSER_MAX_FILE_SIZE if needed"
                );
                return Ok((vec![], vec![], vec![], vec![]));
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }

        // Read file once
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!(path = %path.display(), "Skipping non-UTF8 file");
                return Ok((vec![], vec![], vec![], vec![]));
            }
            Err(e) => return Err(e.into()),
        };

        // Normalize line endings (CRLF -> LF) for consistent hashing across
        // platforms. PF-V1.29-5: the unconditional `replace` allocated a full
        // copy of every source file even on Unix-native repos that never had
        // CRLF. The `contains` probe keeps the Windows-origin path correct
        // while saving one string allocation per file in the common case.
        let source = if source.contains("\r\n") {
            source.replace("\r\n", "\n")
        } else {
            source
        };

        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();

        let language = Language::from_extension(&ext)
            .ok_or_else(|| ParserError::UnsupportedFileType(ext.to_string()))?;

        // Grammar-less languages use custom parsers (issue #954):
        // routing is declarative via `custom_all_parser`, not a match arm.
        // For these the chunk_calls path falls back to per-chunk
        // `extract_calls_from_chunk` because Markdown's per-chunk reference
        // scan is line-based (no tree-sitter cost) and custom parsers like
        // ASPX delegate to inner-language tree-sitter parsers anyway —
        // collapsing those into the chunked walk would require refactoring
        // each custom parser. Same audit gap as Watch (`P2 #63`).
        if language.def().grammar.is_none() {
            let (chunks, calls, types) = match language.def().custom_all_parser {
                Some(f) => f(&source, path, self)?,
                None => {
                    // Markdown (and any future grammar-less language
                    // that opts into the default line-based parser)
                    let mut chunks = crate::parser::markdown::parse_markdown_chunks(&source, path)?;
                    let calls = crate::parser::markdown::parse_markdown_references(&source, path)?;
                    let fenced = crate::parser::markdown::extract_fenced_blocks(&source);
                    chunks.extend(self.parse_fenced_blocks(&fenced, &source, path));
                    (chunks, calls, vec![])
                }
            };
            let chunk_calls = if want_chunk_calls {
                let mut sink: Vec<(String, CallSite)> = Vec::new();
                for chunk in &chunks {
                    for call in self.extract_calls_from_chunk(chunk) {
                        sink.push((chunk.id.clone(), call));
                    }
                }
                sink
            } else {
                Vec::new()
            };
            return Ok((chunks, calls, types, chunk_calls));
        }

        // Single tree-sitter parse
        let grammar = language.try_grammar().ok_or_else(|| {
            ParserError::ParseFailed(format!("{} has no tree-sitter grammar", language))
        })?;
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
        // Map (start_byte, end_byte) -> chunk.id for non-discarded chunks.
        // Built only when chunk_calls are requested so the no-chunk-calls
        // path (Watch) pays nothing. Pass 2 looks up the chunk id by the
        // func_node's byte_range and emits (chunk_id, CallSite) pairs
        // without re-parsing the chunk body — eliminating the per-chunk
        // tree-sitter re-parse the indexing pipeline used to run.
        let mut byte_range_to_chunk_id: HashMap<(usize, usize), String> = HashMap::new();
        // P3 #105: env-overridable per-chunk byte cap; track drops for a
        // single warn at end-of-file.
        let max_chunk_bytes = crate::limits::parser_max_chunk_bytes();
        let mut dropped_oversized = 0usize;

        while let Some(m) = matches.next() {
            // Capture the def node up-front so we can record its byte_range
            // alongside the chunk id once we know the chunk survived the
            // size + post_process filters. `extract_definition_node` is the
            // same helper Pass 2 (and post_process below) use, so the
            // byte_range we record is exactly what Pass 2's `func_node`
            // produces — guaranteeing a hit on lookup.
            let def_node = if want_chunk_calls {
                extract_definition_node(m, chunk_query)
            } else {
                None
            };
            match self.extract_chunk(&source, m, chunk_query, language, path) {
                Ok(mut chunk) => {
                    if chunk.content.len() > max_chunk_bytes {
                        tracing::debug!(
                            "Skipping {} ({} bytes > {} max)",
                            chunk.id,
                            chunk.content.len(),
                            max_chunk_bytes
                        );
                        dropped_oversized += 1;
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
                    if want_chunk_calls {
                        if let Some(node) = def_node {
                            let r = node.byte_range();
                            byte_range_to_chunk_id.insert((r.start, r.end), chunk.id.clone());
                        }
                    }
                    chunks.push(chunk);
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to extract chunk");
                }
            }
        }

        // --- Pass 2: Relationship extraction (calls + types) ---
        let mut cursor2 = tree_sitter::QueryCursor::new();
        let mut matches2 = cursor2.matches(chunk_query, tree.root_node(), source.as_bytes());

        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        // PERF P2 #63: emit (chunk_id, CallSite) pairs from the same Pass 2
        // walk that builds `call_results`. The indexing pipeline used to
        // re-parse every chunk body afterwards just to populate this list.
        let mut chunk_calls: Vec<(String, CallSite)> = Vec::new();
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let capture_names = chunk_query.capture_names();
        let name_idx = chunk_query.capture_index_for_name("name");

        while let Some(m) = matches2.next() {
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                types::capture_name_to_chunk_type(name).is_some()
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
                if want_chunk_calls {
                    if let Some(chunk_id) =
                        byte_range_to_chunk_id.get(&(byte_range.start, byte_range.end))
                    {
                        // Match the legacy `extract_calls_from_chunk(chunk)`
                        // behavior: per-chunk extraction passed line_offset=0
                        // against the chunk content alone, so chunk_calls
                        // line numbers were 1-indexed RELATIVE to the chunk.
                        // Pass-2 here scans the whole file and produces
                        // ABSOLUTE line numbers, so we subtract `line_start`
                        // (saturating, .max(1)) to get the same relative
                        // numbering downstream consumers (the `calls`
                        // SQLite table) already expect.
                        for call in &calls {
                            let rel_line = call
                                .line_number
                                .saturating_sub(line_start.saturating_sub(1))
                                .max(1);
                            chunk_calls.push((
                                chunk_id.clone(),
                                CallSite {
                                    callee_name: call.callee_name.clone(),
                                    line_number: rel_line,
                                },
                            ));
                        }
                    }
                    // No matching chunk id ⇒ Pass 1 discarded the chunk
                    // (oversize / post_process). The previous pipeline loop
                    // also skipped these because it iterated `&chunks`.
                }
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
                            // Identify outer chunk ids about to be dropped
                            // so we can also drop their entries from
                            // chunk_calls. Done before retain so we don't
                            // iterate twice.
                            let drop_ids: std::collections::HashSet<String> = if want_chunk_calls {
                                chunks
                                    .iter()
                                    .filter(|c| {
                                        injection::chunk_within_container(
                                            c.line_start,
                                            c.line_end,
                                            &group.container_lines,
                                        )
                                    })
                                    .map(|c| c.id.clone())
                                    .collect()
                            } else {
                                std::collections::HashSet::new()
                            };
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
                            if want_chunk_calls {
                                // Drop outer chunk_calls that lived inside
                                // this injection container (their chunks are
                                // gone) and emit per-inner-chunk calls.
                                // Inner chunks are typically small (script
                                // blocks inside HTML/ASPX) and few per file,
                                // so the cost is negligible compared to the
                                // outer hot-path saving.
                                chunk_calls.retain(|(id, _)| !drop_ids.contains(id));
                                for chunk in &inner_chunks {
                                    for call in self.extract_calls_from_chunk(chunk) {
                                        chunk_calls.push((chunk.id.clone(), call));
                                    }
                                }
                            }
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

        // P3 #105: per-file summary of oversized-chunk drops, surfaced at
        // warn level so users discover CQS_PARSER_MAX_CHUNK_BYTES.
        if dropped_oversized > 0 {
            tracing::warn!(
                path = %path.display(),
                dropped = dropped_oversized,
                cap_bytes = max_chunk_bytes,
                "Dropped oversized chunks; bump CQS_PARSER_MAX_CHUNK_BYTES if needed"
            );
        }

        Ok((chunks, call_results, type_results, chunk_calls))
    }

    /// Retrieves the list of file extensions supported by the language registry.
    /// # Returns
    /// A vector of supported file extensions as static string slices (e.g., "rs", "py", "js").
    pub fn supported_extensions(&self) -> Vec<&'static str> {
        crate::language::REGISTRY.supported_extensions().collect()
    }

    /// Parse fenced code blocks from markdown into typed chunks.
    /// For each block with a recognized language, parses the content with that
    /// language's tree-sitter grammar and extracts chunks. Line numbers are
    /// adjusted to reflect their position in the original markdown file.
    fn parse_fenced_blocks(
        &self,
        blocks: &[markdown::FencedBlock],
        _source: &str,
        path: &Path,
    ) -> Vec<Chunk> {
        let _span = tracing::info_span!("parse_fenced_blocks", count = blocks.len()).entered();
        let mut result = Vec::new();

        for block in blocks {
            let language = match block.lang.parse::<Language>() {
                Ok(lang) if lang.is_enabled() => lang,
                _ => continue,
            };

            // Skip grammar-less languages (avoid recursion for nested markdown)
            let def = language.def();
            let grammar_fn = match def.grammar {
                Some(f) => f,
                None => continue,
            };

            let grammar = grammar_fn();
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(&grammar).is_err() {
                tracing::debug!(lang = %block.lang, "Failed to set tree-sitter language for fenced block");
                continue;
            }

            let tree = match parser.parse(&block.content, None) {
                Some(t) => t,
                None => {
                    tracing::debug!(lang = %block.lang, "Tree-sitter parse returned None for fenced block");
                    continue;
                }
            };

            let query = match self.get_query(language) {
                Ok(q) => q,
                Err(e) => {
                    tracing::debug!(lang = %block.lang, error = %e, "Failed to get query for fenced block");
                    continue;
                }
            };

            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(query, tree.root_node(), block.content.as_bytes());

            // Line offset: fenced block content starts on the line after the opening fence
            let line_offset = block.line_start; // fence is at line_start, content starts at line_start+1

            // P3 #105: env-overridable per-chunk byte cap.
            let max_chunk_bytes = crate::limits::parser_max_chunk_bytes();

            while let Some(m) = matches.next() {
                match self.extract_chunk(&block.content, m, query, language, path) {
                    Ok(mut chunk) => {
                        if chunk.content.len() > max_chunk_bytes {
                            continue;
                        }
                        // Apply post-process if defined
                        if let Some(post_process) = def.post_process_chunk {
                            if let Some(node) = extract_definition_node(m, query) {
                                if !post_process(
                                    &mut chunk.name,
                                    &mut chunk.chunk_type,
                                    node,
                                    &block.content,
                                ) {
                                    continue;
                                }
                            }
                        }
                        // Adjust line numbers to markdown file position
                        chunk.line_start += line_offset;
                        chunk.line_end += line_offset;
                        // Rebuild ID with adjusted line numbers
                        let hash_prefix =
                            chunk.content_hash.get(..8).unwrap_or(&chunk.content_hash);
                        chunk.id =
                            format!("{}:{}:{}", path.display(), chunk.line_start, hash_prefix);
                        result.push(chunk);
                    }
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            language = %language,
                            "Failed to extract chunk from fenced block"
                        );
                    }
                }
            }
        }

        tracing::debug!(chunks = result.len(), "Extracted chunks from fenced blocks");
        result
    }
}

/// Find a direct child of a tree-sitter node by kind.
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
    // Uses ChunkType::CAPTURE_NAMES — single source of truth from define_chunk_types! macro
    ChunkType::CAPTURE_NAMES.iter().find_map(|name| {
        query
            .capture_index_for_name(name)
            .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
            .map(|c| c.node)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Verifies that the Parser correctly extracts function definitions from Rust source code.
    /// This is a unit test that validates the `parse_source` method's ability to identify and parse individual functions from a source file. It tests parsing a Rust snippet containing two function definitions and asserts that both functions are extracted as separate chunks with their correct names.
    /// # Arguments
    /// None. This is a test function that creates its own test data internally.
    /// # Returns
    /// None. This function performs assertions and will panic if any assertion fails.
    /// # Panics
    /// Panics if the parser initialization fails, if source parsing fails, or if any of the assertions about extracted chunks fail (incorrect count, missing function names).

    #[test]
    fn parse_source_extracts_functions() {
        let parser = Parser::new().unwrap();
        let source = "fn hello() { println!(\"hi\"); }\nfn world() { }";
        let chunks = parser
            .parse_source(source, Language::Rust, Path::new("test.rs"))
            .unwrap();
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().any(|c| c.name == "hello"));
        assert!(chunks.iter().any(|c| c.name == "world"));
    }

    #[test]
    fn parse_source_empty_string() {
        let parser = Parser::new().unwrap();
        let chunks = parser
            .parse_source("", Language::Rust, Path::new("test.rs"))
            .unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_source_whitespace_only() {
        let parser = Parser::new().unwrap();
        let chunks = parser
            .parse_source("   \n\t\n  \n", Language::Rust, Path::new("test.rs"))
            .unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_source_only_comments() {
        let parser = Parser::new().unwrap();
        let source = "// comment\n/* block */";
        let chunks = parser
            .parse_source(source, Language::Rust, Path::new("test.rs"))
            .unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_source_binary_content_no_panic() {
        let parser = Parser::new().unwrap();
        // Safety: we are deliberately constructing a &str from bytes that are
        // not valid UTF-8 sequences. tree-sitter must not panic on this.
        // Using a lossy approach: embed the bytes in a string literal that
        // Rust allows by escaping them.
        let source = "\x00\x01\x02";
        // This should not panic — result may be Ok or Err, both are acceptable.
        let _ = parser.parse_source(source, Language::Rust, Path::new("binary.rs"));
    }

    #[test]
    fn parse_source_extremely_long_line() {
        let parser = Parser::new().unwrap();
        // 200 000-char line — not valid Rust, but the parser must not panic.
        let long_line = "x".repeat(200_000);
        let _ = parser.parse_source(&long_line, Language::Rust, Path::new("long.rs"));
    }

    #[test]
    fn parse_source_deeply_nested_braces() {
        let parser = Parser::new().unwrap();
        // 500 unclosed opening braces — malformed, but must not panic.
        let source = "{".repeat(500);
        let _ = parser.parse_source(&source, Language::Rust, Path::new("nested.rs"));
    }

    #[test]
    fn parse_source_wrong_language_no_panic() {
        let parser = Parser::new().unwrap();
        // Python source fed to the Rust grammar — must not panic.
        let python_source = "def foo(x):\n    return x + 1\n\nclass Bar:\n    pass\n";
        let _ = parser.parse_source(python_source, Language::Rust, Path::new("wrong.rs"));
    }

    #[test]
    fn parse_source_null_bytes_in_source() {
        let parser = Parser::new().unwrap();
        // Null byte embedded in otherwise-valid Rust — must not panic.
        let source = "fn foo() {}\0fn bar() {}";
        let _ = parser.parse_source(source, Language::Rust, Path::new("null.rs"));
    }

    #[test]
    fn parse_file_all_nonexistent_file() {
        let parser = Parser::new().unwrap();
        let result = parser.parse_file_all(Path::new("/nonexistent/file.rs"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_file_all_empty_file() {
        let parser = Parser::new().unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.rs");
        std::fs::File::create(&path).unwrap();
        let result = parser.parse_file_all(&path).unwrap();
        let (chunks, calls, type_refs) = result;
        assert!(chunks.is_empty());
        assert!(calls.is_empty());
        assert!(type_refs.is_empty());
    }

    // TC-35: Verify the oversized file guard constant and behavior
    #[test]
    fn tc35_max_file_size_is_50mb() {
        assert_eq!(MAX_FILE_SIZE, 50 * 1024 * 1024);
    }

    #[test]
    fn tc35_parse_file_errors_on_missing_file() {
        let parser = Parser::new().unwrap();
        let result = parser.parse_file(std::path::Path::new("/tmp/cqs_nonexistent_tc35.rs"));
        assert!(
            result.is_err(),
            "parse_file should error on nonexistent file"
        );
    }

    /// TC-ADV-V1.33-8: oversized files are silently skipped (Ok(vec![])),
    /// not surfaced as an error. This is the documented contract — a
    /// future refactor that surfaced the size cap as Err would break the
    /// watch loop's "parse failures are recoverable" invariant.
    #[test]
    fn parse_file_returns_empty_vec_for_oversized_file() {
        use std::sync::Mutex;
        // CQS_PARSER_MAX_FILE_SIZE is process-global; serialise.
        static PARSER_SIZE_LOCK: Mutex<()> = Mutex::new(());
        let _guard = PARSER_SIZE_LOCK.lock().unwrap();

        let parser = Parser::new().unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("big.rs");
        // Write a 200-byte Rust file; cap is set to 100 below.
        std::fs::write(&path, vec![b'a'; 200]).unwrap();

        let prev = std::env::var("CQS_PARSER_MAX_FILE_SIZE").ok();
        std::env::set_var("CQS_PARSER_MAX_FILE_SIZE", "100");
        let result = parser.parse_file(&path);
        // Restore env immediately.
        match prev {
            Some(v) => std::env::set_var("CQS_PARSER_MAX_FILE_SIZE", v),
            None => std::env::remove_var("CQS_PARSER_MAX_FILE_SIZE"),
        }

        let chunks = result.expect("oversized file must skip silently, not error");
        assert!(
            chunks.is_empty(),
            "oversized file must produce empty Vec, got {} chunk(s)",
            chunks.len()
        );
    }

    /// TC-ADV-V1.33-8: non-UTF8 file content is silently skipped
    /// (Ok(vec![])). Documented behaviour ("Returns an empty Vec for
    /// non-UTF8 files (with a warning logged)") — pinning so a future
    /// refactor that surfaced this as an error doesn't break the indexer.
    #[test]
    fn parse_file_returns_empty_vec_for_non_utf8_content() {
        let parser = Parser::new().unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad_utf8.rs");
        // Write bytes that are not valid UTF-8 (lone continuation bytes).
        std::fs::write(&path, [0xFFu8, 0xFE, 0xFD, 0xFC]).unwrap();

        let chunks = parser
            .parse_file(&path)
            .expect("non-UTF8 file must skip silently, not error");
        assert!(
            chunks.is_empty(),
            "non-UTF8 file must produce empty Vec, got {} chunk(s)",
            chunks.len()
        );
    }

    /// P2 #63: property test pinning the equivalence between
    /// `parse_file_all_with_chunk_calls` (Pass-2 emit) and the previous
    /// per-chunk `extract_calls_from_chunk` loop the indexing pipeline used
    /// to run. For every (chunk_id, callee_name, line_number) triple the new
    /// method emits, the old per-chunk extractor must emit the same triple
    /// against the same chunk — and vice versa. Run on a small Rust fixture
    /// covering multiple chunks, dedup boundaries, and the
    /// `should_skip_callee` filter.
    #[test]
    fn p2_63_chunk_calls_match_per_chunk_extraction() {
        let parser = Parser::new().unwrap();
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("p2_63_fixture.rs");
        // Three functions with: (a) plain calls, (b) duplicate callees that
        // must dedup within a chunk, (c) self/this noise that should_skip
        // strips, (d) qualified path calls.
        let source = "\
fn alpha() {
    helper();
    helper(); // dup — must collapse to one entry
    util::compute();
}

fn beta() {
    self.field();
    other.method();
    real_call();
}

struct Holder;

impl Holder {
    fn gamma(&self) {
        Self::associated();
        another_call();
    }
}
";
        std::fs::write(&path, source).unwrap();

        // New path
        let (chunks_new, _calls, _types, chunk_calls_new) =
            parser.parse_file_all_with_chunk_calls(&path).unwrap();

        // Old path: parse_file_all (no chunk_calls) + per-chunk extraction
        let (chunks_old, _calls_old, _types_old) = parser.parse_file_all(&path).unwrap();

        // Chunks themselves must match exactly (same Pass 1 walk)
        assert_eq!(
            chunks_new.len(),
            chunks_old.len(),
            "chunk count must match between parse_file_all and parse_file_all_with_chunk_calls"
        );
        for (n, o) in chunks_new.iter().zip(chunks_old.iter()) {
            assert_eq!(n.id, o.id, "chunk id mismatch");
        }

        // Build the legacy per-chunk shape for comparison.
        let mut chunk_calls_old: Vec<(String, String, u32)> = Vec::new();
        for chunk in &chunks_old {
            for call in parser.extract_calls_from_chunk(chunk) {
                chunk_calls_old.push((chunk.id.clone(), call.callee_name, call.line_number));
            }
        }

        let mut chunk_calls_new_shape: Vec<(String, String, u32)> = chunk_calls_new
            .iter()
            .map(|(id, c)| (id.clone(), c.callee_name.clone(), c.line_number))
            .collect();

        // Order may differ between the two paths (Pass-2 emits per-match,
        // per-chunk extraction emits per-chunk), so compare as sets.
        chunk_calls_old.sort();
        chunk_calls_new_shape.sort();
        assert_eq!(
            chunk_calls_new_shape, chunk_calls_old,
            "Pass-2 chunk_calls must equal per-chunk extract_calls_from_chunk loop\nnew: {:#?}\nold: {:#?}",
            chunk_calls_new_shape, chunk_calls_old
        );

        // Sanity: skip filter actually fires (no `self`/`Self` callees).
        for (_, name, _) in &chunk_calls_new_shape {
            assert!(
                !matches!(name.as_str(), "self" | "Self" | "this" | "super"),
                "should_skip_callee leak: {name}"
            );
        }
    }
}
