//! Markdown parser -- heading-based chunking with adaptive heading detection
//!
//! No tree-sitter. Scans lines for ATX headings, builds breadcrumb signatures,
//! extracts cross-references (links + backtick function patterns).
//!
//! Split into submodules:
//! - `headings` -- heading extraction, hierarchy detection, level tracking
//! - `code_blocks` -- fenced code block extraction, language detection
//! - `tables` -- table detection and chunk extraction

pub mod code_blocks;
mod headings;
mod tables;

pub use code_blocks::{extract_fenced_blocks, FencedBlock};

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use super::types::{CallSite, Chunk, ChunkType, FunctionCalls, Language, ParserError};
use headings::{detect_heading_levels, extract_headings};
use tables::{extract_table_chunks, TableContext};

/// Pre-compiled regex for markdown links: [text](url)
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex"));

/// Pre-compiled regex for backtick function references: `Name()`, `Module.func()`
static FUNC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([\w.:]+)\([^)]*\)`").expect("valid regex"));

/// Minimum section size (lines) -- smaller sections merge with next.
/// Override with CQS_MD_MIN_SECTION_LINES env var.
fn min_section_lines() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_MD_MIN_SECTION_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30)
    })
}

/// Maximum section size (lines) before attempting overflow split.
/// Override with CQS_MD_MAX_SECTION_LINES env var.
fn max_section_lines() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_MD_MAX_SECTION_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(150)
    })
}

/// Context for building a markdown chunk, replacing 9 positional arguments.
struct ChunkFields<'a> {
    path: &'a Path,
    id: String,
    name: String,
    signature: String,
    content: String,
    line_start: u32,
    line_end: u32,
    content_hash: String,
    parent_id: Option<String>,
}

/// Build a markdown Chunk with common fields pre-filled.
fn make_markdown_chunk(fields: ChunkFields<'_>) -> Chunk {
    Chunk {
        id: fields.id,
        file: fields.path.to_path_buf(),
        language: Language::Markdown,
        chunk_type: ChunkType::Section,
        name: fields.name,
        signature: fields.signature,
        content: fields.content,
        doc: None,
        line_start: fields.line_start,
        line_end: fields.line_end,
        content_hash: fields.content_hash,
        parent_id: fields.parent_id,
        window_idx: None,
        parent_type_name: None,
    }
}

/// Parse markdown into chunks using heading-based splitting
/// Adaptive heading detection handles both standard (H1 -> H2 -> H3) and
/// inverted (H2 title -> H1 chapters -> H3 subsections) hierarchies.
/// **Precondition:** `source` should use LF line endings. CRLF input works
/// (Rust's `.lines()` handles both), but content hashes will differ from
/// LF-normalized versions of the same file. The parser pipeline normalizes
/// line endings before calling this function.
pub fn parse_markdown_chunks(source: &str, path: &Path) -> Result<Vec<Chunk>, ParserError> {
    let _span = tracing::debug_span!("parse_markdown_chunks", path = %path.display()).entered();
    let lines: Vec<&str> = source.lines().collect();
    let headings = extract_headings(&lines);

    // No headings -> entire file is one chunk
    if headings.is_empty() {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();
        let content = source.to_string();
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let hash_prefix = content_hash.get(..8).unwrap_or(&content_hash);
        let id = format!("{}:1:{}", path.display(), hash_prefix);

        let mut chunks = vec![make_markdown_chunk(ChunkFields {
            path,
            id: id.clone(),
            name: name.clone(),
            signature: name.clone(),
            content,
            line_start: 1,
            line_end: lines.len() as u32,
            content_hash,
            parent_id: None,
        })];
        extract_table_chunks(
            &TableContext {
                lines: &lines,
                section_start: 0,
                section_end: lines.len(),
                section_name: &name,
                signature: &name,
                section_id: &id,
                path,
            },
            &mut chunks,
        );
        return Ok(chunks);
    }

    // Only one heading -> title-only file, one chunk
    if headings.len() == 1 {
        let h = &headings[0];
        let content = source.to_string();
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let hash_prefix = content_hash.get(..8).unwrap_or(&content_hash);
        let line_start = 1;
        let line_end = lines.len() as u32;
        let id = format!("{}:{}:{}", path.display(), line_start, hash_prefix);

        let mut chunks = vec![make_markdown_chunk(ChunkFields {
            path,
            id: id.clone(),
            name: h.text.clone(),
            signature: h.text.clone(),
            content,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
        })];
        extract_table_chunks(
            &TableContext {
                lines: &lines,
                section_start: 0,
                section_end: lines.len(),
                section_name: &h.text,
                signature: &h.text,
                section_id: &id,
                path,
            },
            &mut chunks,
        );
        return Ok(chunks);
    }

    // Adaptive heading detection
    let (title_idx, primary_level, overflow_level) = detect_heading_levels(&headings);

    // Build sections by splitting at the primary level
    let mut sections = build_sections(&lines, &headings, title_idx, primary_level);

    // Overflow split: if a section > MAX_SECTION_LINES, split at overflow_level
    if let Some(ovf) = overflow_level {
        sections = overflow_split(sections, &headings, ovf);
    }

    // Merge small sections (<MIN_SECTION_LINES) with next
    sections = merge_small_sections(sections);

    // Build chunks from sections
    let title_text = title_idx.map(|i| headings[i].text.as_str()).unwrap_or("");

    let mut chunks = Vec::with_capacity(sections.len());
    for section in &sections {
        let line_start = section.line_start as u32 + 1; // 1-indexed
        let line_end = section.line_end as u32; // 1-indexed (inclusive)

        let content = lines[section.line_start..section.line_end].join("\n");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let hash_prefix = content_hash.get(..8).unwrap_or(&content_hash);
        let id = format!("{}:{}:{}", path.display(), line_start, hash_prefix);

        // Build breadcrumb signature
        let signature = build_breadcrumb(title_text, &section.heading_stack);

        chunks.push(make_markdown_chunk(ChunkFields {
            path,
            id: id.clone(),
            name: section.name.clone(),
            signature: signature.clone(),
            content,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
        }));

        // Extract tables as additional chunks with parent_id = section chunk
        extract_table_chunks(
            &TableContext {
                lines: &lines,
                section_start: section.line_start,
                section_end: section.line_end,
                section_name: &section.name,
                signature: &signature,
                section_id: &id,
                path,
            },
            &mut chunks,
        );
    }

    Ok(chunks)
}

/// Extract all function calls from a markdown file (per-section)
pub fn parse_markdown_references(
    source: &str,
    path: &Path,
) -> Result<Vec<FunctionCalls>, ParserError> {
    let _span = tracing::debug_span!("parse_markdown_references", path = %path.display()).entered();
    let lines: Vec<&str> = source.lines().collect();
    let headings = extract_headings(&lines);

    if headings.is_empty() {
        // Whole file as one section
        let calls = extract_references_from_text(source);
        if calls.is_empty() {
            return Ok(vec![]);
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();
        return Ok(vec![FunctionCalls {
            name,
            line_start: 1,
            calls,
        }]);
    }

    // Split at headings and extract references per section
    let mut results = Vec::new();
    for i in 0..headings.len() {
        let start = headings[i].line;
        let end = if i + 1 < headings.len() {
            headings[i + 1].line
        } else {
            lines.len()
        };

        let section_text = lines[start..end].join("\n");
        let calls = extract_references_from_text(&section_text);
        if !calls.is_empty() {
            results.push(FunctionCalls {
                name: headings[i].text.clone(),
                line_start: start as u32 + 1,
                calls,
            });
        }
    }

    // Bridge edge: file stem -> title heading.
    // Connects file-stem-based references (from other docs' links) to the actual
    // section name in this document, enabling graph traversal across documents.
    // Example: "config" -> "Configuration Guide" so that a link [X](config.md)
    // from another doc can reach sections in this doc via BFS.
    if let Some(title) = headings.first() {
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if file_stem.len() > 1 && file_stem != title.text {
            results.push(FunctionCalls {
                name: file_stem,
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: title.text.clone(),
                    line_number: 1,
                }],
            });
        }
    }

    Ok(results)
}

/// Extract cross-references from a single chunk's content
pub fn extract_calls_from_markdown_chunk(chunk: &Chunk) -> Vec<CallSite> {
    extract_references_from_text(&chunk.content)
}

// --- Internal helpers ---

/// A section to become a chunk
#[derive(Debug)]
struct Section {
    name: String,
    heading_stack: Vec<String>, // parent headings for breadcrumb
    line_start: usize,          // 0-indexed, inclusive
    line_end: usize,            // 0-indexed, exclusive
}

/// Build sections by splitting at primary_level headings
fn build_sections(
    lines: &[&str],
    headings: &[headings::Heading],
    title_idx: Option<usize>,
    primary_level: u32,
) -> Vec<Section> {
    // Collect primary-level headings (excluding title)
    let primary_headings: Vec<&headings::Heading> = headings
        .iter()
        .enumerate()
        .filter(|(i, h)| h.level == primary_level && title_idx != Some(*i))
        .map(|(_, h)| h)
        .collect();

    if primary_headings.is_empty() {
        // No primary splits -- whole file is one section
        let name = headings[0].text.clone();
        return vec![Section {
            name,
            heading_stack: vec![],
            line_start: 0,
            line_end: lines.len(),
        }];
    }

    let mut sections = Vec::new();

    // Content before first primary heading (if there's a title)
    if let Some(ti) = title_idx {
        let first_primary_line = primary_headings[0].line;
        if headings[ti].line < first_primary_line {
            // There's content between the title and the first primary heading
            // Include it as a section only if there's non-blank content
            let content_start = headings[ti].line;
            let has_content = lines[content_start..first_primary_line]
                .iter()
                .any(|l| !l.trim().is_empty() && !l.trim().starts_with('#'));
            if has_content {
                sections.push(Section {
                    name: headings[ti].text.clone(),
                    heading_stack: vec![],
                    line_start: content_start,
                    line_end: first_primary_line,
                });
            }
        }
    }

    // Build heading stack tracker for breadcrumbs
    // Track the most recent heading at each level above primary
    let mut parent_stack: Vec<(u32, String)> = Vec::new();

    for (i, ph) in primary_headings.iter().enumerate() {
        let line_start = ph.line;
        let line_end = if i + 1 < primary_headings.len() {
            primary_headings[i + 1].line
        } else {
            lines.len()
        };

        // Update parent stack -- find any headings between previous section and this one
        // that are shallower than primary (they're parent context)
        let search_start = if i == 0 {
            0
        } else {
            primary_headings[i - 1].line
        };

        for h in headings {
            if h.line >= search_start && h.line < line_start && h.level < primary_level {
                // Remove any existing entries at this level or deeper
                parent_stack.retain(|(lvl, _)| *lvl < h.level);
                parent_stack.push((h.level, h.text.clone()));
            }
        }

        let heading_stack: Vec<String> = parent_stack.iter().map(|(_, t)| t.clone()).collect();

        sections.push(Section {
            name: ph.text.clone(),
            heading_stack,
            line_start,
            line_end,
        });
    }

    sections
}

/// Split oversized sections at overflow_level boundaries
fn overflow_split(
    sections: Vec<Section>,
    headings: &[headings::Heading],
    overflow_level: u32,
) -> Vec<Section> {
    let mut result = Vec::new();

    for section in sections {
        let section_lines = section.line_end - section.line_start;
        if section_lines <= max_section_lines() {
            result.push(section);
            continue;
        }

        // Find overflow-level headings within this section
        let sub_headings: Vec<&headings::Heading> = headings
            .iter()
            .filter(|h| {
                h.level == overflow_level
                    && h.line > section.line_start
                    && h.line < section.line_end
            })
            .collect();

        if sub_headings.is_empty() {
            result.push(section);
            continue;
        }

        // Split: content before first sub-heading, then each sub-section
        if sub_headings[0].line > section.line_start {
            result.push(Section {
                name: section.name.clone(),
                heading_stack: section.heading_stack.clone(),
                line_start: section.line_start,
                line_end: sub_headings[0].line,
            });
        }

        for (i, sh) in sub_headings.iter().enumerate() {
            let end = if i + 1 < sub_headings.len() {
                sub_headings[i + 1].line
            } else {
                section.line_end
            };

            let mut stack = section.heading_stack.clone();
            stack.push(section.name.clone());

            result.push(Section {
                name: sh.text.clone(),
                heading_stack: stack,
                line_start: sh.line,
                line_end: end,
            });
        }
    }

    result
}

/// Merge adjacent sections smaller than MIN_SECTION_LINES into the next section
fn merge_small_sections(sections: Vec<Section>) -> Vec<Section> {
    if sections.len() <= 1 {
        return sections;
    }

    let mut result: Vec<Section> = Vec::new();
    // Track start of consecutive small sections to merge into the next big one
    let mut pending_start: Option<usize> = None;
    let mut pending_end: usize = 0;

    for section in sections {
        let section_lines = section.line_end - section.line_start;

        if section_lines < min_section_lines() {
            if pending_start.is_none() {
                pending_start = Some(section.line_start);
            }
            pending_end = section.line_end;
        } else {
            // Big section -- absorb any pending small sections by extending start
            let mut section = section;
            if let Some(start) = pending_start.take() {
                section.line_start = start;
            }
            result.push(section);
        }
    }

    // Trailing small sections -- merge into previous big section
    if let Some(start) = pending_start {
        if let Some(last) = result.last_mut() {
            last.line_end = pending_end;
        } else {
            // All sections were small -- shouldn't happen with real files,
            // but return a single section covering the whole range
            result.push(Section {
                name: "Document".to_string(),
                heading_stack: vec![],
                line_start: start,
                line_end: pending_end,
            });
        }
    }

    result
}

/// Build breadcrumb signature: "Title > Parent > Section"
fn build_breadcrumb(title: &str, heading_stack: &[String]) -> String {
    let mut parts = Vec::new();
    if !title.is_empty() {
        parts.push(title.to_string());
    }
    for h in heading_stack {
        if !parts.contains(h) {
            parts.push(h.clone());
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    parts.join(" > ")
}

/// Extract file stem from a relative .md/.mdx URL.
/// Returns None for external URLs (http/https), absolute paths, or non-markdown targets.
fn extract_md_file_stem(url: &str) -> Option<String> {
    // Skip external URLs
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("//") {
        return None;
    }
    // Skip absolute paths
    if url.starts_with('/') {
        return None;
    }
    // Strip anchor fragment before checking extension
    let path_part = url.split('#').next().unwrap_or(url);
    // Check for .md or .mdx extension
    if !path_part.ends_with(".md") && !path_part.ends_with(".mdx") {
        return None;
    }
    // Extract file stem (last path component without extension)
    // PB-28: Split on both `/` and `\` for cross-platform paths
    let filename = path_part.rsplit(['/', '\\']).next().unwrap_or(path_part);
    let stem = filename
        .strip_suffix(".mdx")
        .or_else(|| filename.strip_suffix(".md"))?;
    if stem.is_empty() || stem.len() == 1 {
        return None;
    }
    Some(stem.to_string())
}

/// Extract anchor fragment from a URL.
/// Returns the part after `#` if present and non-empty.
fn extract_anchor(url: &str) -> Option<String> {
    let anchor = url.split_once('#')?.1;
    if anchor.is_empty() {
        return None;
    }
    Some(anchor.to_string())
}

/// Extract cross-references (links + backtick function patterns) from text
fn extract_references_from_text(text: &str) -> Vec<CallSite> {
    let mut calls = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Markdown links (not images): [text](url)
    // Rust regex doesn't support lookbehind, so match all links then filter images
    for cap in LINK_RE.captures_iter(text) {
        let Some(full_match) = cap.get(0) else {
            continue;
        };
        let match_start = full_match.start();
        // Skip image links: preceded by '!'
        if match_start > 0 && text.as_bytes()[match_start - 1] == b'!' {
            continue;
        }
        let link_text = cap[1].to_string();
        let line_number = text[..match_start].matches('\n').count() as u32 + 1;

        // Use the link text as the callee name -- it's what the author chose to reference
        if !link_text.is_empty() && seen.insert(link_text.clone()) {
            calls.push(CallSite {
                callee_name: link_text,
                line_number,
            });
        }

        // For .md/.mdx links, also emit file stem as callee (cross-document link)
        let url = cap[2].to_string();
        if let Some(stem) = extract_md_file_stem(&url) {
            if seen.insert(stem.clone()) {
                calls.push(CallSite {
                    callee_name: stem,
                    line_number,
                });
            }
        }
        // For anchor links, emit the anchor as callee (cross-section reference)
        if let Some(anchor) = extract_anchor(&url) {
            if seen.insert(anchor.clone()) {
                calls.push(CallSite {
                    callee_name: anchor,
                    line_number,
                });
            }
        }
    }

    // Backtick function references: `Name()`, `Module.func()`, `Class::method(args)`
    for cap in FUNC_RE.captures_iter(text) {
        // Extract the name before the parentheses
        let full_ref = &cap[1];
        let callee_name = full_ref.to_string();
        if !callee_name.is_empty() && seen.insert(callee_name.clone()) {
            let Some(full_match) = cap.get(0) else {
                continue;
            };
            let match_start = full_match.start();
            let line_number = text[..match_start].matches('\n').count() as u32 + 1;
            calls.push(CallSite {
                callee_name,
                line_number,
            });
        }
    }

    calls
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_path() -> PathBuf {
        PathBuf::from("test.md")
    }

    #[test]
    fn test_no_headings_fallback() {
        let source = "Just some text\nwith no headings\nat all.\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "test");
        assert_eq!(chunks[0].chunk_type, ChunkType::Section);
        assert_eq!(chunks[0].signature, "test");
    }

    #[test]
    fn test_single_heading_fallback() {
        let source = "# Only Title\n\nSome content below.\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "Only Title");
        assert_eq!(chunks[0].signature, "Only Title");
    }

    #[test]
    fn test_standard_hierarchy() {
        // Build sections > MIN_SECTION_LINES so they don't get merged
        let mut source = String::from("# Title\n\nIntro text.\n\n## Section A\n\n");
        for i in 0..35 {
            source.push_str(&format!("Section A line {}.\n", i));
        }
        source.push_str("\n## Section B\n\n");
        for i in 0..35 {
            source.push_str(&format!("Section B line {}.\n", i));
        }

        let chunks = parse_markdown_chunks(&source, &test_path()).unwrap();

        // Should have: Section A and Section B (title preamble merged into A since it's small)
        assert!(
            chunks.len() >= 2,
            "got {} chunks: {:?}",
            chunks.len(),
            chunks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );

        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Section A"));
        assert!(names.contains(&"Section B"));

        // Section A should have breadcrumb with Title
        let sec_a = chunks.iter().find(|c| c.name == "Section A").unwrap();
        assert!(
            sec_a.signature.contains("Title"),
            "signature was: {}",
            sec_a.signature
        );
    }

    #[test]
    fn test_inverted_hierarchy() {
        // AVEVA pattern: H2 title -> H1 chapters -> H3 subsections
        let mut source = String::new();
        source.push_str("## AVEVA Historian Concepts\n\n");
        source.push_str("Introduction text.\n\n");
        source.push_str("# Process Data\n\n");
        for i in 0..80 {
            source.push_str(&format!("Line {} of process data content.\n", i));
        }
        source.push_str("\n# Data Acquisition\n\n");
        for i in 0..80 {
            source.push_str(&format!("Line {} of data acquisition content.\n", i));
        }

        let chunks = parse_markdown_chunks(&source, &test_path()).unwrap();

        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Process Data"));
        assert!(names.contains(&"Data Acquisition"));

        // Breadcrumbs should include the H2 title
        let pd = chunks.iter().find(|c| c.name == "Process Data").unwrap();
        assert!(
            pd.signature.contains("AVEVA Historian Concepts"),
            "signature was: {}",
            pd.signature
        );
    }

    #[test]
    fn test_cross_references_extracted() {
        let source =
            "# Docs\n\n## API\n\nSee [TagRead](api.md) for details.\nUse `TagRead()` to read.\n";
        let refs = parse_markdown_references(source, &test_path()).unwrap();

        assert!(!refs.is_empty());
        let all_callees: Vec<&str> = refs
            .iter()
            .flat_map(|fc| fc.calls.iter().map(|c| c.callee_name.as_str()))
            .collect();
        // Link text
        assert!(all_callees.contains(&"TagRead"));
        // File stem from link URL
        assert!(
            all_callees.contains(&"api"),
            "Should extract file stem 'api' from api.md link: {:?}",
            all_callees
        );
        // Backtick function ref
        assert!(all_callees.contains(&"TagRead"));
    }

    #[test]
    fn test_image_links_not_extracted() {
        let source = "# Doc\n\n![screenshot](img.png)\n[real link](other.md)\n";
        let refs = parse_markdown_references(source, &test_path()).unwrap();

        let all_callees: Vec<&str> = refs
            .iter()
            .flat_map(|fc| fc.calls.iter().map(|c| c.callee_name.as_str()))
            .collect();
        assert!(!all_callees.contains(&"screenshot"));
        assert!(all_callees.contains(&"real link"));
        // File stem extracted from other.md
        assert!(
            all_callees.contains(&"other"),
            "Should extract file stem 'other': {:?}",
            all_callees
        );
    }

    #[test]
    fn test_backtick_function_refs() {
        let text = "Call `Module.func()` and `Class::method(arg)` for results.";
        let calls = extract_references_from_text(text);

        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"Module.func"));
        assert!(names.contains(&"Class::method"));
    }

    // ===== Cross-document link extraction tests =====

    #[test]
    fn test_link_extracts_file_stem() {
        let text = "[Configuration Guide](config.md)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"Configuration Guide"));
        assert!(
            names.contains(&"config"),
            "Should extract file stem: {:?}",
            names
        );
    }

    #[test]
    fn test_link_extracts_anchor() {
        let text = "[Database Settings](config.md#db-settings)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"Database Settings"));
        assert!(
            names.contains(&"config"),
            "Should extract file stem: {:?}",
            names
        );
        assert!(
            names.contains(&"db-settings"),
            "Should extract anchor: {:?}",
            names
        );
    }

    #[test]
    fn test_link_extracts_both_stem_and_anchor() {
        let text = "[X](foo.md#bar)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"X"));
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn test_external_links_no_stem() {
        let text = "[Docs](https://example.com/page.md) and [API](http://api.com)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        // Link text is still extracted
        assert!(names.contains(&"Docs"));
        assert!(names.contains(&"API"));
        // But no file stems from external URLs
        assert!(
            !names.contains(&"page"),
            "Should not extract stem from external URL: {:?}",
            names
        );
    }

    #[test]
    fn test_self_anchor_link() {
        let text = "[Jump to setup](#setup-instructions)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"Jump to setup"));
        assert!(
            names.contains(&"setup-instructions"),
            "Should extract self-anchor: {:?}",
            names
        );
        // No file stem (no .md target)
    }

    #[test]
    fn test_link_with_directory_prefix() {
        let text = "[Setup](../guides/setup-guide.md)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"Setup"));
        assert!(
            names.contains(&"setup-guide"),
            "Should extract stem from last path component: {:?}",
            names
        );
        // Should NOT include directory prefix
        assert!(!names.contains(&"../guides/setup-guide"));
    }

    #[test]
    fn test_link_non_md_target() {
        let text = "[Source](main.rs) and [Schema](schema.sql)";
        let calls = extract_references_from_text(text);
        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        // Link text extracted
        assert!(names.contains(&"Source"));
        assert!(names.contains(&"Schema"));
        // No file stems -- only .md/.mdx targets
        assert!(
            !names.contains(&"main"),
            "Should not extract stem from .rs: {:?}",
            names
        );
        assert!(
            !names.contains(&"schema"),
            "Should not extract stem from .sql: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_md_file_stem_helper() {
        assert_eq!(
            extract_md_file_stem("config.md"),
            Some("config".to_string())
        );
        assert_eq!(extract_md_file_stem("page.mdx"), Some("page".to_string()));
        assert_eq!(
            extract_md_file_stem("dir/file.md"),
            Some("file".to_string())
        );
        assert_eq!(
            extract_md_file_stem("../other/doc.md"),
            Some("doc".to_string())
        );
        assert_eq!(
            extract_md_file_stem("config.md#anchor"),
            Some("config".to_string())
        );
        assert_eq!(extract_md_file_stem("https://example.com/page.md"), None);
        assert_eq!(extract_md_file_stem("http://foo.md"), None);
        assert_eq!(extract_md_file_stem("/absolute/path.md"), None);
        assert_eq!(extract_md_file_stem("code.rs"), None);
        assert_eq!(extract_md_file_stem(""), None);
        assert_eq!(extract_md_file_stem(".md"), None); // empty stem
        assert_eq!(extract_md_file_stem("a.md"), None); // single-char stem
    }

    #[test]
    fn test_extract_anchor_helper() {
        assert_eq!(
            extract_anchor("file.md#section"),
            Some("section".to_string())
        );
        assert_eq!(
            extract_anchor("#local-anchor"),
            Some("local-anchor".to_string())
        );
        assert_eq!(extract_anchor("file.md"), None);
        assert_eq!(extract_anchor("file.md#"), None); // empty anchor
        assert_eq!(extract_anchor(""), None);
    }

    // ===== Bridge edge tests =====

    #[test]
    fn test_bridge_edge_emitted() {
        let source = "# Configuration Guide\n\n## Database\n\nSome content.\n";
        let path = PathBuf::from("config.md");
        let refs = parse_markdown_references(source, &path).unwrap();

        // Should have a bridge edge: "config" -> "Configuration Guide"
        let bridge = refs.iter().find(|fc| fc.name == "config");
        assert!(
            bridge.is_some(),
            "Should emit bridge edge for file stem 'config': {:?}",
            refs.iter().map(|fc| &fc.name).collect::<Vec<_>>()
        );
        let bridge = bridge.unwrap();
        assert_eq!(bridge.calls.len(), 1);
        assert_eq!(bridge.calls[0].callee_name, "Configuration Guide");
    }

    #[test]
    fn test_bridge_edge_skipped_when_stem_equals_title() {
        // File stem "overview" matches title "overview" -- no bridge needed
        let source = "# overview\n\nContent here.\n";
        let path = PathBuf::from("overview.md");
        let refs = parse_markdown_references(source, &path).unwrap();

        let bridge = refs.iter().find(|fc| fc.name == "overview");
        assert!(
            bridge.is_none(),
            "Should not emit bridge when stem equals title: {:?}",
            refs.iter().map(|fc| &fc.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_bridge_edge_skipped_no_headings() {
        let source = "Just plain text with no headings at all.\n";
        let path = PathBuf::from("notes.md");
        let refs = parse_markdown_references(source, &path).unwrap();

        // No headings -> no bridge edge (early return path)
        let bridge = refs.iter().find(|fc| fc.name == "notes");
        assert!(
            bridge.is_none(),
            "Should not emit bridge when no headings: {:?}",
            refs.iter().map(|fc| &fc.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_bridge_edge_with_directory_path() {
        let source = "# AVEVA System Platform\n\nContent.\n";
        let path = PathBuf::from("docs/aveva-system-platform.md");
        let refs = parse_markdown_references(source, &path).unwrap();

        let bridge = refs.iter().find(|fc| fc.name == "aveva-system-platform");
        assert!(
            bridge.is_some(),
            "Should emit bridge using file stem from full path: {:?}",
            refs.iter().map(|fc| &fc.name).collect::<Vec<_>>()
        );
        assert_eq!(
            bridge.unwrap().calls[0].callee_name,
            "AVEVA System Platform"
        );
    }
}
