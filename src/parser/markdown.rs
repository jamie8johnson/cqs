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

        return Ok(vec![Chunk {
            id,
            file: path.to_path_buf(),
            language: Language::Markdown,
            chunk_type: ChunkType::Section,
            name: name.clone(),
            signature: name,
            content,
            doc: None,
            line_start: 1,
            line_end: lines.len() as u32,
            content_hash,
            parent_id: None,
            window_idx: None,
        }]);
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

        return Ok(vec![Chunk {
            id,
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
        }]);
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
            id,
            file: path.to_path_buf(),
            language: Language::Markdown,
            chunk_type: ChunkType::Section,
            name: section.name.clone(),
            signature,
            content,
            doc: None,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
            window_idx: None,
        });
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
        // Use the link text as the callee name — it's what the author chose to reference
        if !link_text.is_empty() && seen.insert(link_text.clone()) {
            let line_number = text[..match_start].matches('\n').count() as u32 + 1;
            calls.push(CallSite {
                callee_name: link_text,
                line_number,
            });
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
    }

    #[test]
    fn test_backtick_function_refs() {
        let text = "Call `Module.func()` and `Class::method(arg)` for results.";
        let calls = extract_references_from_text(text);

        let names: Vec<&str> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"Module.func"));
        assert!(names.contains(&"Class::method"));
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
}
