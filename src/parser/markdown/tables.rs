//! Table detection and chunk extraction
//!
//! Detects markdown tables by their separator rows (`|---|---|`), extracts them
//! as additional chunks with parent references, and splits large tables row-wise
//! with headers preserved.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use super::headings::atx_heading_level;
use crate::parser::types::{Chunk, ChunkType, Language};

/// Maximum chars per table chunk before row-wise splitting
const MAX_TABLE_CHARS: usize = 1500;

/// Pre-compiled regex for table separator rows: |---|---|  or  :---:|---:  etc.
static TABLE_SEP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$").expect("valid regex")
});

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

/// Context for table chunk extraction, replacing scattered arguments.
pub(super) struct TableContext<'a> {
    pub lines: &'a [&'a str],
    pub section_start: usize,
    pub section_end: usize,
    pub section_name: &'a str,
    pub signature: &'a str,
    pub section_id: &'a str,
    pub path: &'a Path,
}

/// Context for emitting a single row-wise table window chunk.
struct TableWindowContext<'a> {
    header_prefix: &'a str,
    name: &'a str,
    signature: &'a str,
    parent_id: &'a str,
    line_start: u32,
    line_end: u32,
    table_idx: usize,
    path: &'a Path,
}

/// Extract table chunks from a section's line range and append to `chunks`.
/// For each detected table, creates an additional chunk with `parent_id` set to
/// the containing section. Large tables are split row-wise with headers preserved.
pub(super) fn extract_table_chunks(ctx: &TableContext<'_>, chunks: &mut Vec<Chunk>) {
    let section_lines = &ctx.lines[ctx.section_start..ctx.section_end];
    let table_spans = detect_tables(section_lines);

    for (table_idx, span) in table_spans.iter().enumerate() {
        let table_lines = &section_lines[span.start..span.end];
        let table_content = table_lines.join("\n");

        // Disambiguate multiple tables: single = "(table)", multiple = "(table L{line})"
        let abs_table_start = ctx.section_start + span.start;
        let table_name = if table_spans.len() == 1 {
            format!("{} (table)", ctx.section_name)
        } else {
            format!("{} (table L{})", ctx.section_name, abs_table_start + 1)
        };

        let table_line_start = abs_table_start as u32 + 1; // 1-indexed
        let table_line_end = (ctx.section_start + span.end) as u32; // 1-indexed

        if table_content.len() <= MAX_TABLE_CHARS {
            let table_hash = blake3::hash(table_content.as_bytes()).to_hex().to_string();
            let thash_prefix = table_hash.get(..8).unwrap_or(&table_hash);
            let table_id = format!(
                "{}:{}:{}",
                ctx.path.display(),
                table_line_start,
                thash_prefix
            );
            chunks.push(Chunk {
                id: table_id,
                file: ctx.path.to_path_buf(),
                language: Language::Markdown,
                chunk_type: ChunkType::Section,
                name: table_name,
                signature: ctx.signature.to_string(),
                content: table_content,
                doc: None,
                line_start: table_line_start,
                line_end: table_line_end,
                content_hash: table_hash,
                parent_id: Some(ctx.section_id.to_string()),
                window_idx: None,
                parent_type_name: None,
                parser_version: super::super::chunk::PARSER_VERSION,
            });
        } else {
            // Split row-wise with headers preserved
            let header_count = span.header_end - span.start;
            let header_lines = &table_lines[..header_count];
            let header_prefix = header_lines.join("\n");
            let data_lines = &table_lines[header_count..];

            let win_ctx = TableWindowContext {
                header_prefix: &header_prefix,
                name: &table_name,
                signature: ctx.signature,
                parent_id: ctx.section_id,
                line_start: table_line_start,
                line_end: table_line_end,
                table_idx,
                path: ctx.path,
            };

            let mut window: Vec<&str> = Vec::new();
            let mut window_chars = header_prefix.len();
            let mut widx: u32 = 0;

            for row in data_lines {
                if window_chars + row.len() + 1 > MAX_TABLE_CHARS && !window.is_empty() {
                    emit_table_window(&win_ctx, &window, widx, chunks);
                    window.clear();
                    window_chars = header_prefix.len();
                    widx += 1;
                }
                window.push(row);
                window_chars += row.len() + 1;
            }
            // Emit remaining rows
            if !window.is_empty() {
                emit_table_window(&win_ctx, &window, widx, chunks);
            }
        }
    }
}

/// Emit a single row-wise table window chunk.
fn emit_table_window(
    ctx: &TableWindowContext<'_>,
    rows: &[&str],
    window_idx: u32,
    chunks: &mut Vec<Chunk>,
) {
    let mut content = ctx.header_prefix.to_string();
    content.push('\n');
    content.push_str(&rows.join("\n"));
    let whash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let whash_prefix = whash.get(..8).unwrap_or(&whash);
    let wid = format!(
        "{}:{}:{}:t{}w{}",
        ctx.path.display(),
        ctx.line_start,
        whash_prefix,
        ctx.table_idx,
        window_idx
    );
    chunks.push(Chunk {
        id: wid,
        file: ctx.path.to_path_buf(),
        language: Language::Markdown,
        chunk_type: ChunkType::Section,
        name: ctx.name.to_string(),
        signature: ctx.signature.to_string(),
        content,
        doc: None,
        line_start: ctx.line_start,
        line_end: ctx.line_end,
        content_hash: whash,
        parent_id: Some(ctx.parent_id.to_string()),
        window_idx: Some(window_idx),
        parent_type_name: None,
        parser_version: super::super::chunk::PARSER_VERSION,
    });
}

/// Detect markdown tables within a slice of lines.
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

        // Separator found -- check header row above
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
    use crate::parser::markdown::parse_markdown_chunks;
    use std::path::PathBuf;

    fn test_path() -> PathBuf {
        PathBuf::from("test.md")
    }

    // -- Table detection tests --

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

    // -- Table chunk creation tests --

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
        // Single table -> "(table)" -- section name comes from the section after merge
        let source = "# Doc\n\n## Sec\n\n| A |\n|---|\n| 1 |\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        let table = chunks.iter().find(|c| c.name.contains("(table)")).unwrap();
        // Small sections get merged -- name may be "Doc" or "Sec" depending on merge
        assert!(
            table.name.ends_with("(table)"),
            "Single table should end with '(table)': {}",
            table.name
        );

        // Multiple tables -> "(table L{line})"
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
        // Table before any heading -- file gets a single chunk from file_stem
        let source = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let chunks = parse_markdown_chunks(source, &test_path()).unwrap();
        // No headings -> whole file is one chunk + table chunk
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
