//! Multi-grammar injection parsing
//!
//! Implements recursive parsing for files containing embedded languages:
//! 1. Parse the outer grammar (e.g., PHP)
//! 2. Find injection regions (e.g., `text` nodes containing HTML)
//! 3. Re-parse those regions with inner grammars (e.g., HTML)
//! 4. If the inner language has its own injection rules, recurse (e.g., HTML→JS/CSS)
//!
//! Uses tree-sitter's `set_included_ranges()` for byte-accurate inner parsing.
//! Recursion is bounded by `MAX_INJECTION_DEPTH` (default: 3).

use std::collections::HashMap;
use std::path::Path;

use tree_sitter::StreamingIterator;

use super::types::{
    capture_name_to_chunk_type, Chunk, ChunkType, ChunkTypeRefs, FunctionCalls, Language,
    ParserError,
};
use super::Parser;
use crate::language::InjectionRule;

/// Maximum number of injection ranges per file. Prevents OOM from crafted
/// files with millions of tiny injection containers (e.g., `<script>` blocks).
const MAX_INJECTION_RANGES: usize = 1000;

/// Maximum depth for recursive injection. Prevents infinite loops and bounds
/// the cost of chained injection (e.g., PHP→HTML→JS is depth 2).
const MAX_INJECTION_DEPTH: usize = 3;

/// Result of scanning an outer tree for injection regions.
/// Groups byte ranges by target language, plus tracks which outer chunk
/// line ranges correspond to injection containers (for removal).
pub(crate) struct InjectionGroup {
    /// Resolved inner language
    pub language: Language,
    /// Byte ranges for `set_included_ranges()`
    pub ranges: Vec<tree_sitter::Range>,
    /// Line ranges of container nodes (start, end) — outer chunks overlapping
    /// these should be replaced by inner chunks
    pub container_lines: Vec<(u32, u32)>,
}

/// Scan an outer parse tree for injection regions defined by the given rules.
/// Returns injection groups — each group has a target language, byte ranges
/// for inner parsing, and line ranges of the container nodes to replace.
pub(crate) fn find_injection_ranges(
    tree: &tree_sitter::Tree,
    source: &str,
    rules: &[InjectionRule],
) -> Vec<InjectionGroup> {
    let _span = tracing::debug_span!("find_injection_ranges", rules = rules.len()).entered();

    // Collect (language_name, range, container_lines) tuples
    let mut entries: Vec<(&str, tree_sitter::Range, (u32, u32))> = Vec::new();

    let root = tree.root_node();

    for rule in rules {
        walk_for_containers(root, rule, source, &mut entries);
    }

    if entries.is_empty() {
        return vec![];
    }

    // Cap injection ranges to prevent OOM from crafted files
    if entries.len() > MAX_INJECTION_RANGES {
        tracing::warn!(
            count = entries.len(),
            limit = MAX_INJECTION_RANGES,
            "Too many injection ranges, truncating to limit"
        );
        entries.truncate(MAX_INJECTION_RANGES);
    }

    // Deduplicate by byte range (guards against two rules sharing a container_kind)
    entries.dedup_by(|a, b| a.1.start_byte == b.1.start_byte && a.1.end_byte == b.1.end_byte);

    // Group by resolved language using HashMap for O(1) lookup
    let mut group_index: HashMap<Language, usize> = HashMap::new();
    let mut groups: Vec<InjectionGroup> = Vec::new();
    for (lang_name, range, lines) in entries {
        // Resolve language
        let language = match lang_name.parse::<Language>() {
            Ok(lang) if lang.is_enabled() && lang.def().grammar.is_some() => lang,
            Ok(lang) => {
                tracing::warn!(
                    language = lang_name,
                    "Injection target language '{}' not available (disabled or no grammar)",
                    lang
                );
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    language = lang_name,
                    "Injection target language '{}' not recognized",
                    lang_name
                );
                continue;
            }
        };

        if let Some(&idx) = group_index.get(&language) {
            groups[idx].ranges.push(range);
            groups[idx].container_lines.push(lines);
        } else {
            let idx = groups.len();
            group_index.insert(language, idx);
            groups.push(InjectionGroup {
                language,
                ranges: vec![range],
                container_lines: vec![lines],
            });
        }
    }

    groups
}

/// Advance a tree cursor to the next sibling, walking up parents as needed.
/// Returns `false` if the entire tree has been exhausted.
fn advance_cursor(cursor: &mut tree_sitter::TreeCursor) -> bool {
    if cursor.goto_next_sibling() {
        return true;
    }
    loop {
        if !cursor.goto_parent() {
            return false;
        }
        if cursor.goto_next_sibling() {
            return true;
        }
    }
}

/// Calculate a `tree_sitter::Point` (row, column) from a byte offset in source text.
/// Used by `_inner` content mode where we compute content ranges from source text
/// rather than from tree-sitter node positions.
fn byte_offset_to_point(source: &str, byte: usize) -> tree_sitter::Point {
    let byte = byte.min(source.len());
    let byte = source.floor_char_boundary(byte);
    let before = &source[..byte];
    let row = before.as_bytes().iter().filter(|&&b| b == b'\n').count();
    let col = before.len() - before.rfind('\n').map(|p| p + 1).unwrap_or(0);
    tree_sitter::Point { row, column: col }
}

/// Walk the tree using a cursor to find all nodes matching an injection rule's container_kind.
/// Creates and manages its own cursor — callers don't need to handle cursor state.
/// Supports two content extraction modes:
/// - **Named children** (`content_kind` is a node kind string): finds child nodes matching
///   the kind, e.g., `raw_text` inside `script_element`.
/// - **Inner content** (`content_kind == "_inner"`): extracts the bytes between the first `>`
///   and last `</` in the container's source text. Used when grammars have generic element
///   nodes without named content children (e.g., Razor's `element` node).
fn walk_for_containers(
    root: tree_sitter::Node,
    rule: &InjectionRule,
    source: &str,
    entries: &mut Vec<(&str, tree_sitter::Range, (u32, u32))>,
) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();

        if node.kind() == rule.container_kind {
            // Determine target language (once per container)
            let target = if let Some(detect) = rule.detect_language {
                detect(node, source).unwrap_or(rule.target_language)
            } else {
                rule.target_language
            };

            // Skip non-parseable content (e.g., JSON-LD, shader scripts)
            if target != "_skip" {
                let container_lines = (
                    node.start_position().row as u32 + 1,
                    node.end_position().row as u32 + 1,
                );

                if rule.content_kind == "_inner" {
                    // Inner content mode: extract bytes between first '>' and last '</'
                    // in the container's source text. Used for grammars with generic
                    // element nodes (e.g., Razor) that lack named content children.
                    let text = &source[node.byte_range()];
                    if let Some(tag_close) = text.find('>') {
                        let content_start = node.start_byte() + tag_close + 1;
                        if let Some(close_pos) = text.rfind("</") {
                            let content_end = node.start_byte() + close_pos;
                            if content_start < content_end {
                                let start_point = byte_offset_to_point(source, content_start);
                                let end_point = byte_offset_to_point(source, content_end);
                                let range = tree_sitter::Range {
                                    start_byte: content_start,
                                    end_byte: content_end,
                                    start_point,
                                    end_point,
                                };
                                entries.push((target, range, container_lines));
                            }
                        }
                    }
                } else {
                    // Named children mode: collect ALL matching content children
                    // (error recovery may split raw_text into multiple nodes)
                    let mut child_cursor = node.walk();
                    for child in node.children(&mut child_cursor) {
                        if child.kind() == rule.content_kind {
                            let byte_range = child.byte_range();
                            if byte_range.start < byte_range.end {
                                let range = tree_sitter::Range {
                                    start_byte: byte_range.start,
                                    end_byte: byte_range.end,
                                    start_point: child.start_position(),
                                    end_point: child.end_position(),
                                };

                                // Safe: row count fits u32 because MAX_FILE_SIZE (50MB)
                                // limits files to ~50M lines at minimum 1 byte/line,
                                // well within u32::MAX.
                                //
                                // content_scoped_lines: use the content child's line
                                // range instead of the container's. Required for PHP
                                // where container is `program` (entire file) but we
                                // only want to replace chunks within each individual
                                // `text` region.
                                let child_lines = if rule.content_scoped_lines {
                                    (
                                        child.start_position().row as u32 + 1,
                                        child.end_position().row as u32 + 1,
                                    )
                                } else {
                                    container_lines
                                };

                                entries.push((target, range, child_lines));
                            }
                        }
                    }
                }
            }
            // Don't descend into containers — skip to next sibling
            if !advance_cursor(&mut cursor) {
                return;
            }
            continue;
        }

        // Try to go deeper
        if cursor.goto_first_child() {
            continue;
        }
        // Advance to next sibling or walk up
        if !advance_cursor(&mut cursor) {
            return;
        }
    }
}

/// Build an inner tree-sitter parse tree for injection ranges.
/// Allocates a fresh `tree_sitter::Parser` per call. This is intentional:
/// `Parser::new()` is cheap (~32 bytes on stack), and parsers are not `Send`
/// so they can't be shared across rayon threads. The real allocation cost is
/// in `parser.parse()` which builds the syntax tree.
/// Returns `None` on any failure (with warnings logged).
fn build_injection_tree(
    language: Language,
    source: &str,
    ranges: &[tree_sitter::Range],
) -> Option<tree_sitter::Tree> {
    let grammar = language.try_grammar()?;
    let mut parser = tree_sitter::Parser::new();
    if let Err(e) = parser.set_language(&grammar) {
        tracing::warn!(
            error = ?e,
            %language,
            "Failed to set language for injection"
        );
        return None;
    }

    if let Err(e) = parser.set_included_ranges(ranges) {
        tracing::warn!(
            error = %e,
            %language,
            "Failed to set included ranges for injection"
        );
        return None;
    }

    let tree = parser.parse(source, None);
    if tree.is_none() {
        tracing::warn!(%language, "Injection parse returned None");
    }
    tree
}

impl Parser {
    /// Parse injected chunks from byte ranges using an inner language grammar.
    /// Creates a new tree-sitter parser, sets included ranges, parses the source,
    /// and extracts chunks using the inner language's query.
    /// Supports recursive injection: if the inner language has its own injection
    /// rules and `depth < MAX_INJECTION_DEPTH`, nested injections are processed.
    /// For example, PHP→HTML→JS requires depth 2.
    pub(crate) fn parse_injected_chunks(
        &self,
        source: &str,
        path: &Path,
        group: &InjectionGroup,
        depth: usize,
    ) -> Result<Vec<Chunk>, ParserError> {
        let inner_language = group.language;
        let _span = tracing::info_span!(
            "parse_injected_chunks",
            language = %inner_language,
            range_count = group.ranges.len(),
            depth = depth,
            path = %path.display()
        )
        .entered();

        let tree = match build_injection_tree(inner_language, source, &group.ranges) {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        let query = match self.get_query(inner_language) {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    language = %inner_language,
                    "Failed to get chunk query for injection language"
                );
                return Ok(vec![]);
            }
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

        let mut chunks = Vec::new();

        while let Some(m) = matches.next() {
            match self.extract_chunk(source, m, query, inner_language, path) {
                Ok(mut chunk) => {
                    // Skip oversized chunks
                    if chunk.content.len() > super::MAX_CHUNK_BYTES {
                        tracing::debug!(
                            id = %chunk.id,
                            bytes = chunk.content.len(),
                            "Skipping oversized injected chunk"
                        );
                        continue;
                    }

                    // Apply post-process hook
                    if let Some(post_process) = inner_language.def().post_process_chunk {
                        if let Some(node) = super::extract_definition_node(m, query) {
                            if !post_process(&mut chunk.name, &mut chunk.chunk_type, node, source) {
                                continue;
                            }
                        }
                    }

                    // Ensure language is set to the inner language
                    chunk.language = inner_language;
                    chunks.push(chunk);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        language = %inner_language,
                        "Failed to extract injected chunk"
                    );
                }
            }
        }

        if chunks.is_empty() {
            tracing::debug!(
                language = %inner_language,
                "Injection produced no chunks, keeping outer"
            );
        } else {
            tracing::debug!(
                language = %inner_language,
                count = chunks.len(),
                "Injection extracted chunks"
            );
        }

        // --- Recursive injection ---
        // If the inner language has its own injection rules (e.g., HTML has
        // script→JS/style→CSS), recurse to extract nested embedded chunks.
        let inner_rules = inner_language.def().injections;
        if !inner_rules.is_empty() && depth < MAX_INJECTION_DEPTH {
            let nested_groups = find_injection_ranges(&tree, source, inner_rules);
            if !nested_groups.is_empty() {
                let _nested_span = tracing::debug_span!(
                    "recursive_injection",
                    depth = depth + 1,
                    language = %inner_language,
                    groups = nested_groups.len()
                )
                .entered();

                for nested_group in &nested_groups {
                    let nested_chunks =
                        self.parse_injected_chunks(source, path, nested_group, depth + 1)?;
                    if !nested_chunks.is_empty() {
                        // Remove inner chunks that fall within nested containers
                        chunks.retain(|c| {
                            !chunk_within_container(
                                c.line_start,
                                c.line_end,
                                &nested_group.container_lines,
                            )
                        });
                        chunks.extend(nested_chunks);
                    }
                }
            }
        } else if !inner_rules.is_empty() {
            tracing::debug!(
                depth = depth,
                language = %inner_language,
                "Injection depth limit reached, skipping nested rules"
            );
        }

        Ok(chunks)
    }

    /// Parse injected relationships (calls + types) from byte ranges.
    /// Supports recursive injection via `depth` parameter.
    pub(crate) fn parse_injected_relationships(
        &self,
        source: &str,
        group: &InjectionGroup,
        depth: usize,
    ) -> Result<(Vec<FunctionCalls>, Vec<ChunkTypeRefs>), ParserError> {
        let inner_language = group.language;
        let _span = tracing::info_span!(
            "parse_injected_relationships",
            language = %inner_language,
            range_count = group.ranges.len()
        )
        .entered();

        let tree = match build_injection_tree(inner_language, source, &group.ranges) {
            Some(t) => t,
            None => return Ok((vec![], vec![])),
        };

        // Get queries
        let chunk_query = match self.get_query(inner_language) {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "No chunk query for injection language");
                return Ok((vec![], vec![]));
            }
        };

        // Call query is optional — some languages (e.g., CSS) don't define one.
        // Proceed with type extraction even if call query is unavailable.
        let call_query = match self.get_call_query(inner_language) {
            Ok(q) => Some(q),
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    language = %inner_language,
                    "No call query for injection language, skipping call extraction"
                );
                None
            }
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());

        let capture_names = chunk_query.capture_names();
        let name_idx = chunk_query.capture_index_for_name("name");
        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();

        while let Some(m) = matches.next() {
            // Find chunk node (same logic as parse_file_relationships)
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                capture_name_to_chunk_type(name).is_some()
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            // Get chunk name
            let mut name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            // Apply post-process hook
            if let Some(post_process) = inner_language.def().post_process_chunk {
                let cap_name = capture_names
                    .get(func_capture.index as usize)
                    .copied()
                    .unwrap_or("");
                let mut ct = capture_name_to_chunk_type(cap_name).unwrap_or(ChunkType::Function);
                if !post_process(&mut name, &mut ct, node, source) {
                    continue;
                }
            }

            let line_start = node.start_position().row as u32 + 1;
            let byte_range = node.byte_range();

            // --- Call extraction (if query available) ---
            if let Some(call_query) = call_query {
                call_cursor.set_byte_range(byte_range.clone());
                calls.clear();

                let mut call_matches =
                    call_cursor.matches(call_query, tree.root_node(), source.as_bytes());

                while let Some(cm) = call_matches.next() {
                    for cap in cm.captures {
                        let callee_name = source[cap.node.byte_range()].to_string();
                        let call_line = cap.node.start_position().row as u32 + 1;

                        if !super::calls::should_skip_callee(&callee_name) {
                            calls.push(super::types::CallSite {
                                callee_name,
                                line_number: call_line,
                            });
                        }
                    }
                }

                // Deduplicate calls
                seen.clear();
                calls.retain(|c| seen.insert(c.callee_name.clone()));

                if !calls.is_empty() {
                    call_results.push(FunctionCalls {
                        name: name.clone(),
                        line_start,
                        calls: std::mem::take(&mut calls),
                    });
                }
            }

            // --- Type extraction ---
            let mut type_refs = self.extract_types(
                source,
                &tree,
                inner_language,
                byte_range.start,
                byte_range.end,
            );

            type_refs.retain(|t| t.type_name != name);

            if !type_refs.is_empty() {
                type_results.push(ChunkTypeRefs {
                    name,
                    line_start,
                    type_refs,
                });
            }
        }

        tracing::debug!(
            language = %inner_language,
            calls = call_results.len(),
            types = type_results.len(),
            "Injection extracted relationships"
        );

        // --- Recursive injection for relationships ---
        let inner_rules = inner_language.def().injections;
        if !inner_rules.is_empty() && depth < MAX_INJECTION_DEPTH {
            let nested_groups = find_injection_ranges(&tree, source, inner_rules);
            for nested_group in &nested_groups {
                let (nested_calls, nested_types) =
                    self.parse_injected_relationships(source, nested_group, depth + 1)?;
                call_results.extend(nested_calls);
                type_results.extend(nested_types);
            }
        }

        Ok((call_results, type_results))
    }

    /// Combined injection: extract chunks + calls + types in a single inner parse.
    /// Builds one inner tree-sitter tree and runs two query cursor passes:
    /// 1. Chunk extraction (same as `parse_injected_chunks`)
    /// 2. Relationship extraction (same as `parse_injected_relationships`)
    /// Supports recursive injection via `depth` parameter.
    /// Used by `parse_file_all()` to avoid double-parsing injection regions.
    pub(crate) fn parse_injected_all(
        &self,
        source: &str,
        path: &Path,
        group: &InjectionGroup,
        depth: usize,
    ) -> Result<super::ParseAllResult, ParserError> {
        let inner_language = group.language;
        let _span = tracing::info_span!(
            "parse_injected_all",
            language = %inner_language,
            range_count = group.ranges.len(),
            path = %path.display()
        )
        .entered();

        let tree = match build_injection_tree(inner_language, source, &group.ranges) {
            Some(t) => t,
            None => return Ok((vec![], vec![], vec![])),
        };

        let chunk_query = match self.get_query(inner_language) {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    language = %inner_language,
                    "Failed to get chunk query for injection language"
                );
                return Ok((vec![], vec![], vec![]));
            }
        };

        // --- Pass 1: Chunk extraction ---
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());
        let mut chunks = Vec::new();

        while let Some(m) = matches.next() {
            match self.extract_chunk(source, m, chunk_query, inner_language, path) {
                Ok(mut chunk) => {
                    if chunk.content.len() > super::MAX_CHUNK_BYTES {
                        tracing::debug!(
                            id = %chunk.id,
                            bytes = chunk.content.len(),
                            "Skipping oversized injected chunk"
                        );
                        continue;
                    }
                    if let Some(post_process) = inner_language.def().post_process_chunk {
                        if let Some(node) = super::extract_definition_node(m, chunk_query) {
                            if !post_process(&mut chunk.name, &mut chunk.chunk_type, node, source) {
                                continue;
                            }
                        }
                    }
                    chunk.language = inner_language;
                    chunks.push(chunk);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        language = %inner_language,
                        "Failed to extract injected chunk"
                    );
                }
            }
        }

        if chunks.is_empty() {
            tracing::debug!(
                language = %inner_language,
                "Injection produced no chunks, keeping outer"
            );
        }

        // --- Pass 2: Relationship extraction (calls + types) ---
        let call_query = match self.get_call_query(inner_language) {
            Ok(q) => Some(q),
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    language = %inner_language,
                    "No call query for injection language, skipping call extraction"
                );
                None
            }
        };

        let mut cursor2 = tree_sitter::QueryCursor::new();
        let mut matches2 = cursor2.matches(chunk_query, tree.root_node(), source.as_bytes());

        let capture_names = chunk_query.capture_names();
        let name_idx = chunk_query.capture_index_for_name("name");
        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();

        while let Some(m) = matches2.next() {
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                capture_name_to_chunk_type(name).is_some()
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            let mut name = name_idx
                .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                .map(|c| source[c.node.byte_range()].to_string())
                .unwrap_or_else(|| "<anonymous>".to_string());

            if let Some(post_process) = inner_language.def().post_process_chunk {
                let cap_name = capture_names
                    .get(func_capture.index as usize)
                    .copied()
                    .unwrap_or("");
                let mut ct = capture_name_to_chunk_type(cap_name).unwrap_or(ChunkType::Function);
                if !post_process(&mut name, &mut ct, node, source) {
                    continue;
                }
            }

            let line_start = node.start_position().row as u32 + 1;
            let byte_range = node.byte_range();

            if let Some(cq) = call_query {
                call_cursor.set_byte_range(byte_range.clone());
                calls.clear();

                let mut call_matches = call_cursor.matches(cq, tree.root_node(), source.as_bytes());

                while let Some(cm) = call_matches.next() {
                    for cap in cm.captures {
                        let callee_name = source[cap.node.byte_range()].to_string();
                        let call_line = cap.node.start_position().row as u32 + 1;

                        if !super::calls::should_skip_callee(&callee_name) {
                            calls.push(super::types::CallSite {
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
            }

            let mut type_refs = self.extract_types(
                source,
                &tree,
                inner_language,
                byte_range.start,
                byte_range.end,
            );

            type_refs.retain(|t| t.type_name != name);

            if !type_refs.is_empty() {
                type_results.push(ChunkTypeRefs {
                    name,
                    line_start,
                    type_refs,
                });
            }
        }

        tracing::debug!(
            language = %inner_language,
            chunks = chunks.len(),
            calls = call_results.len(),
            types = type_results.len(),
            "Injection extracted all"
        );

        // --- Recursive injection ---
        let inner_rules = inner_language.def().injections;
        if !inner_rules.is_empty() && depth < MAX_INJECTION_DEPTH {
            let nested_groups = find_injection_ranges(&tree, source, inner_rules);
            if !nested_groups.is_empty() {
                let _nested_span = tracing::debug_span!(
                    "recursive_injection_all",
                    depth = depth + 1,
                    language = %inner_language,
                    groups = nested_groups.len()
                )
                .entered();

                for nested_group in &nested_groups {
                    let (nested_chunks, nested_calls, nested_types) =
                        self.parse_injected_all(source, path, nested_group, depth + 1)?;
                    if !nested_chunks.is_empty() {
                        chunks.retain(|c| {
                            !chunk_within_container(
                                c.line_start,
                                c.line_end,
                                &nested_group.container_lines,
                            )
                        });
                        chunks.extend(nested_chunks);
                    }
                    call_results.extend(nested_calls);
                    type_results.extend(nested_types);
                }
            }
        } else if !inner_rules.is_empty() {
            tracing::debug!(
                depth = depth,
                language = %inner_language,
                "Injection depth limit reached, skipping nested rules"
            );
        }

        Ok((chunks, call_results, type_results))
    }
}

/// Check if an outer chunk is fully contained within any injection container.
/// Returns `true` if the chunk's line range `[chunk_start, chunk_end]` is
/// entirely within some container's `[start, end]`. This is strict containment,
/// not overlap — a chunk that partially overlaps a container is NOT matched.
/// Used to identify outer chunks (e.g., HTML Module chunks for script/style)
/// that should be replaced by inner chunks when injection parsing succeeds.
pub(crate) fn chunk_within_container(
    chunk_start: u32,
    chunk_end: u32,
    container_lines: &[(u32, u32)],
) -> bool {
    container_lines
        .iter()
        .any(|&(start, end)| chunk_start >= start && chunk_end <= end)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod chunk_within_container_tests {
        use super::*;
        /// Tests that a chunk fully contained within a container range is correctly identified.
        /// # Arguments
        /// This function takes no arguments. It tests the internal logic of `chunk_within_container`.
        /// # Returns
        /// Returns nothing. This is a test function that asserts expected behavior.
        /// # Panics
        /// Panics if the assertion fails, indicating that `chunk_within_container` did not correctly identify that lines 5-10 are fully contained within the container range 3-15.

        #[test]
        fn fully_contained() {
            // Chunk lines 5-10 inside container 3-15
            assert!(chunk_within_container(5, 10, &[(3, 15)]));
        }
        /// Tests that a chunk is correctly identified as being within a container when the chunk boundaries exactly match the container boundaries.
        /// # Arguments
        /// This is a test function with no parameters.
        /// # Returns
        /// Returns nothing. Panics if the assertion fails.
        /// # Panics
        /// Panics if `chunk_within_container(3, 15, &[(3, 15)])` returns false, indicating the function failed to recognize an exact boundary match.

        #[test]
        fn exact_match() {
            // Chunk exactly matches container boundaries
            assert!(chunk_within_container(3, 15, &[(3, 15)]));
        }
        /// Verifies that a chunk positioned at the exact start boundary of a container is correctly identified as being within that container.
        /// # Arguments
        /// None. This is a test helper function that uses hardcoded values.
        /// # Panics
        /// Panics if the assertion fails, indicating that a chunk at position 3 with size 10 is not recognized as being within the container range (3, 15).

        #[test]
        fn start_boundary() {
            // Chunk starts at container start
            assert!(chunk_within_container(3, 10, &[(3, 15)]));
        }
        /// Verifies that a chunk is correctly identified as being within a container when the chunk's end boundary aligns with the container's end boundary.
        /// # Arguments
        /// No parameters. This is a test assertion function.
        /// # Panics
        /// Panics if the assertion fails, indicating that a chunk from position 10 to 15 is not correctly recognized as being within a container spanning from position 3 to 15.

        #[test]
        fn end_boundary() {
            // Chunk ends at container end
            assert!(chunk_within_container(10, 15, &[(3, 15)]));
        }
        /// Tests that a chunk positioned entirely before a container is correctly identified as not contained within it.
        /// # Arguments
        /// * `1` - chunk start position
        /// * `2` - chunk end position
        /// * `&[(3, 15)]` - container ranges to check against
        /// # Returns
        /// Asserts that `chunk_within_container` returns `false` when the chunk (1-2) ends before the container (3-15) begins.

        #[test]
        fn not_contained_before() {
            // Chunk entirely before container
            assert!(!chunk_within_container(1, 2, &[(3, 15)]));
        }
        /// Tests that a chunk positioned entirely after a container is correctly identified as not contained within it.
        /// # Arguments
        /// This is a test function with no parameters.
        /// # Returns
        /// This test function returns nothing. It asserts that `chunk_within_container(16, 20, &[(3, 15)])` returns `false`, indicating that a chunk from position 16-20 is not contained within a container spanning positions 3-15.

        #[test]
        fn not_contained_after() {
            // Chunk entirely after container
            assert!(!chunk_within_container(16, 20, &[(3, 15)]));
        }
        /// Verifies that a chunk is not considered contained within a container when the chunk's start position precedes the container's start position, even if their ranges overlap.
        /// # Arguments
        /// * `1` - The start position of the chunk
        /// * `5` - The end position of the chunk
        /// * `&[(3, 15)]` - A slice containing one container with start position 3 and end position 15
        /// # Returns
        /// Asserts that `chunk_within_container` returns `false`, indicating the chunk (1-5) is not strictly contained within the container (3-15).

        #[test]
        fn partial_overlap_start() {
            // Chunk starts before container — NOT contained (strict containment)
            assert!(!chunk_within_container(1, 5, &[(3, 15)]));
        }
        /// Verifies that a chunk is not considered contained within a container when the chunk partially overlaps the end of a container range.
        /// # Arguments
        /// This function takes no arguments. It is a test that internally uses hardcoded values:
        /// - Chunk start position: 10
        /// - Chunk end position: 20
        /// - Container range: (3, 15)
        /// # Returns
        /// This function returns nothing. It asserts that `chunk_within_container(10, 20, &[(3, 15)])` returns `false`.
        /// # Panics
        /// Panics if the assertion fails, indicating the `chunk_within_container` function incorrectly identified a partially overlapping chunk as contained.

        #[test]
        fn partial_overlap_end() {
            // Chunk ends after container — NOT contained
            assert!(!chunk_within_container(10, 20, &[(3, 15)]));
        }
        /// Tests that `chunk_within_container` returns `false` when given an empty container list.
        /// # Arguments
        /// * `chunk_within_container` - The function being tested
        /// * `5` - A chunk identifier
        /// * `10` - A size or offset value
        /// * `&[]` - An empty slice of containers
        /// # Returns
        /// `false` - Confirms that no chunk can exist within an empty container collection

        #[test]
        fn empty_containers() {
            assert!(!chunk_within_container(5, 10, &[]));
        }
        /// Verifies that the `chunk_within_container` function correctly identifies whether a chunk range is contained within any of multiple containers.
        /// # Arguments
        /// This is a test function with no parameters. It uses hardcoded test data including:
        /// - A vector of container ranges as tuples of (start, end)
        /// - Chunk ranges to test against the containers
        /// # Returns
        /// Returns nothing. Assertions validate that chunks fully contained within a container return true, and chunks not contained in any container return false.

        #[test]
        fn multiple_containers() {
            // Second container matches
            let containers = vec![(1, 3), (10, 20), (30, 40)];
            assert!(chunk_within_container(12, 18, &containers));
            // Not in any container
            assert!(!chunk_within_container(5, 8, &containers));
        }
        /// This function tests the `chunk_within_container` function's behavior when checking if a single-line chunk (where start equals end) falls within a container's line range. It verifies that a chunk at line 5 is correctly identified as within a container spanning lines 3-15, and that a chunk at line 2 is correctly identified as outside that range.
        /// # Arguments
        /// None
        /// # Returns
        /// None (test function that uses assertions)
        /// # Panics
        /// Panics if either assertion fails, indicating the `chunk_within_container` function does not correctly handle single-line chunks.

        #[test]
        fn single_line_chunk() {
            // start == end (e.g., FunctionCalls with only line_start)
            assert!(chunk_within_container(5, 5, &[(3, 15)]));
            assert!(!chunk_within_container(2, 2, &[(3, 15)]));
        }
    }
}
