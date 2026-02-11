//! Markdown parser — heading-based chunking with adaptive heading detection
//!
//! No tree-sitter. Scans lines for ATX headings, builds breadcrumb signatures,
//! extracts cross-references (links + backtick function patterns).

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use super::types::{CallSite, Chunk, ChunkType, FunctionCalls, Language, ParserError};

/// Pre-compiled regex for markdown links: [text](url)
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex"));

/// Pre-compiled regex for backtick function references: `Name()`, `Module.func()`
static FUNC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([\w.:]+)\([^)]*\)`").expect("valid regex"));

/// Minimum section size (lines) — smaller sections merge with next
const MIN_SECTION_LINES: usize = 30;
/// Maximum section size (lines) before attempting overflow split
const MAX_SECTION_LINES: usize = 150;

/// A detected heading in the markdown source
#[derive(Debug, Clone)]
struct Heading {
    level: u32,
    text: String,
    line: usize, // 0-indexed
}

/// Parse markdown into chunks using heading-based splitting
///
/// Adaptive heading detection handles both standard (H1 → H2 → H3) and
/// inverted (H2 title → H1 chapters → H3 subsections) hierarchies.
pub fn parse_markdown_chunks(source: &str, path: &Path) -> Result<Vec<Chunk>, ParserError> {
    let lines: Vec<&str> = source.lines().collect();
    let headings = extract_headings(&lines);

    // No headings → entire file is one chunk
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

        let mut chunks = vec![Chunk {
            id: id.clone(),
            file: path.to_path_buf(),
            language: Language::Markdown,
            chunk_type: ChunkType::Section,
            name: name.clone(),
            signature: name.clone(),
            content,
            doc: None,
            line_start: 1,
            line_end: lines.len() as u32,
            content_hash,
            parent_id: None,
            window_idx: None,
        }];
        extract_table_chunks(&lines, 0, lines.len(), &name, &name, &id, path, &mut chunks);
        return Ok(chunks);
    }

    // Only one heading → title-only file, one chunk
    if headings.len() == 1 {
        let h = &headings[0];
        let content = source.to_string();
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let hash_prefix = content_hash.get(..8).unwrap_or(&content_hash);
        let line_start = 1;
        let line_end = lines.len() as u32;
        let id = format!("{}:{}:{}", path.display(), line_start, hash_prefix);

        let mut chunks = vec![Chunk {
            id: id.clone(),
            file: path.to_path_buf(),
            language: Language::Markdown,
            chunk_type: ChunkType::Section,
            name: h.text.clone(),
            signature: h.text.clone(),
            content,
            doc: None,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
            window_idx: None,
        }];
        extract_table_chunks(
            &lines,
            0,
            lines.len(),
            &h.text,
            &h.text,
            &id,
            path,
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

        chunks.push(Chunk {
            id: id.clone(),
            file: path.to_path_buf(),
            language: Language::Markdown,
            chunk_type: ChunkType::Section,
            name: section.name.clone(),
            signature: signature.clone(),
            content,
            doc: None,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
            window_idx: None,
        });

        // Extract tables as additional chunks with parent_id = section chunk
        extract_table_chunks(
            &lines,
            section.line_start,
            section.line_end,
            &section.name,
            &signature,
            &id,
            path,
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

    // Bridge edge: file stem → title heading.
    // Connects file-stem-based references (from other docs' links) to the actual
    // section name in this document, enabling graph traversal across documents.
    // Example: "config" → "Configuration Guide" so that a link [X](config.md)
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

// ─── Internal helpers ───

/// Scan lines for ATX headings, respecting fenced code blocks
fn extract_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut in_code_block = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Toggle fenced code block state
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            continue;
        }

        // ATX heading: one or more # followed by space
        if let Some(level) = atx_heading_level(trimmed) {
            let text = trimmed[level as usize..]
                .trim_start_matches(' ')
                .trim_end()
                .to_string();
            if !text.is_empty() {
                headings.push(Heading {
                    level,
                    text,
                    line: i,
                });
            }
        }
    }

    headings
}

/// Return ATX heading level (1-6) or None
fn atx_heading_level(line: &str) -> Option<u32> {
    let bytes = line.as_bytes();
    let mut count = 0u32;
    for &b in bytes {
        if b == b'#' {
            count += 1;
        } else {
            break;
        }
    }
    // Must have 1-6 # followed by space (or line is just #s — treat as invalid)
    if (1..=6).contains(&count) && bytes.get(count as usize) == Some(&b' ') {
        Some(count)
    } else {
        None
    }
}

/// Detect title, primary split level, and overflow split level
///
/// Returns (title_heading_index, primary_level, overflow_level)
fn detect_heading_levels(headings: &[Heading]) -> (Option<usize>, u32, Option<u32>) {
    // Count frequency of each heading level
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for h in headings {
        *freq.entry(h.level).or_insert(0) += 1;
    }

    // Title level: level of the first heading
    let first_level = headings[0].level;
    let first_level_count = freq.get(&first_level).copied().unwrap_or(0);

    // Title index: first heading, but only if its level appears once
    // (or if it's the shallowest level and appears first)
    let title_idx = if first_level_count == 1 {
        Some(0)
    } else {
        // First heading's level appears multiple times — no distinct title
        None
    };

    // Primary split level: shallowest heading level appearing more than once,
    // excluding the title level if it only appears once
    let mut levels: Vec<u32> = freq.keys().copied().collect();
    levels.sort();

    let primary_level = levels
        .iter()
        .copied()
        .find(|&lvl| {
            let count = freq.get(&lvl).copied().unwrap_or(0);
            if title_idx.is_some() && lvl == first_level {
                false // Skip title level
            } else {
                count > 1
            }
        })
        .unwrap_or(first_level); // Fallback: split at first heading's level

    // Overflow level: next level deeper than primary that exists
    // (excluding the title level — it's a parent, not a subsection)
    let title_level = title_idx.map(|i| headings[i].level);
    let overflow_level = levels
        .iter()
        .copied()
        .find(|&lvl| lvl > primary_level && Some(lvl) != title_level);

    (title_idx, primary_level, overflow_level)
}

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
    headings: &[Heading],
    title_idx: Option<usize>,
    primary_level: u32,
) -> Vec<Section> {
    // Collect primary-level headings (excluding title)
    let primary_headings: Vec<&Heading> = headings
        .iter()
        .enumerate()
        .filter(|(i, h)| h.level == primary_level && title_idx != Some(*i))
        .map(|(_, h)| h)
        .collect();

    if primary_headings.is_empty() {
        // No primary splits — whole file is one section
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

        // Update parent stack — find any headings between previous section and this one
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
    headings: &[Heading],
    overflow_level: u32,
) -> Vec<Section> {
    let mut result = Vec::new();

    for section in sections {
        let section_lines = section.line_end - section.line_start;
        if section_lines <= MAX_SECTION_LINES {
            result.push(section);
            continue;
        }

        // Find overflow-level headings within this section
        let sub_headings: Vec<&Heading> = headings
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

        if section_lines < MIN_SECTION_LINES {
            if pending_start.is_none() {
                pending_start = Some(section.line_start);
            }
            pending_end = section.line_end;
        } else {
            // Big section — absorb any pending small sections by extending start
            let mut section = section;
            if let Some(start) = pending_start.take() {
                section.line_start = start;
            }
            result.push(section);
        }
    }

    // Trailing small sections — merge into previous big section
    if let Some(start) = pending_start {
        if let Some(last) = result.last_mut() {
            last.line_end = pending_end;
        } else {
            // All sections were small — shouldn't happen with real files,
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
///
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
    let filename = path_part.rsplit('/').next().unwrap_or(path_part);
    let stem = filename
        .strip_suffix(".mdx")
        .or_else(|| filename.strip_suffix(".md"))?;
    if stem.is_empty() || stem.len() == 1 {
        return None;
    }
    Some(stem.to_string())
}

/// Extract anchor fragment from a URL.
///
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

        // Use the link text as the callee name — it's what the author chose to reference
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

/// Maximum chars per table chunk before row-wise splitting
const MAX_TABLE_CHARS: usize = 1500;

/// Extract table chunks from a section's line range and append to `chunks`.
///
/// For each detected table, creates an additional chunk with `parent_id` set to
/// the containing section. Large tables are split row-wise with headers preserved.
#[allow(clippy::too_many_arguments)]
fn extract_table_chunks(
    lines: &[&str],
    section_start: usize,
    section_end: usize,
    section_name: &str,
    signature: &str,
    section_id: &str,
    path: &Path,
    chunks: &mut Vec<Chunk>,
) {
    let section_lines = &lines[section_start..section_end];
    let table_spans = detect_tables(section_lines);

    for (table_idx, span) in table_spans.iter().enumerate() {
        let table_lines = &section_lines[span.start..span.end];
        let table_content = table_lines.join("\n");

        // Disambiguate multiple tables: single = "(table)", multiple = "(table L{line})"
        let abs_table_start = section_start + span.start;
        let table_name = if table_spans.len() == 1 {
            format!("{} (table)", section_name)
        } else {
            format!("{} (table L{})", section_name, abs_table_start + 1)
        };

        let table_line_start = abs_table_start as u32 + 1; // 1-indexed
        let table_line_end = (section_start + span.end) as u32; // 1-indexed

        if table_content.len() <= MAX_TABLE_CHARS {
            let table_hash = blake3::hash(table_content.as_bytes()).to_hex().to_string();
            let thash_prefix = table_hash.get(..8).unwrap_or(&table_hash);
            let table_id = format!("{}:{}:{}", path.display(), table_line_start, thash_prefix);
            chunks.push(Chunk {
                id: table_id,
                file: path.to_path_buf(),
                language: Language::Markdown,
                chunk_type: ChunkType::Section,
                name: table_name,
                signature: signature.to_string(),
                content: table_content,
                doc: None,
                line_start: table_line_start,
                line_end: table_line_end,
                content_hash: table_hash,
                parent_id: Some(section_id.to_string()),
                window_idx: None,
            });
        } else {
            // Split row-wise with headers preserved
            let header_count = span.header_end - span.start;
            let header_lines = &table_lines[..header_count];
            let header_prefix = header_lines.join("\n");
            let data_lines = &table_lines[header_count..];

            let mut window: Vec<&str> = Vec::new();
            let mut window_chars = header_prefix.len();
            let mut widx: u32 = 0;

            for row in data_lines {
                if window_chars + row.len() + 1 > MAX_TABLE_CHARS && !window.is_empty() {
                    emit_table_window(
                        &header_prefix,
                        &window,
                        &table_name,
                        signature,
                        section_id,
                        table_line_start,
                        table_line_end,
                        table_idx,
                        widx,
                        path,
                        chunks,
                    );
                    window.clear();
                    window_chars = header_prefix.len();
                    widx += 1;
                }
                window.push(row);
                window_chars += row.len() + 1;
            }
            // Emit remaining rows
            if !window.is_empty() {
                emit_table_window(
                    &header_prefix,
                    &window,
                    &table_name,
                    signature,
                    section_id,
                    table_line_start,
                    table_line_end,
                    table_idx,
                    widx,
                    path,
                    chunks,
                );
            }
        }
    }
}

/// Emit a single row-wise table window chunk.
#[allow(clippy::too_many_arguments)]
fn emit_table_window(
    header_prefix: &str,
    rows: &[&str],
    name: &str,
    signature: &str,
    parent_id: &str,
    line_start: u32,
    line_end: u32,
    table_idx: usize,
    window_idx: u32,
    path: &Path,
    chunks: &mut Vec<Chunk>,
) {
    let mut content = header_prefix.to_string();
    content.push('\n');
    content.push_str(&rows.join("\n"));
    let whash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let whash_prefix = whash.get(..8).unwrap_or(&whash);
    let wid = format!(
        "{}:{}:{}:t{}w{}",
        path.display(),
        line_start,
        whash_prefix,
        table_idx,
        window_idx
    );
    chunks.push(Chunk {
        id: wid,
        file: path.to_path_buf(),
        language: Language::Markdown,
        chunk_type: ChunkType::Section,
        name: name.to_string(),
        signature: signature.to_string(),
        content,
        doc: None,
        line_start,
        line_end,
        content_hash: whash,
        parent_id: Some(parent_id.to_string()),
        window_idx: Some(window_idx),
    });
}

/// A detected table span within a section
#[derive(Debug, Clone)]
struct TableSpan {
    /// 0-indexed line, inclusive (header row)
    start: usize,
    /// 0-indexed line, exclusive (first non-table line)
    end: usize,
    /// Line index after separator (start of data rows, for row-wise splitting)
    header_end: usize,
}

/// Pre-compiled regex for table separator rows: |---|---|  or  :---:|---:  etc.
static TABLE_SEP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$").expect("valid regex")
});

/// Detect markdown tables within a slice of lines.
///
/// Tables are identified by their separator row (the `|---|---|` line).
/// The header row is the line immediately above the separator, and data rows
/// follow below. Tables inside fenced code blocks are ignored.
fn detect_tables(lines: &[&str]) -> Vec<TableSpan> {
    let mut tables = Vec::new();
    let mut in_code_block = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Track fenced code blocks
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        // Look for separator rows
        if !TABLE_SEP_RE.is_match(trimmed) {
            continue;
        }

        // Separator found — check header row above
        if i == 0 {
            continue; // No header row above
        }
        let header_line = lines[i - 1].trim();
        if !header_line.contains('|') {
            continue; // Header must contain pipes
        }

        // Check at least one data row below
        let data_start = i + 1;
        if data_start >= lines.len() {
            continue; // No data rows
        }
        let first_data = lines[data_start].trim();
        if !first_data.contains('|') {
            continue; // First data row must contain pipes
        }

        // Find extent of data rows (contiguous pipe-containing lines)
        let mut data_end = data_start + 1;
        while data_end < lines.len() {
            let row = lines[data_end].trim();
            if row.is_empty() || !row.contains('|') {
                break;
            }
            // Stop at headings
            if atx_heading_level(row).is_some() {
                break;
            }
            data_end += 1;
        }

        let span = TableSpan {
            start: i - 1,      // header row
            end: data_end,     // exclusive
            header_end: i + 1, // first data row
        };
        tables.push(span);
    }

    tables
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_path() -> PathBuf {
        PathBuf::from("test.md")
    }

    #[test]
    fn test_atx_heading_level() {
        assert_eq!(atx_heading_level("# Title"), Some(1));
        assert_eq!(atx_heading_level("## Section"), Some(2));
        assert_eq!(atx_heading_level("### Sub"), Some(3));
        assert_eq!(atx_heading_level("###### Deep"), Some(6));
        assert_eq!(atx_heading_level("####### Too deep"), None);
        assert_eq!(atx_heading_level("#NoSpace"), None);
        assert_eq!(atx_heading_level("Not a heading"), None);
        assert_eq!(atx_heading_level(""), None);
    }

    #[test]
    fn test_headings_in_code_blocks_ignored() {
        let source =
            "# Real heading\n\n```\n# Not a heading\n## Also not\n```\n\n## Another real heading\n";
        let lines: Vec<&str> = source.lines().collect();
        let headings = extract_headings(&lines);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].text, "Real heading");
        assert_eq!(headings[1].text, "Another real heading");
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
        // AVEVA pattern: H2 title → H1 chapters → H3 subsections
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
        // No file stems — only .md/.mdx targets
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

        // Should have a bridge edge: "config" → "Configuration Guide"
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
        // File stem "overview" matches title "overview" — no bridge needed
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

        // No headings → no bridge edge (early return path)
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

    #[test]
    fn test_detect_levels_standard() {
        let headings = vec![
            Heading {
                level: 1,
                text: "Title".into(),
                line: 0,
            },
            Heading {
                level: 2,
                text: "A".into(),
                line: 5,
            },
            Heading {
                level: 2,
                text: "B".into(),
                line: 20,
            },
            Heading {
                level: 3,
                text: "Sub".into(),
                line: 30,
            },
        ];
        let (title_idx, primary, overflow) = detect_heading_levels(&headings);
        assert_eq!(title_idx, Some(0));
        assert_eq!(primary, 2);
        assert_eq!(overflow, Some(3));
    }

    #[test]
    fn test_detect_levels_inverted() {
        let headings = vec![
            Heading {
                level: 2,
                text: "Doc Title".into(),
                line: 0,
            },
            Heading {
                level: 1,
                text: "Chapter A".into(),
                line: 10,
            },
            Heading {
                level: 1,
                text: "Chapter B".into(),
                line: 50,
            },
            Heading {
                level: 3,
                text: "Sub".into(),
                line: 60,
            },
        ];
        let (title_idx, primary, overflow) = detect_heading_levels(&headings);
        assert_eq!(title_idx, Some(0));
        assert_eq!(primary, 1);
        assert_eq!(overflow, Some(3));
    }

    // ── Table detection tests ──

    #[test]
    fn test_table_detection_basic() {
        let lines = vec![
            "Some text before",
            "| Name | Type | Default |",
            "|------|------|---------|",
            "| port | int  | 8080    |",
            "| host | str  | 0.0.0.0 |",
            "",
            "Some text after",
        ];
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].start, 1); // header row
        assert_eq!(tables[0].end, 5); // exclusive (empty line)
        assert_eq!(tables[0].header_end, 3); // first data row
    }

    #[test]
    fn test_table_detection_without_leading_pipes() {
        let lines = vec![
            "Name | Type | Default",
            "------|------|--------",
            "port | int  | 8080",
            "host | str  | 0.0.0.0",
        ];
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].start, 0);
        assert_eq!(tables[0].end, 4);
    }

    #[test]
    fn test_table_detection_alignment() {
        let lines = vec![
            "| Left | Center | Right |",
            "|:-----|:------:|------:|",
            "| a    | b      | c     |",
        ];
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);
    }

    #[test]
    fn test_table_detection_in_code_block() {
        let lines = vec![
            "```",
            "| Name | Type |",
            "|------|------|",
            "| a    | b    |",
            "```",
        ];
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 0, "Tables in code blocks should be ignored");
    }

    #[test]
    fn test_table_detection_multiple() {
        let lines = vec![
            "| A | B |",
            "|---|---|",
            "| 1 | 2 |",
            "",
            "Some text",
            "",
            "| X | Y |",
            "|---|---|",
            "| 3 | 4 |",
        ];
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].start, 0);
        assert_eq!(tables[0].end, 3);
        assert_eq!(tables[1].start, 6);
        assert_eq!(tables[1].end, 9);
    }

    #[test]
    fn test_table_detection_no_separator() {
        let lines = vec!["| Name | Type |", "| port | int  |", "| host | str  |"];
        let tables = detect_tables(&lines);
        assert_eq!(
            tables.len(),
            0,
            "Pipes without separator row should not be a table"
        );
    }

    #[test]
    fn test_table_detection_min_size() {
        // Exactly 3 lines (header + sep + 1 data) = detected
        let lines = vec!["| A |", "|---|", "| 1 |"];
        let tables = detect_tables(&lines);
        assert_eq!(tables.len(), 1);

        // Only 2 lines (header + sep, no data) = not detected
        let lines2 = vec!["| A |", "|---|"];
        let tables2 = detect_tables(&lines2);
        assert_eq!(tables2.len(), 0);
    }

    // ── Table chunk creation tests ──

    #[test]
    fn test_table_chunk_created() {
        let source = "# Doc Title\n\n\
            ## Configuration\n\n\
            Some intro text about configuration.\n\n\
            | Option | Default | Description |\n\
            |--------|---------|-------------|\n\
            | port   | 8080    | Server port |\n\
            | host   | 0.0.0.0 | Bind address|\n\n\
            More text after the table.\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        // Should have section chunk + table chunk
        let table_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.name.contains("(table)"))
            .collect();
        assert_eq!(
            table_chunks.len(),
            1,
            "Should create one table chunk, got chunks: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        assert!(table_chunks[0].content.contains("| Option"));
        assert!(table_chunks[0].content.contains("| port"));
    }

    #[test]
    fn test_table_chunk_has_parent_id() {
        let source = "# Doc\n\n\
            ## Settings\n\n\
            | Key | Val |\n\
            |-----|-----|\n\
            | a   | 1   |\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        let table_chunk = chunks.iter().find(|c| c.name.contains("(table)")).unwrap();
        // Find the section chunk (the one without parent_id that contains the table)
        let section_chunk = chunks.iter().find(|c| c.parent_id.is_none()).unwrap();
        assert_eq!(
            table_chunk.parent_id.as_ref().unwrap(),
            &section_chunk.id,
            "Table chunk parent_id should match section chunk id"
        );
    }

    #[test]
    fn test_table_chunk_name() {
        // Single table → "(table)" — section name comes from the section after merge
        let source = "# Doc\n\n## Sec\n\n| A |\n|---|\n| 1 |\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        let table = chunks.iter().find(|c| c.name.contains("(table)")).unwrap();
        // Small sections get merged — name may be "Doc" or "Sec" depending on merge
        assert!(
            table.name.ends_with("(table)"),
            "Single table should end with '(table)': {}",
            table.name
        );

        // Multiple tables → "(table L{line})"
        let source2 = "# Doc\n\n## Sec\n\n\
            | A |\n|---|\n| 1 |\n\n\
            Some text.\n\n\
            | B |\n|---|\n| 2 |\n";
        let chunks2 = parse_markdown_chunks(source2, &test_path()).unwrap();
        let tables: Vec<_> = chunks2
            .iter()
            .filter(|c| c.name.contains("(table"))
            .collect();
        assert_eq!(tables.len(), 2, "Should have two table chunks");
        assert!(
            tables[0].name.contains("(table L"),
            "Should include line number: {}",
            tables[0].name
        );
    }

    #[test]
    fn test_table_chunk_line_numbers() {
        let source = "# Doc\n\n## Config\n\nIntro text.\n\n\
            | Name | Type |\n\
            |------|------|\n\
            | port | int  |\n\n\
            More text.\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        let table = chunks.iter().find(|c| c.name.contains("(table)")).unwrap();
        // Table starts at line 7 (1-indexed), ends at line 9
        assert_eq!(table.line_start, 7, "Table should start at line 7");
        assert_eq!(table.line_end, 9, "Table should end at line 9");
    }

    #[test]
    fn test_large_table_split_row_wise() {
        // Build a table with 50 rows to exceed 1500 chars
        let mut source = String::from("# Doc\n\n## Data\n\n");
        source.push_str("| Column A | Column B | Column C | Column D | Column E |\n");
        source.push_str("|----------|----------|----------|----------|----------|\n");
        for i in 0..50 {
            source.push_str(&format!(
                "| value_{}_a | value_{}_b | value_{}_c | value_{}_d | value_{}_e |\n",
                i, i, i, i, i
            ));
        }
        let chunks = parse_markdown_chunks(&source, &test_path()).unwrap();
        let table_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.name.contains("(table)"))
            .collect();
        assert!(
            table_chunks.len() > 1,
            "Large table should be split into multiple chunks, got {}",
            table_chunks.len()
        );
        // Each split should start with header rows
        for tc in &table_chunks {
            assert!(
                tc.content.starts_with("| Column A"),
                "Each split should start with header: {}",
                &tc.content[..50.min(tc.content.len())]
            );
            assert!(
                tc.content.contains("|-------"),
                "Each split should contain separator"
            );
        }
        // All should have parent_id
        for tc in &table_chunks {
            assert!(
                tc.parent_id.is_some(),
                "Split table chunks should have parent_id"
            );
        }
        // All should have window_idx
        for tc in &table_chunks {
            assert!(
                tc.window_idx.is_some(),
                "Split table chunks should have window_idx"
            );
        }
    }

    #[test]
    fn test_table_at_file_start() {
        // Table before any heading — file gets a single chunk from file_stem
        let source = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        // No headings → whole file is one chunk + table chunk
        let table_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.name.contains("(table)"))
            .collect();
        assert_eq!(
            table_chunks.len(),
            1,
            "Should detect table even with no headings: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }
}
