//! ASP.NET Web Forms parser — custom parser for .aspx, .ascx, .asmx, .master files
//!
//! No tree-sitter grammar exists for Web Forms files. This parser manually scans
//! the source for server-side code regions, then delegates to the C# or VB.NET
//! tree-sitter grammar via `set_included_ranges()`.
//!
//! Web Forms files contain:
//! - `<%@ Page Language="VB" %>` / `<%@ Page Language="C#" %>` directives
//! - `<script runat="server">...</script>` blocks (compiled server code)
//! - `<% code %>` inline code blocks
//! - `<%= expression %>` and `<%: encoded_expression %>` expression blocks

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::StreamingIterator;

use super::types::{Chunk, ChunkTypeRefs, FunctionCalls, Language, ParserError, TypeRef};
use super::ParseAllResult;
use super::Parser;

// ---------------------------------------------------------------------------
// Pre-compiled regexes
// ---------------------------------------------------------------------------

/// Match the `<%@ ... Language="VB" ... %>` or `<%@ ... Language="C#" ... %>` directive.
/// Captures the language value (case-insensitive).
static DIRECTIVE_LANG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<%@[^%]*Language\s*=\s*"([^"]+)""#).expect("valid regex"));

/// Match `<script runat="server">...</script>` blocks (single-line or multi-line).
/// Uses a non-greedy match to handle multiple script blocks in one file.
/// Group 1: content inside the script element (everything between > and </script>).
static SCRIPT_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<script[^>]*\brunat\s*=\s*["']server["'][^>]*>(.*?)</script\s*>"#)
        .expect("valid regex")
});

/// Match all `<% ... %>` blocks. Directives (`<%@`) and comments (`<%--`)
/// are filtered out in `find_code_blocks()` after matching.
/// Group 1: optional prefix (`=`, `:`, `@`, or `--`).
/// Group 2: the content.
static CODE_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<%(=|:|@|--|--)?(.*?)(--%>|%>)"#).expect("valid regex"));

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// A region of server-side code within the ASPX source.
#[derive(Debug, Clone)]
struct CodeRegion {
    /// Byte offset where this region starts in the source
    start_byte: usize,
    /// Byte offset where this region ends (exclusive) in the source
    end_byte: usize,
    /// 0-indexed row (line number) of the start
    start_row: usize,
    /// 0-indexed column of the start byte
    start_col: usize,
    /// 0-indexed row (line number) of the end
    end_row: usize,
    /// 0-indexed column of the end byte
    end_col: usize,
}

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

/// Detect the server-side language from the ASPX `<%@ ... %>` directive.
/// Scans for `Language="VB"` (case-insensitive). Returns `Language::VbNet`
/// for VB, `Language::CSharp` for C# (the default when not found or
/// when the value is "C#", "csharp", etc.).
pub fn detect_language(source: &str) -> Language {
    if let Some(cap) = DIRECTIVE_LANG_RE.captures(source) {
        let lang_val = cap.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        if lang_val.eq_ignore_ascii_case("vb")
            || lang_val.eq_ignore_ascii_case("vbnet")
            || lang_val.eq_ignore_ascii_case("vb.net")
        {
            return Language::VbNet;
        }
    }
    Language::CSharp
}

// ---------------------------------------------------------------------------
// Region discovery
// ---------------------------------------------------------------------------

/// Calculate the 0-indexed (row, col) for a byte offset in source text.
fn byte_to_point(source: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(source.len());
    let byte = source.floor_char_boundary(byte);
    let before = &source[..byte];
    let row = before.bytes().filter(|&b| b == b'\n').count();
    let col = before.len() - before.rfind('\n').map(|p| p + 1).unwrap_or(0);
    (row, col)
}

/// Find all `<script runat="server">...</script>` regions.
/// Returns one `CodeRegion` per script block, covering the content bytes
/// inside the element (between the closing `>` and opening `</`).
fn find_server_script_blocks(source: &str) -> Vec<CodeRegion> {
    let mut regions = Vec::new();

    for cap in SCRIPT_BLOCK_RE.captures_iter(source) {
        // cap.get(0) = full match, cap.get(1) = content inside the tags
        let content_match = match cap.get(1) {
            Some(m) => m,
            None => continue,
        };

        let start_byte = content_match.start();
        let end_byte = content_match.end();

        if start_byte >= end_byte {
            continue;
        }

        let (start_row, start_col) = byte_to_point(source, start_byte);
        let (end_row, end_col) = byte_to_point(source, end_byte);

        regions.push(CodeRegion {
            start_byte,
            end_byte,
            start_row,
            start_col,
            end_row,
            end_col,
        });
    }

    regions
}

/// Find all inline code blocks: `<% %>`, `<%= %>`, `<%: %>`.
/// Excludes directives (`<%@ %>`), comments (`<%-- --%>`), and empty blocks.
fn find_code_blocks(source: &str) -> Vec<CodeRegion> {
    let mut regions = Vec::new();

    for cap in CODE_BLOCK_RE.captures_iter(source) {
        // Skip directives (<%@ ... %>) and comments (<%-- ... --%>)
        if let Some(prefix) = cap.get(1) {
            let p = prefix.as_str();
            if p == "@" || p == "--" {
                continue;
            }
        }

        // cap.get(2) = the code content (group 2)
        let content_match = match cap.get(2) {
            Some(m) => m,
            None => continue,
        };

        let start_byte = content_match.start();
        let end_byte = content_match.end();

        // Skip empty or whitespace-only blocks
        if start_byte >= end_byte || source[start_byte..end_byte].trim().is_empty() {
            continue;
        }

        let (start_row, start_col) = byte_to_point(source, start_byte);
        let (end_row, end_col) = byte_to_point(source, end_byte);

        regions.push(CodeRegion {
            start_byte,
            end_byte,
            start_row,
            start_col,
            end_row,
            end_col,
        });
    }

    regions
}

// ---------------------------------------------------------------------------
// Server-side code parsing
// ---------------------------------------------------------------------------

/// Parse server-side code regions using the appropriate tree-sitter grammar.
/// Uses `set_included_ranges()` to tell tree-sitter which byte ranges within
/// the full source contain valid code. This means line/column numbers in
/// extracted chunks refer to positions in the original ASPX file.
/// Returns extracted chunks. Falls back to an empty vec on parse failure
/// (with a warning logged) rather than propagating an error — ASPX files
/// with syntactically invalid server code should still yield HTML chunks.
fn parse_server_code(
    source: &str,
    path: &Path,
    regions: &[CodeRegion],
    language: Language,
    cqs_parser: &Parser,
) -> Vec<Chunk> {
    if regions.is_empty() {
        return vec![];
    }

    // Build tree-sitter ranges from our CodeRegions
    let ts_ranges: Vec<tree_sitter::Range> = regions
        .iter()
        .map(|r| tree_sitter::Range {
            start_byte: r.start_byte,
            end_byte: r.end_byte,
            start_point: tree_sitter::Point {
                row: r.start_row,
                column: r.start_col,
            },
            end_point: tree_sitter::Point {
                row: r.end_row,
                column: r.end_col,
            },
        })
        .collect();

    // Get the grammar — if the language feature is disabled, skip gracefully
    let grammar = match language.try_def().and_then(|d| d.grammar) {
        Some(grammar_fn) => grammar_fn(),
        None => {
            tracing::warn!(
                %language,
                "Language not available (feature disabled?), skipping server code parse"
            );
            return vec![];
        }
    };

    let mut ts_parser = tree_sitter::Parser::new();
    if let Err(e) = ts_parser.set_language(&grammar) {
        tracing::warn!(error = ?e, %language, "Failed to set tree-sitter language for ASPX server code");
        return vec![];
    }

    if let Err(e) = ts_parser.set_included_ranges(&ts_ranges) {
        tracing::warn!(error = %e, %language, "Failed to set included ranges for ASPX server code");
        return vec![];
    }

    let tree = match ts_parser.parse(source, None) {
        Some(t) => t,
        None => {
            tracing::warn!(%language, path = %path.display(), "ASPX server code parse returned None");
            return vec![];
        }
    };

    let query = match cqs_parser.get_query(language) {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!(error = %e, %language, "Failed to get chunk query for ASPX server code");
            return vec![];
        }
    };

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());

    let mut chunks = Vec::new();
    // P3 #105: env-overridable per-chunk byte cap.
    let max_chunk_bytes = crate::limits::parser_max_chunk_bytes();

    while let Some(m) = matches.next() {
        match cqs_parser.extract_chunk(source, m, query, language, path) {
            Ok(mut chunk) => {
                if chunk.content.len() > max_chunk_bytes {
                    tracing::debug!(
                        id = %chunk.id,
                        bytes = chunk.content.len(),
                        "Skipping oversized ASPX server-code chunk"
                    );
                    continue;
                }
                chunk.language = language;
                chunks.push(chunk);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    %language,
                    "Failed to extract chunk from ASPX server code"
                );
            }
        }
    }

    tracing::debug!(
        %language,
        count = chunks.len(),
        path = %path.display(),
        "ASPX server code extraction complete"
    );

    chunks
}

/// Parse server-side code regions and extract function calls.
/// Runs the call query over the same `set_included_ranges()` tree used
/// for chunk extraction. Returns `FunctionCalls` grouped by chunk.
fn parse_server_code_calls(
    source: &str,
    regions: &[CodeRegion],
    language: Language,
    cqs_parser: &Parser,
) -> Vec<FunctionCalls> {
    if regions.is_empty() {
        return vec![];
    }

    let ts_ranges: Vec<tree_sitter::Range> = regions
        .iter()
        .map(|r| tree_sitter::Range {
            start_byte: r.start_byte,
            end_byte: r.end_byte,
            start_point: tree_sitter::Point {
                row: r.start_row,
                column: r.start_col,
            },
            end_point: tree_sitter::Point {
                row: r.end_row,
                column: r.end_col,
            },
        })
        .collect();

    // v1.22.0 audit EH-5: mirror the sibling `parse_server_code`'s warn
    // calls. The old code silently returned vec![] on every failure point,
    // making missing call-graph data for ASPX server code invisible.
    let grammar = match language.try_def().and_then(|d| d.grammar) {
        Some(grammar_fn) => grammar_fn(),
        None => {
            tracing::warn!(%language, "No grammar for ASPX server language (calls), skipping");
            return vec![];
        }
    };

    let mut ts_parser = tree_sitter::Parser::new();
    if ts_parser.set_language(&grammar).is_err() {
        tracing::warn!(%language, "Failed to set language for ASPX call parser");
        return vec![];
    }
    if ts_parser.set_included_ranges(&ts_ranges).is_err() {
        tracing::warn!(%language, "Failed to set included ranges for ASPX call parser");
        return vec![];
    }

    let tree = match ts_parser.parse(source, None) {
        Some(t) => t,
        None => {
            tracing::warn!(%language, "ASPX call parser returned no tree");
            return vec![];
        }
    };

    let chunk_query = match cqs_parser.get_query(language) {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!(%language, error = %e, "No chunk query for ASPX server language (calls)");
            return vec![];
        }
    };

    let call_query = match cqs_parser.get_call_query(language) {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!(%language, error = %e, "No call query for ASPX server language");
            return vec![];
        }
    };

    let capture_names = chunk_query.capture_names();
    let name_idx = chunk_query.capture_index_for_name("name");

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(chunk_query, tree.root_node(), source.as_bytes());

    let mut call_cursor = tree_sitter::QueryCursor::new();
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    while let Some(m) = matches.next() {
        // Find the definition capture
        let func_node = m.captures.iter().find(|c| {
            let name = capture_names.get(c.index as usize).copied().unwrap_or("");
            super::types::capture_name_to_chunk_type(name).is_some()
        });

        let Some(func_capture) = func_node else {
            continue;
        };

        let node = func_capture.node;
        let func_name = name_idx
            .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
            .map(|c| source[c.node.byte_range()].to_string())
            .unwrap_or_else(|| "<anonymous>".to_string());

        let line_start = node.start_position().row as u32 + 1;
        let byte_range = node.byte_range();

        call_cursor.set_byte_range(byte_range);

        let mut calls = Vec::new();
        let mut call_matches = call_cursor.matches(call_query, tree.root_node(), source.as_bytes());

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
            results.push(FunctionCalls {
                name: func_name,
                line_start,
                calls,
            });
        }
    }

    results
}

/// Parse server-side code regions and extract type references.
/// Returns a flat `Vec<TypeRef>` (not grouped by chunk) — suitable for
/// the custom-parser pattern used in ASPX where we don't need per-chunk
/// type tracking.
fn parse_server_code_types(
    source: &str,
    regions: &[CodeRegion],
    language: Language,
    cqs_parser: &Parser,
) -> Vec<TypeRef> {
    if regions.is_empty() {
        return vec![];
    }

    let ts_ranges: Vec<tree_sitter::Range> = regions
        .iter()
        .map(|r| tree_sitter::Range {
            start_byte: r.start_byte,
            end_byte: r.end_byte,
            start_point: tree_sitter::Point {
                row: r.start_row,
                column: r.start_col,
            },
            end_point: tree_sitter::Point {
                row: r.end_row,
                column: r.end_col,
            },
        })
        .collect();

    let grammar = match language.try_def().and_then(|d| d.grammar) {
        Some(grammar_fn) => grammar_fn(),
        None => {
            tracing::warn!(%language, "No grammar for ASPX server language (types), skipping");
            return vec![];
        }
    };

    let mut ts_parser = tree_sitter::Parser::new();
    if ts_parser.set_language(&grammar).is_err() {
        tracing::warn!(%language, "Failed to set language for ASPX type parser");
        return vec![];
    }
    if ts_parser.set_included_ranges(&ts_ranges).is_err() {
        tracing::warn!(%language, "Failed to set included ranges for ASPX type parser");
        return vec![];
    }

    let tree = match ts_parser.parse(source, None) {
        Some(t) => t,
        None => {
            tracing::warn!(%language, "ASPX type parser returned no tree");
            return vec![];
        }
    };

    // Extract types across all regions using the full byte span of each region
    let mut all_type_refs = Vec::new();
    for region in regions {
        let type_refs =
            cqs_parser.extract_types(source, &tree, language, region.start_byte, region.end_byte);
        all_type_refs.extend(type_refs);
    }

    all_type_refs
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse an ASPX/ASCX/ASMX/Master file into chunks.
/// Detects the server-side language from the `<%@ ... Language="..." %>` directive
/// (defaulting to C#), then uses `set_included_ranges()` to parse server-side code
/// with the appropriate tree-sitter grammar. Returns extracted chunks.
/// HTML content outside server blocks is not currently chunked (tree-sitter HTML
/// injection would go here if needed in the future).
pub fn parse_aspx_chunks(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<Vec<Chunk>, ParserError> {
    let _span = tracing::debug_span!("parse_aspx_chunks", path = %path.display()).entered();

    let language = detect_language(source);
    tracing::debug!(%language, "ASPX detected language");

    // Collect all server-side code regions
    let mut regions = find_server_script_blocks(source);
    regions.extend(find_code_blocks(source));

    // Sort by start byte so set_included_ranges receives them in order
    regions.sort_by_key(|r| r.start_byte);

    let chunks = parse_server_code(source, path, &regions, language, parser);

    Ok(chunks)
}

/// Parse an ASPX/ASCX/ASMX/Master file into chunks, calls, and type refs.
/// Combines chunk extraction, call graph construction, and type reference
/// extraction in a single parser pass (one tree-sitter parse per language).
pub fn parse_aspx_all(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<ParseAllResult, ParserError> {
    let _span = tracing::debug_span!("parse_aspx_all", path = %path.display()).entered();

    let language = detect_language(source);
    tracing::debug!(%language, "ASPX detected language");

    let mut regions = find_server_script_blocks(source);
    regions.extend(find_code_blocks(source));
    regions.sort_by_key(|r| r.start_byte);

    let chunks = parse_server_code(source, path, &regions, language, parser);
    let calls = parse_server_code_calls(source, &regions, language, parser);
    let flat_types = parse_server_code_types(source, &regions, language, parser);

    // Group flat TypeRefs by chunk (matching how parse_file_all does it)
    let mut chunk_types = Vec::new();
    for chunk in &chunks {
        let mut refs: Vec<TypeRef> = flat_types
            .iter()
            .filter(|t| {
                let line = t.line_number;
                line >= chunk.line_start && line <= chunk.line_end
            })
            .cloned()
            .collect();
        refs.retain(|t| t.type_name != chunk.name);
        if !refs.is_empty() {
            chunk_types.push(ChunkTypeRefs {
                name: chunk.name.clone(),
                line_start: chunk.line_start,
                type_refs: refs,
            });
        }
    }

    Ok((chunks, calls, chunk_types))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Creates a temporary file with the specified content and extension.
    /// # Arguments
    /// * `content` - The string content to write to the temporary file
    /// * `ext` - The file extension (without leading dot) to append to the temporary filename
    /// # Returns
    /// A `NamedTempFile` object representing the created temporary file with the content written and flushed to disk.
    /// # Panics
    /// Panics if the temporary file cannot be created or if writing/flushing the content fails.
    fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    // -------------------------------------------------------------------------
    // detect_language tests
    // -------------------------------------------------------------------------
    /// Tests that the detect_language function correctly identifies C# when given a C# ASP.NET Page directive.
    /// # Arguments
    /// None. This is a test function that uses a hardcoded C# ASP.NET source string.
    /// # Returns
    /// Nothing. This function asserts that detect_language returns Language::CSharp for the test input.
    /// # Panics
    /// Panics if the assertion fails, indicating that detect_language did not correctly identify the source as C#.

    #[test]
    fn detect_csharp_explicit() {
        let source = r#"<%@ Page Language="C#" AutoEventWireup="true" %>"#;
        assert_eq!(detect_language(source), Language::CSharp);
    }
    /// Verifies that the language detection function correctly identifies Visual Basic .NET source code based on the VB language directive in an ASP.NET Page declaration.
    /// # Arguments
    /// None. This is a test function that uses hardcoded source code.
    /// # Returns
    /// Nothing. This function asserts that `detect_language()` returns `Language::VbNet` for the given ASP.NET page source code containing a VB language directive.
    /// # Panics
    /// Panics if the assertion fails, indicating that the language detection did not correctly identify the source as Visual Basic .NET.

    #[test]
    fn detect_vb_explicit() {
        let source = r#"<%@ Page Language="VB" AutoEventWireup="false" %>"#;
        assert_eq!(detect_language(source), Language::VbNet);
    }
    /// Tests that the language detection correctly identifies Visual Basic .NET from a Page directive with case-insensitive language attribute.
    /// # Arguments
    /// None. This is a unit test function that uses hardcoded test data.
    /// # Returns
    /// None. This function asserts the result of language detection and returns nothing.
    /// # Panics
    /// Panics if the detected language is not `Language::VbNet`, indicating the language detection failed to properly handle case-insensitive VB language attributes.

    #[test]
    fn detect_vb_case_insensitive() {
        let source = r#"<%@ Page Language="vb" %>"#;
        assert_eq!(detect_language(source), Language::VbNet);
    }
    /// Verifies that the language detection defaults to CSharp when processing HTML content without any language directive.
    /// # Arguments
    /// This function takes no parameters. It uses a hardcoded HTML string as test input.
    /// # Returns
    /// This function returns nothing. It is a test function that asserts the expected behavior.
    /// # Panics
    /// Panics if `detect_language()` does not return `Language::CSharp` for HTML input without a language directive.

    #[test]
    fn detect_default_to_csharp_when_no_directive() {
        let source = r#"<html><body><h1>Hello</h1></body></html>"#;
        assert_eq!(detect_language(source), Language::CSharp);
    }
    /// Verifies that the language detection defaults to C# when an unknown or unsupported language is specified in ASP.NET page directives.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Returns
    /// This function returns nothing (unit type). It asserts that when `detect_language` is called with an ASP.NET Page directive containing an unknown language identifier ("COBOL"), the result is `Language::CSharp`.
    /// # Panics
    /// Panics if the assertion fails, indicating that unknown languages are not defaulting to C# as expected.

    #[test]
    fn detect_default_to_csharp_when_unknown_language() {
        let source = r#"<%@ Page Language="COBOL" %>"#;
        // Anything other than VB variants defaults to C#
        assert_eq!(detect_language(source), Language::CSharp);
    }
    /// Verifies that the `detect_language` function correctly identifies VB.NET as the language when parsing an ASP.NET control directive.
    /// This is a unit test that validates language detection for a control directive containing a Language attribute set to "VB".
    /// # Arguments
    /// None. This is a test function that operates on a hardcoded ASP.NET control directive string.
    /// # Returns
    /// None. This function asserts the correctness of language detection and panics if the assertion fails.
    /// # Panics
    /// Panics if `detect_language` does not return `Language::VbNet` for the given control directive source code.

    #[test]
    fn detect_control_directive() {
        let source = r#"<%@ Control Language="VB" ClassName="MyControl" %>"#;
        assert_eq!(detect_language(source), Language::VbNet);
    }

    // -------------------------------------------------------------------------
    // find_server_script_blocks tests
    // -------------------------------------------------------------------------
    /// Verifies that `find_server_script_blocks` correctly identifies a single server-side script block in HTML source code.
    /// This test function parses HTML containing one `<script runat="server">` block and asserts that exactly one script region is found. It validates that the identified region's byte range correctly points to the script content between the opening and closing tags.
    /// # Arguments
    /// None. This is a test function that uses hardcoded HTML source.
    /// # Returns
    /// None. Returns `()`. This function performs assertions to validate behavior.
    /// # Panics
    /// Panics if any assertion fails:
    /// - If the number of found regions is not exactly 1
    /// - If the region's content does not contain the expected script code ("void Page_Load")

    #[test]
    fn find_single_script_block() {
        let source = r#"<html>
<script runat="server">
void Page_Load() { }
</script>
</html>"#;
        let regions = find_server_script_blocks(source);
        assert_eq!(regions.len(), 1);
        let region = &regions[0];
        // Content should be inside the tags (between > and </script>)
        let content = &source[region.start_byte..region.end_byte];
        assert!(content.contains("void Page_Load"));
    }
    /// Verifies that the `find_server_script_blocks` function correctly identifies multiple server-side script blocks within HTML source code.
    /// This test parses HTML containing two `<script runat="server">` blocks and confirms that exactly 2 script regions are found and returned.
    /// # Arguments
    /// None (this is a test function with no parameters).
    /// # Returns
    /// None (this is a test function that performs assertions).
    /// # Panics
    /// Panics if the number of found script blocks does not equal 2, indicating the `find_server_script_blocks` function is not working as expected.

    #[test]
    fn find_multiple_script_blocks() {
        let source = r#"<html>
<script runat="server">
void First() { }
</script>
<body></body>
<script runat="server">
void Second() { }
</script>
</html>"#;
        let regions = find_server_script_blocks(source);
        assert_eq!(regions.len(), 2);
    }
    /// Verifies that find_server_script_blocks returns an empty collection when the input HTML contains no server script blocks.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Returns
    /// Returns nothing. This function validates behavior through assertions.
    /// # Panics
    /// Panics if the regions collection is not empty, indicating that server script blocks were incorrectly identified in plain HTML content.

    #[test]
    fn no_script_blocks_returns_empty() {
        let source = r#"<html><body>No server code here.</body></html>"#;
        let regions = find_server_script_blocks(source);
        assert!(regions.is_empty());
    }
    /// Verifies that client-side script blocks without the `runat="server"` attribute are not identified as server script regions.
    /// This is a test function that validates the `find_server_script_blocks` function correctly ignores standard HTML script elements that lack server-side execution markers.
    /// # Arguments
    /// None
    /// # Returns
    /// None (unit test)

    #[test]
    fn client_script_not_matched() {
        let source = r#"<html><script type="text/javascript">alert('hi');</script></html>"#;
        // No runat="server" — should not be matched
        let regions = find_server_script_blocks(source);
        assert!(regions.is_empty());
    }

    // -------------------------------------------------------------------------
    // find_code_blocks tests
    // -------------------------------------------------------------------------
    /// Tests the `find_code_blocks` function to verify it correctly identifies inline code blocks within HTML markup.
    /// # Arguments
    /// None. This is a test function that uses hardcoded test data.
    /// # Returns
    /// None. This function performs assertions to validate behavior.
    /// # Panics
    /// Panics if any assertion fails, indicating that `find_code_blocks` did not correctly identify the expected code block region or its content.

    #[test]
    fn find_inline_code_block() {
        let source = r#"<html><body><% Response.Write("Hello"); %></body></html>"#;
        let regions = find_code_blocks(source);
        assert_eq!(regions.len(), 1);
        let content = &source[regions[0].start_byte..regions[0].end_byte];
        assert!(content.contains("Response.Write"));
    }
    /// Searches for embedded code expression blocks in template source code.
    /// Scans the provided source string for template expression delimiters and identifies all code blocks enclosed by `<%= ... %>` and `<%: ... %>` markers. Returns a vector of regions representing each discovered block's location and content.
    /// # Arguments
    /// * `source` - A string slice containing the template markup to search
    /// # Returns
    /// A vector of regions, where each region represents a found code expression block. The regions contain information about the block's position and content within the source string.

    #[test]
    fn find_expression_blocks() {
        let source = r#"<p><%= Model.Name %></p><p><%: Model.Title %></p>"#;
        let regions = find_code_blocks(source);
        // Should find both <%= ... %> and <%: ... %> blocks
        assert_eq!(regions.len(), 2);
    }
    /// Verifies that ASP.NET page directives are not incorrectly identified as code blocks.
    /// This test function checks that the `find_code_blocks` function properly distinguishes between ASP.NET directives (starting with `<%@`) and actual code blocks. It ensures that directives like `<%@ Page Language="C#" %>` are excluded from the code block detection results.
    /// # Arguments
    /// None. This is a test function that operates on a hardcoded ASP.NET source string.
    /// # Returns
    /// None. This function performs assertions to validate the behavior of `find_code_blocks`.
    /// # Panics
    /// Panics if the assertion fails, indicating that `find_code_blocks` incorrectly identified directives as code blocks.

    #[test]
    fn directives_not_matched_as_code_blocks() {
        let source = r#"<%@ Page Language="C#" %><html></html>"#;
        // Directives (<%@) must not appear as code blocks
        let regions = find_code_blocks(source);
        assert!(regions.is_empty());
    }
    /// Verifies that empty code blocks are not identified as valid code regions.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Returns
    /// Returns nothing. Asserts that parsing HTML containing an empty code block `<% %>` results in no identified code regions.

    #[test]
    fn empty_code_block_skipped() {
        let source = r#"<html><% %></html>"#;
        let regions = find_code_blocks(source);
        assert!(regions.is_empty());
    }

    // -------------------------------------------------------------------------
    // parse_aspx_chunks integration tests
    // -------------------------------------------------------------------------
    /// Parses C# code blocks embedded in ASP.NET ASPX files and validates correct extraction of methods.
    /// This is a unit test that verifies the parser can correctly identify and extract C# methods (Page_Load and Add) from a server-side script block in an ASPX page. It creates a temporary ASPX file, parses it using the ASPX chunk parser, and asserts that the resulting chunks contain the expected method names and are correctly tagged as C# language.
    /// # Arguments
    /// None. This is a test function with no parameters.
    /// # Returns
    /// None. This function returns unit type `()` and is intended to be run as a test.
    /// # Panics
    /// Panics if any of the following assertions fail:
    /// - Chunks are successfully parsed and not empty
    /// - The "Page_Load" method name is found in parsed chunks
    /// - The "Add" method name is found in parsed chunks
    /// - Any parsed chunk is not tagged with `Language::CSharp`

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_aspx_csharp_script_block() {
        let source = r#"<%@ Page Language="C#" %>
<html>
<script runat="server">
    public void Page_Load(object sender, EventArgs e) {
        Response.Write("Hello");
    }

    private int Add(int a, int b) {
        return a + b;
    }
</script>
</html>"#;

        let f = write_temp_file(source, "aspx");
        let parser = Parser::new().unwrap();
        let chunks = parse_aspx_chunks(source, f.path(), &parser).unwrap();

        // Should find at least the Page_Load and Add methods
        assert!(
            !chunks.is_empty(),
            "Expected chunks from C# script block, got none"
        );

        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"Page_Load"),
            "Expected Page_Load in chunks, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Add"),
            "Expected Add in chunks, got: {:?}",
            names
        );

        // All chunks should be tagged as CSharp
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::CSharp);
        }
    }
    /// Parses and validates VB.NET script blocks embedded in ASP.NET Web Forms markup.
    /// Creates a temporary ASP.NET file containing VB.NET code within a `<script runat="server">` block, parses it into language chunks, and verifies that all chunks are correctly identified as VB.NET. This test ensures that the parser properly extracts and classifies server-side VB.NET code from Web Forms pages.
    /// # Panics
    /// Panics if no chunks are parsed from the script block, indicating the parser failed to extract the VB.NET code.

    #[test]
    #[cfg(feature = "lang-vbnet")]
    fn parse_aspx_vb_script_block() {
        // VB.NET grammar requires a class/module wrapper around methods.
        // In real Web Forms, <script runat="server"> content is compiled
        // as members of the page class — so this is realistic.
        let source = r#"<%@ Page Language="VB" %>
<html>
<script runat="server">
Public Class MyPage
    Public Sub Page_Load(sender As Object, e As EventArgs)
        Response.Write("Hello")
    End Sub

    Public Function Add(a As Integer, b As Integer) As Integer
        Return a + b
    End Function
End Class
</script>
</html>"#;

        let f = write_temp_file(source, "aspx");
        let parser = Parser::new().unwrap();
        let chunks = parse_aspx_chunks(source, f.path(), &parser).unwrap();

        assert!(
            !chunks.is_empty(),
            "Expected chunks from VB script block, got none"
        );

        for chunk in &chunks {
            assert_eq!(chunk.language, Language::VbNet);
        }
    }
    /// Tests that the parser correctly handles inline code blocks in ASPX markup without producing errors. Inline code blocks (delimited by `<% %>` and `<%= %>`) are parsed through `set_included_ranges` and may not generate named chunks, but the parse operation must succeed without errors.
    /// # Arguments
    /// No arguments. This is a test function that uses hardcoded ASPX source containing inline code blocks.
    /// # Returns
    /// Nothing. This is a test function that asserts parsing succeeds.
    /// # Panics
    /// Panics if the parse operation returns an error, indicating that inline code blocks were not handled correctly.

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_aspx_inline_code_blocks() {
        // Inline code blocks are parsed by set_included_ranges — they may not
        // produce named chunks (they're typically single statements/expressions),
        // but the parse must not error.
        let source = r#"<%@ Page Language="C#" %>
<html><body>
<p><% var x = 42; %></p>
<p><%= x.ToString() %></p>
</body></html>"#;

        let f = write_temp_file(source, "aspx");
        let parser = Parser::new().unwrap();
        let result = parse_aspx_chunks(source, f.path(), &parser);
        // Must not error
        assert!(result.is_ok());
    }
    /// Verifies that parsing an ASPX file containing only static HTML markup (no server-side code) results in an empty chunks collection.
    /// # Arguments
    /// This is a test function with no parameters.
    /// # Returns
    /// None. This function performs assertions and panics if the test fails.
    /// # Panics
    /// Panics if the ASPX parser fails to initialize, if parsing the temporary file fails, or if the returned chunks collection is not empty.

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_aspx_no_server_code_returns_empty() {
        let source = r#"<%@ Page Language="C#" %>
<html><body><h1>Static page</h1></body></html>"#;

        let f = write_temp_file(source, "aspx");
        let parser = Parser::new().unwrap();
        let chunks = parse_aspx_chunks(source, f.path(), &parser).unwrap();
        assert!(chunks.is_empty());
    }
    /// Tests that ASPX files without an explicit Language directive are correctly parsed as C# by default.
    /// # Arguments
    /// This is a test function with no parameters. It internally creates a temporary ASPX file containing server-side code without a language directive and parses it using the `Parser`.
    /// # Returns
    /// Returns nothing. This is a test function that validates behavior through assertions.
    /// # Panics
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser fails to initialize
    /// - Parsing the ASPX chunks fails
    /// - No chunks are returned from parsing
    /// - Any parsed chunk is not identified as C# language

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_aspx_default_to_csharp() {
        // No Language directive — should default to C# and not error
        let source = r#"<html>
<script runat="server">
    protected void Button1_Click(object sender, EventArgs e) {
        Label1.Text = "Clicked";
    }
</script>
<body><form runat="server"></form></body>
</html>"#;

        let f = write_temp_file(source, "aspx");
        let parser = Parser::new().unwrap();
        let chunks = parse_aspx_chunks(source, f.path(), &parser).unwrap();

        assert!(
            !chunks.is_empty(),
            "Expected C# chunks without directive, got none"
        );
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::CSharp);
        }
    }
    /// Verifies that the ASPX parser correctly identifies and processes both standard expression blocks (`<%= %>`) and HTML-encoded expression blocks (`<%: %>`).
    /// # Arguments
    /// None. This is a test function that uses hardcoded ASPX source code.
    /// # Returns
    /// None. This function asserts expected behavior and panics on test failure.
    /// # Panics
    /// Panics if the number of found expression blocks is not exactly 2, or if the ASPX parsing fails unexpectedly.

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_aspx_expression_and_encoded_expression_blocks() {
        // Both <%= %> and <%: %> blocks are recognized
        let source = r#"<%@ Page Language="C#" %>
<html><body>
<p>Name: <%= Model.Name %></p>
<p>Safe: <%: Model.Description %></p>
</body></html>"#;

        let f = write_temp_file(source, "aspx");
        let regions_expr = find_code_blocks(source);
        // Should find two code block regions (the expression contents)
        assert_eq!(
            regions_expr.len(),
            2,
            "Expected 2 expression blocks, got {}",
            regions_expr.len()
        );

        // Parsing must not error
        let parser = Parser::new().unwrap();
        let result = parse_aspx_chunks(source, f.path(), &parser);
        assert!(result.is_ok());
    }

    // -------------------------------------------------------------------------
    // parse_aspx_all tests
    // -------------------------------------------------------------------------
    /// Parses an ASPX file and verifies that code chunks are extracted and function calls are properly identified.
    /// This is a test function that validates the `parse_aspx_all` parser by processing a sample ASPX source file containing C# script with a Page_Load method that calls a Helper method. It asserts that chunks are extracted and that the function call from Page_Load to Helper is correctly recorded.
    /// # Arguments
    /// None. Uses hardcoded ASPX source code containing a Page_Load method and Helper method.
    /// # Returns
    /// None. This is a test function that performs assertions rather than returning values.
    /// # Panics
    /// Panics if:
    /// - Creating a temporary file fails
    /// - Parsing the ASPX source fails
    /// - No chunks are extracted from the ASPX file
    /// - No FunctionCalls entry is found for the Page_Load method

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_aspx_all_returns_chunks_and_calls() {
        let source = r#"<%@ Page Language="C#" %>
<html>
<script runat="server">
    public void Page_Load(object sender, EventArgs e) {
        Helper();
    }

    private void Helper() { }
</script>
</html>"#;

        let f = write_temp_file(source, "aspx");
        let parser = Parser::new().unwrap();
        let (chunks, calls, _type_refs) = parse_aspx_all(source, f.path(), &parser).unwrap();

        assert!(!chunks.is_empty(), "Expected chunks");
        // Page_Load should have a call to Helper
        let page_load_calls = calls.iter().find(|fc| fc.name == "Page_Load");
        assert!(
            page_load_calls.is_some(),
            "Expected FunctionCalls entry for Page_Load"
        );
    }

    // -------------------------------------------------------------------------
    // ascx / master extension tests
    // -------------------------------------------------------------------------
    /// Parses an ASP.NET user control (.ascx) file and verifies that chunks are successfully extracted.
    /// # Arguments
    /// None. This function creates its own test data internally.
    /// # Returns
    /// Nothing. This is a test function that verifies parsing behavior through assertions.
    /// # Panics
    /// Panics if the parser fails to initialize, if parsing the ASPX chunks fails, or if the resulting chunks collection is empty.

    #[test]
    #[cfg(feature = "lang-csharp")]
    fn parse_ascx_user_control() {
        let source = r#"<%@ Control Language="C#" ClassName="MyControl" %>
<script runat="server">
    public string Title { get; set; }

    protected void Page_Load(object sender, EventArgs e) { }
</script>
<div><%= Title %></div>"#;

        let f = write_temp_file(source, "ascx");
        let parser = Parser::new().unwrap();
        let chunks = parse_aspx_chunks(source, f.path(), &parser).unwrap();
        assert!(!chunks.is_empty());
    }
}
