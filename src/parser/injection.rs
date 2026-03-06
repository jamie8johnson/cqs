//! Multi-grammar injection parsing
//!
//! Implements two-phase parsing for files containing embedded languages:
//! 1. Parse the outer grammar (e.g., HTML)
//! 2. Find injection regions (e.g., `<script>`, `<style>`)
//! 3. Re-parse those regions with inner grammars (e.g., JavaScript, CSS)
//!
//! Uses tree-sitter's `set_included_ranges()` for byte-accurate inner parsing.
//!
//! **Limitation:** Injection is single-level only. Inner languages are not
//! checked for their own injection rules (e.g., PHP→HTML→JS would require
//! recursive injection, which is not yet implemented).

use std::path::Path;

use tree_sitter::StreamingIterator;

use super::types::{
    capture_name_to_chunk_type, Chunk, ChunkType, ChunkTypeRefs, FunctionCalls, Language,
    ParserError, CHUNK_CAPTURE_NAMES,
};
use super::Parser;
use crate::language::InjectionRule;

/// Maximum number of injection ranges per file. Prevents OOM from crafted
/// files with millions of tiny injection containers (e.g., `<script>` blocks).
const MAX_INJECTION_RANGES: usize = 1000;

/// Result of scanning an outer tree for injection regions.
///
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
///
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
    let mut cursor = root.walk();

    for rule in rules {
        // Walk the tree to find container nodes
        cursor.reset(root);
        walk_for_containers(&mut cursor, rule, source, &mut entries);
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

    // Group by language name
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

        // Find existing group or create new one
        if let Some(group) = groups.iter_mut().find(|g| g.language == language) {
            group.ranges.push(range);
            group.container_lines.push(lines);
        } else {
            groups.push(InjectionGroup {
                language,
                ranges: vec![range],
                container_lines: vec![lines],
            });
        }
    }

    groups
}

/// Walk the tree using a cursor to find all nodes matching an injection rule's container_kind.
fn walk_for_containers(
    cursor: &mut tree_sitter::TreeCursor,
    rule: &InjectionRule,
    source: &str,
    entries: &mut Vec<(&str, tree_sitter::Range, (u32, u32))>,
) {
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
            if target == "_skip" {
                // Fall through to the sibling-advance logic below
            } else {
                // Collect ALL matching content children (error recovery may split
                // raw_text into multiple nodes)
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

                            let container_lines = (
                                node.start_position().row as u32 + 1,
                                node.end_position().row as u32 + 1,
                            );

                            entries.push((target, range, container_lines));
                        }
                    }
                }
            }
            // Don't descend into containers — skip to next sibling
            if !cursor.goto_next_sibling() {
                // Walk back up to find more siblings
                loop {
                    if !cursor.goto_parent() {
                        return;
                    }
                    if cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            continue;
        }

        // Try to go deeper
        if cursor.goto_first_child() {
            continue;
        }
        // Try next sibling
        if cursor.goto_next_sibling() {
            continue;
        }
        // Walk back up
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Build an inner tree-sitter parse tree for injection ranges.
///
/// Returns `None` on any failure (with warnings logged).
fn build_injection_tree(
    language: Language,
    source: &str,
    ranges: &[tree_sitter::Range],
) -> Option<tree_sitter::Tree> {
    let grammar = language.grammar();
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
    ///
    /// Creates a new tree-sitter parser, sets included ranges, parses the source,
    /// and extracts chunks using the inner language's query.
    pub(crate) fn parse_injected_chunks(
        &self,
        source: &str,
        path: &Path,
        group: &InjectionGroup,
    ) -> Result<Vec<Chunk>, ParserError> {
        let inner_language = group.language;
        let _span = tracing::info_span!(
            "parse_injected_chunks",
            language = %inner_language,
            range_count = group.ranges.len()
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
        }

        Ok(chunks)
    }

    /// Parse injected relationships (calls + types) from byte ranges.
    pub(crate) fn parse_injected_relationships(
        &self,
        source: &str,
        group: &InjectionGroup,
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
        let mut call_results = Vec::new();
        let mut type_results = Vec::new();
        let mut call_cursor = tree_sitter::QueryCursor::new();
        let mut calls = Vec::new();
        let mut seen = std::collections::HashSet::new();

        while let Some(m) = matches.next() {
            // Find chunk node (same logic as parse_file_relationships)
            let func_node = m.captures.iter().find(|c| {
                let name = capture_names.get(c.index as usize).copied().unwrap_or("");
                CHUNK_CAPTURE_NAMES.contains(&name)
            });

            let Some(func_capture) = func_node else {
                continue;
            };

            let node = func_capture.node;

            // Get chunk name
            let name_idx = chunk_query.capture_index_for_name("name");
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

        Ok((call_results, type_results))
    }
}

/// Check if an outer chunk is fully contained within any injection container.
///
/// Returns `true` if the chunk's line range `[chunk_start, chunk_end]` is
/// entirely within some container's `[start, end]`. This is strict containment,
/// not overlap — a chunk that partially overlaps a container is NOT matched.
///
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

        #[test]
        fn fully_contained() {
            // Chunk lines 5-10 inside container 3-15
            assert!(chunk_within_container(5, 10, &[(3, 15)]));
        }

        #[test]
        fn exact_match() {
            // Chunk exactly matches container boundaries
            assert!(chunk_within_container(3, 15, &[(3, 15)]));
        }

        #[test]
        fn start_boundary() {
            // Chunk starts at container start
            assert!(chunk_within_container(3, 10, &[(3, 15)]));
        }

        #[test]
        fn end_boundary() {
            // Chunk ends at container end
            assert!(chunk_within_container(10, 15, &[(3, 15)]));
        }

        #[test]
        fn not_contained_before() {
            // Chunk entirely before container
            assert!(!chunk_within_container(1, 2, &[(3, 15)]));
        }

        #[test]
        fn not_contained_after() {
            // Chunk entirely after container
            assert!(!chunk_within_container(16, 20, &[(3, 15)]));
        }

        #[test]
        fn partial_overlap_start() {
            // Chunk starts before container — NOT contained (strict containment)
            assert!(!chunk_within_container(1, 5, &[(3, 15)]));
        }

        #[test]
        fn partial_overlap_end() {
            // Chunk ends after container — NOT contained
            assert!(!chunk_within_container(10, 20, &[(3, 15)]));
        }

        #[test]
        fn empty_containers() {
            assert!(!chunk_within_container(5, 10, &[]));
        }

        #[test]
        fn multiple_containers() {
            // Second container matches
            let containers = vec![(1, 3), (10, 20), (30, 40)];
            assert!(chunk_within_container(12, 18, &containers));
            // Not in any container
            assert!(!chunk_within_container(5, 8, &containers));
        }

        #[test]
        fn single_line_chunk() {
            // start == end (e.g., FunctionCalls with only line_start)
            assert!(chunk_within_container(5, 5, &[(3, 15)]));
            assert!(!chunk_within_container(2, 2, &[(3, 15)]));
        }
    }
}
