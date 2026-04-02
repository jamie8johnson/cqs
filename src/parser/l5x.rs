//! Rockwell/Allen-Bradley PLC export parser (L5X and L5K formats)
//!
//! Extracts Structured Text (IEC 61131-3 ST) code from Logix Designer exports.
//! - L5X: XML format. ST code in CDATA sections within `<STContent>` elements.
//! - L5K: Legacy ASCII format. ST code in keyword-delimited blocks (`ROUTINE...END_ROUTINE`).
//!
//! Both formats share the same ST parsing and chunk generation logic.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::StreamingIterator;

use super::types::{capture_name_to_chunk_type, Chunk, ChunkType, Language, ParserError};
use super::Parser;

// ===========================================================================
// Shared types and helpers
// ===========================================================================

/// An ST code region extracted from either L5X or L5K files.
struct StRegion {
    /// The extracted ST source (lines concatenated with newlines)
    source: String,
    /// Line number (1-indexed) where the region starts in the original file
    line_start: u32,
    /// Context: parent routine name (if known)
    routine_name: Option<String>,
    /// Context: parent program name (if known)
    program_name: Option<String>,
}

/// Count newlines in `source[..byte_offset]` to get 1-indexed line number.
fn line_of(source: &str, byte_offset: usize) -> u32 {
    source[..byte_offset]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Find the nearest preceding regex capture group 1 before `byte_offset`.
fn find_nearest_before<'a>(re: &Regex, source: &'a str, byte_offset: usize) -> Option<&'a str> {
    let mut best: Option<regex::Match<'a>> = None;
    for m in re.find_iter(&source[..byte_offset]) {
        best = Some(m);
    }
    best.and_then(|m| re.captures(&source[m.start()..]))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

/// Parse ST regions into chunks using the tree-sitter ST grammar.
/// Shared by both L5X and L5K parsers.
fn parse_st_regions(
    regions: &[StRegion],
    path: &Path,
    parser: &Parser,
) -> Result<Vec<Chunk>, ParserError> {
    if regions.is_empty() {
        return Ok(vec![]);
    }

    let st_lang = Language::StructuredText;
    let grammar = st_lang
        .try_grammar()
        .ok_or_else(|| ParserError::ParseFailed("Structured Text grammar not available".into()))?;
    let query = parser.get_query(st_lang)?;

    let mut all_chunks = Vec::new();

    for region in regions {
        let region_chunk_start = all_chunks.len();

        let mut ts_parser = tree_sitter::Parser::new();
        ts_parser
            .set_language(&grammar)
            .map_err(|e| ParserError::ParseFailed(format!("{}", e)))?;

        let tree = match ts_parser.parse(&region.source, None) {
            Some(t) => t,
            None => {
                tracing::warn!(
                    routine = region.routine_name.as_deref().unwrap_or("?"),
                    "Failed to parse ST region"
                );
                continue;
            }
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), region.source.as_bytes());

        while let Some(m) = matches.next() {
            match extract_st_chunk(&region.source, m, query, st_lang, path, region) {
                Ok(chunk) => all_chunks.push(chunk),
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to extract ST chunk");
                }
            }
        }

        // If no chunks were extracted but we have a routine name,
        // create a synthetic chunk for the whole routine
        if all_chunks.len() == region_chunk_start {
            if let Some(ref name) = region.routine_name {
                let content = region.source.clone();
                let line_count = content.lines().count() as u32;
                let sig = content.lines().next().unwrap_or("").to_string();
                let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                all_chunks.push(Chunk {
                    id: format!(
                        "{}:{}:{}",
                        path.display(),
                        region.line_start,
                        &content_hash[..8]
                    ),
                    name: name.clone(),
                    chunk_type: ChunkType::Function,
                    content,
                    file: path.to_path_buf(),
                    line_start: region.line_start,
                    line_end: region.line_start + line_count,
                    language: st_lang,
                    signature: sig,
                    doc: None,
                    content_hash,
                    parent_id: None,
                    window_idx: None,
                    parent_type_name: region.program_name.clone(),
                });
            }
        }
    }

    Ok(all_chunks)
}

/// Extract a single chunk from an ST tree-sitter match, adjusting coordinates
/// for the original file's line numbers.
fn extract_st_chunk(
    source: &str,
    m: &tree_sitter::QueryMatch,
    query: &tree_sitter::Query,
    language: Language,
    file_path: &Path,
    region: &StRegion,
) -> Result<Chunk, ParserError> {
    let def = language.def();
    let mut name = String::new();
    let mut chunk_type = ChunkType::Function;
    let mut node = m.captures[0].node;

    for cap in m.captures {
        let cap_name = query.capture_names()[cap.index as usize];
        if cap_name == "name" {
            name = source[cap.node.byte_range()].to_string();
        } else if let Some(ct) = capture_name_to_chunk_type(cap_name) {
            chunk_type = ct;
            node = cap.node;
        }
    }

    if name.is_empty() {
        return Err(ParserError::ParseFailed("No name captured".into()));
    }

    let content = source[node.byte_range()].to_string();
    let line_start = region.line_start + node.start_position().row as u32;
    let line_end = region.line_start + node.end_position().row as u32;

    let signature = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();

    let mut mutable_name = name.clone();
    let mut mutable_type = chunk_type;
    if let Some(post_process) = def.post_process_chunk {
        if !post_process(&mut mutable_name, &mut mutable_type, node, source) {
            return Err(ParserError::ParseFailed("Discarded by post_process".into()));
        }
    }

    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();

    Ok(Chunk {
        id: format!(
            "{}:{}:{}",
            file_path.display(),
            line_start,
            &content_hash[..8]
        ),
        name: mutable_name,
        chunk_type: mutable_type,
        content,
        file: file_path.to_path_buf(),
        line_start,
        line_end,
        language,
        signature,
        doc: None,
        content_hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: region.program_name.clone(),
    })
}

// ===========================================================================
// L5X format (XML with CDATA)
// ===========================================================================

/// Match `<Routine Name="..." Type="ST">` to get routine names.
static L5X_ROUTINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<Routine\s+Name\s*=\s*"([^"]+)"[^>]*\bType\s*=\s*"ST"[^>]*>"#)
        .expect("valid regex")
});

/// Match `<Program Name="...">` for program names.
static L5X_PROGRAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<Program\s+Name\s*=\s*"([^"]+)""#).expect("valid regex"));

/// Match `<STContent>...</STContent>` blocks.
static L5X_ST_CONTENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<STContent>(.*?)</STContent>"#).expect("valid regex"));

/// Extract text from CDATA sections: `<![CDATA[...]]>`
static CDATA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<!\[CDATA\[(.*?)]]>"#).expect("valid regex"));

/// Extract ST regions from an L5X (XML) file.
fn extract_l5x_regions(source: &str) -> Vec<StRegion> {
    let mut regions = Vec::new();

    for st_match in L5X_ST_CONTENT_RE.captures_iter(source) {
        let full = st_match.get(0).unwrap();
        let inner = st_match.get(1).unwrap();
        let start_byte = full.start();
        let line_start = line_of(source, start_byte);

        let mut lines = Vec::new();
        for cdata in CDATA_RE.captures_iter(inner.as_str()) {
            if let Some(content) = cdata.get(1) {
                lines.push(content.as_str().to_string());
            }
        }

        if lines.is_empty() {
            continue;
        }

        let routine_name =
            find_nearest_before(&L5X_ROUTINE_RE, source, start_byte).map(|s| s.to_string());
        let program_name =
            find_nearest_before(&L5X_PROGRAM_RE, source, start_byte).map(|s| s.to_string());

        regions.push(StRegion {
            source: lines.join("\n"),
            line_start,
            routine_name,
            program_name,
        });
    }

    regions
}

/// Parse an L5X file and extract ST code chunks.
pub(crate) fn parse_l5x_chunks(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<Vec<Chunk>, ParserError> {
    let _span = tracing::info_span!("parse_l5x", path = %path.display()).entered();
    let regions = extract_l5x_regions(source);
    if regions.is_empty() {
        tracing::debug!("No ST content found in L5X file");
    }
    let chunks = parse_st_regions(&regions, path, parser)?;
    tracing::info!(
        chunks = chunks.len(),
        regions = regions.len(),
        "L5X parse complete"
    );
    Ok(chunks)
}

// ===========================================================================
// L5K format (ASCII keyword-delimited)
// ===========================================================================

// L5K format uses keyword-delimited blocks. The exact syntax varies by
// RSLogix version, but the general structure is:
//
//   ROUTINE <name>
//     ...routine attributes...
//     ST_CONTENT := [
//       <line>;
//       <line>;
//     ];
//     ...or for some versions...
//     N:0 <st_code>;
//     N:1 <st_code>;
//   END_ROUTINE
//
// The ROUTINE line includes type info. We match ST routines and extract
// the content lines, stripping line number prefixes.

/// Match ROUTINE blocks: from `ROUTINE <name>` to `END_ROUTINE`.
/// Group 1: routine name. Group 2: block content.
static L5K_ROUTINE_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?msi)^\s*ROUTINE\s+(\w+)\b([^\x00]*?)^\s*END_ROUTINE\b"#).expect("valid regex")
});

/// Match `PROGRAM <name>` declarations.
static L5K_PROGRAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?mi)^\s*PROGRAM\s+(\w+)\b"#).expect("valid regex"));

/// Match line-numbered content: `N:0 code;` or `N:123 code;`
static L5K_NUMBERED_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*N:\d+\s+(.+)$"#).expect("valid regex"));

/// Match ST_CONTENT block: `ST_CONTENT := [ ... ];`
static L5K_ST_CONTENT_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?ms)ST_CONTENT\s*:=\s*\[(.*?)\]\s*;"#).expect("valid regex"));

/// Extract ST regions from an L5K (ASCII) file.
fn extract_l5k_regions(source: &str) -> Vec<StRegion> {
    let mut regions = Vec::new();

    for block in L5K_ROUTINE_BLOCK_RE.captures_iter(source) {
        let routine_name = block.get(1).unwrap().as_str().to_string();
        let block_content = block.get(2).unwrap().as_str();
        let block_start = block.get(0).unwrap().start();

        // Check if this routine is type ST
        let is_st = block_content
            .lines()
            .take(5) // Type declaration is near the top
            .any(|line| {
                let upper = line.to_uppercase();
                upper.contains("TYPE") && upper.contains(":=") && upper.contains("ST")
            });

        if !is_st {
            continue;
        }

        let line_start = line_of(source, block_start);

        // Try ST_CONTENT := [ ... ]; block first
        let st_source = if let Some(st_block) = L5K_ST_CONTENT_BLOCK_RE.captures(block_content) {
            let inner = st_block.get(1).unwrap().as_str();
            // Lines inside the bracket block, trimmed
            inner
                .lines()
                .map(|l| l.trim().trim_end_matches(','))
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            // Fall back to N:0 numbered lines
            let lines: Vec<String> = L5K_NUMBERED_LINE_RE
                .captures_iter(block_content)
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
                .collect();
            if lines.is_empty() {
                // Last resort: take all non-attribute lines as content
                block_content
                    .lines()
                    .filter(|l| {
                        let trimmed = l.trim();
                        !trimmed.is_empty()
                            && !trimmed.starts_with("DESCRIPTION")
                            && !trimmed.starts_with("TYPE")
                            && !trimmed.starts_with("ROUTINE")
                            && !trimmed.starts_with("END_ROUTINE")
                    })
                    .map(|l| l.trim())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                lines.join("\n")
            }
        };

        if st_source.trim().is_empty() {
            continue;
        }

        let program_name =
            find_nearest_before(&L5K_PROGRAM_RE, source, block_start).map(|s| s.to_string());

        regions.push(StRegion {
            source: st_source,
            line_start,
            routine_name: Some(routine_name),
            program_name,
        });
    }

    regions
}

/// Parse an L5K file and extract ST code chunks.
pub(crate) fn parse_l5k_chunks(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<Vec<Chunk>, ParserError> {
    let _span = tracing::info_span!("parse_l5k", path = %path.display()).entered();
    let regions = extract_l5k_regions(source);
    if regions.is_empty() {
        tracing::debug!("No ST content found in L5K file");
    }
    let chunks = parse_st_regions(&regions, path, parser)?;
    tracing::info!(
        chunks = chunks.len(),
        regions = regions.len(),
        "L5K parse complete"
    );
    Ok(chunks)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- L5X tests ---

    const SAMPLE_L5X: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<RSLogix5000Content>
  <Controller Name="MainController">
    <Programs>
      <Program Name="MainProgram">
        <Routines>
          <Routine Name="MainRoutine" Type="ST">
            <STContent>
              <Line Number="0"><![CDATA[// Main routine]]></Line>
              <Line Number="1"><![CDATA[myTimer(IN := startButton, PT := T#5s);]]></Line>
              <Line Number="2"><![CDATA[IF myTimer.Q THEN]]></Line>
              <Line Number="3"><![CDATA[  output := TRUE;]]></Line>
              <Line Number="4"><![CDATA[END_IF;]]></Line>
            </STContent>
          </Routine>
          <Routine Name="LadderRoutine" Type="RLL">
            <RLLContent>
              <Rung Number="0" Type="N">
                <Text><![CDATA[XIC(startButton)OTE(motorRun);]]></Text>
              </Rung>
            </RLLContent>
          </Routine>
        </Routines>
      </Program>
    </Programs>
  </Controller>
</RSLogix5000Content>"#;

    #[test]
    fn test_l5x_extract_st_regions() {
        let regions = extract_l5x_regions(SAMPLE_L5X);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].routine_name.as_deref(), Some("MainRoutine"));
        assert_eq!(regions[0].program_name.as_deref(), Some("MainProgram"));
        assert!(regions[0].source.contains("myTimer"));
        assert!(regions[0].source.contains("END_IF"));
        assert!(!regions[0].source.contains("XIC"));
    }

    #[test]
    fn test_l5x_cdata_extraction() {
        let inner = r#"
              <Line Number="0"><![CDATA[line_one;]]></Line>
              <Line Number="1"><![CDATA[line_two;]]></Line>
        "#;
        let lines: Vec<_> = CDATA_RE
            .captures_iter(inner)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();
        assert_eq!(lines, vec!["line_one;", "line_two;"]);
    }

    #[test]
    fn test_l5x_parse_finds_chunks() {
        let parser = Parser::new().unwrap();
        let chunks = parse_l5x_chunks(SAMPLE_L5X, Path::new("test.l5x"), &parser).unwrap();
        assert!(!chunks.is_empty(), "Expected at least one chunk from L5X");
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::StructuredText);
        }
    }

    #[test]
    fn test_l5x_no_st_content() {
        let source = r#"<?xml version="1.0"?><RSLogix5000Content><Controller Name="Empty"><Programs/></Controller></RSLogix5000Content>"#;
        let parser = Parser::new().unwrap();
        let chunks = parse_l5x_chunks(source, Path::new("empty.l5x"), &parser).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_l5x_ladder_only_skipped() {
        let source = r#"<?xml version="1.0"?>
<RSLogix5000Content>
  <Controller Name="Test">
    <Programs>
      <Program Name="Ladder">
        <Routines>
          <Routine Name="Rung1" Type="RLL">
            <RLLContent>
              <Rung><Text><![CDATA[XIC(btn)OTE(out);]]></Text></Rung>
            </RLLContent>
          </Routine>
        </Routines>
      </Program>
    </Programs>
  </Controller>
</RSLogix5000Content>"#;
        let regions = extract_l5x_regions(source);
        assert!(regions.is_empty());
    }

    #[test]
    fn test_find_nearest_before() {
        let source = r#"<Program Name="Prog1"><Program Name="Prog2"><STContent>"#;
        let name = find_nearest_before(&L5X_PROGRAM_RE, source, source.len());
        assert_eq!(name, Some("Prog2"));
    }

    // --- L5K tests ---

    const SAMPLE_L5K: &str = r#"
CONTROLLER TestController

PROGRAM MainProgram

  ROUTINE MainRoutine
    DESCRIPTION := "Main control logic"
    Type := ST
    ST_CONTENT := [
      myTimer(IN := startButton, PT := T#5s);
      IF myTimer.Q THEN
        output := TRUE;
      END_IF;
    ];
  END_ROUTINE

  ROUTINE LadderRoutine
    Type := RLL
    RLL_CONTENT := [
      XIC(startButton)OTE(motorRun);
    ];
  END_ROUTINE

END_PROGRAM
"#;

    #[test]
    fn test_l5k_extract_st_regions() {
        let regions = extract_l5k_regions(SAMPLE_L5K);
        assert_eq!(regions.len(), 1, "Should find exactly one ST routine");
        assert_eq!(regions[0].routine_name.as_deref(), Some("MainRoutine"));
        assert_eq!(regions[0].program_name.as_deref(), Some("MainProgram"));
        assert!(regions[0].source.contains("myTimer"));
        assert!(regions[0].source.contains("END_IF"));
        assert!(!regions[0].source.contains("XIC"));
    }

    #[test]
    fn test_l5k_ladder_only_skipped() {
        let source = r#"
PROGRAM LadderOnly
  ROUTINE Rung1
    Type := RLL
    RLL_CONTENT := [
      XIC(btn)OTE(out);
    ];
  END_ROUTINE
END_PROGRAM
"#;
        let regions = extract_l5k_regions(source);
        assert!(regions.is_empty());
    }

    #[test]
    fn test_l5k_parse_finds_chunks() {
        let parser = Parser::new().unwrap();
        let chunks = parse_l5k_chunks(SAMPLE_L5K, Path::new("test.l5k"), &parser).unwrap();
        assert!(!chunks.is_empty(), "Expected at least one chunk from L5K");
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::StructuredText);
        }
    }

    #[test]
    fn test_l5k_empty_file() {
        let parser = Parser::new().unwrap();
        let chunks = parse_l5k_chunks("", Path::new("empty.l5k"), &parser).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_l5k_numbered_lines() {
        let source = r#"
PROGRAM Prog1
  ROUTINE NumberedRoutine
    Type := ST
    N:0 x := 1;
    N:1 y := x + 2;
    N:2 z := y * 3;
  END_ROUTINE
END_PROGRAM
"#;
        let regions = extract_l5k_regions(source);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].source.contains("x := 1;"));
        assert!(regions[0].source.contains("y := x + 2;"));
        assert!(regions[0].source.contains("z := y * 3;"));
    }
}
