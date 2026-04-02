//! L5X (Rockwell/Allen-Bradley Logix Designer) parser
//!
//! Extracts Structured Text (IEC 61131-3 ST) code from L5X XML export files.
//! L5X files contain ST code inside CDATA sections within `<STContent>` elements.
//!
//! Strategy: regex-based extraction of CDATA content from STContent blocks,
//! then delegation to the ST tree-sitter grammar for chunk/call/type extraction.
//! Similar to the ASPX parser pattern (regex regions → tree-sitter parse).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::StreamingIterator;

use super::types::{capture_name_to_chunk_type, Chunk, ChunkType, Language, ParserError};
use super::Parser;

// ---------------------------------------------------------------------------
// Regexes
// ---------------------------------------------------------------------------

/// Match `<Routine Name="..." Type="ST">` to get routine names.
/// Group 1: routine name.
static ROUTINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<Routine\s+Name\s*=\s*"([^"]+)"[^>]*\bType\s*=\s*"ST"[^>]*>"#)
        .expect("valid regex")
});

/// Match `<Program Name="...">` for program names.
/// Group 1: program name.
static PROGRAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<Program\s+Name\s*=\s*"([^"]+)""#).expect("valid regex"));

/// Match `<STContent>...</STContent>` blocks (possibly spanning many lines).
/// Group 1: everything between the tags.
static ST_CONTENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<STContent>(.*?)</STContent>"#).expect("valid regex"));

/// Extract text from CDATA sections: `<![CDATA[...]]>`
/// Group 1: the content inside CDATA.
static CDATA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<!\[CDATA\[(.*?)]]>"#).expect("valid regex"));

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An ST code region extracted from the L5X file.
struct StRegion {
    /// The extracted ST source (CDATA lines concatenated with newlines)
    source: String,
    /// Line number (1-indexed) where the STContent starts in the original file
    line_start: u32,
    /// Context: parent routine name (if known)
    routine_name: Option<String>,
    /// Context: parent program name (if known)
    program_name: Option<String>,
}

/// Find the nearest preceding regex match before `byte_offset` in `source`.
fn find_nearest_before<'a>(re: &Regex, source: &'a str, byte_offset: usize) -> Option<&'a str> {
    let mut best: Option<regex::Match<'a>> = None;
    for m in re.find_iter(&source[..byte_offset]) {
        best = Some(m);
    }
    best.and_then(|m| re.captures(&source[m.start()..]))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

/// Count newlines in `source[..byte_offset]` to get 1-indexed line number.
fn line_of(source: &str, byte_offset: usize) -> u32 {
    source[..byte_offset]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Extract all ST regions from an L5X file.
fn extract_st_regions(source: &str) -> Vec<StRegion> {
    let mut regions = Vec::new();

    for st_match in ST_CONTENT_RE.captures_iter(source) {
        let full = st_match.get(0).unwrap();
        let inner = st_match.get(1).unwrap();
        let start_byte = full.start();
        let line_start = line_of(source, start_byte);

        // Extract CDATA lines and join with newlines
        let mut lines = Vec::new();
        for cdata in CDATA_RE.captures_iter(inner.as_str()) {
            if let Some(content) = cdata.get(1) {
                lines.push(content.as_str().to_string());
            }
        }

        if lines.is_empty() {
            continue;
        }

        let st_source = lines.join("\n");

        // Find context: nearest Routine and Program names before this STContent
        let routine_name =
            find_nearest_before(&ROUTINE_RE, source, start_byte).map(|s| s.to_string());
        let program_name =
            find_nearest_before(&PROGRAM_RE, source, start_byte).map(|s| s.to_string());

        regions.push(StRegion {
            source: st_source,
            line_start,
            routine_name,
            program_name,
        });
    }

    regions
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse an L5X file and extract ST code chunks.
///
/// Extracts CDATA content from STContent blocks, parses each as Structured Text,
/// and maps chunks back to original file coordinates.
pub(crate) fn parse_l5x_chunks(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<Vec<Chunk>, ParserError> {
    let _span = tracing::info_span!("parse_l5x", path = %path.display()).entered();

    let regions = extract_st_regions(source);
    if regions.is_empty() {
        tracing::debug!("No ST content found in L5X file");
        return Ok(vec![]);
    }

    let st_lang = Language::StructuredText;
    let grammar = st_lang
        .try_grammar()
        .ok_or_else(|| ParserError::ParseFailed("Structured Text grammar not available".into()))?;
    let query = parser.get_query(st_lang)?;

    let mut all_chunks = Vec::new();
    let file_path: PathBuf = path.into();

    for region in &regions {
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
            match extract_st_chunk(&region.source, m, query, st_lang, &file_path, region) {
                Ok(chunk) => all_chunks.push(chunk),
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to extract ST chunk from L5X");
                }
            }
        }

        // If no chunks were extracted from this region but we have a routine name,
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
                    file: file_path.to_path_buf(),
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

    tracing::info!(
        chunks = all_chunks.len(),
        regions = regions.len(),
        "L5X parse complete"
    );
    Ok(all_chunks)
}

/// Extract a single chunk from an ST tree-sitter match, adjusting coordinates
/// for the L5X file's original line numbers.
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

    // Apply post-process hook if the ST language has one
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_extract_st_regions() {
        let regions = extract_st_regions(SAMPLE_L5X);
        assert_eq!(regions.len(), 1, "Should find exactly one STContent block");
        assert_eq!(regions[0].routine_name.as_deref(), Some("MainRoutine"));
        assert_eq!(regions[0].program_name.as_deref(), Some("MainProgram"));
        assert!(regions[0].source.contains("myTimer"));
        assert!(regions[0].source.contains("END_IF"));
        // Should NOT contain ladder logic CDATA
        assert!(!regions[0].source.contains("XIC"));
    }

    #[test]
    fn test_cdata_extraction() {
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
    fn test_find_nearest_before() {
        let source = r#"<Program Name="Prog1"><Program Name="Prog2"><STContent>"#;
        let name = find_nearest_before(&PROGRAM_RE, source, source.len());
        assert_eq!(name, Some("Prog2"));
    }

    #[test]
    fn test_parse_l5x_finds_chunks() {
        let parser = Parser::new().unwrap();
        let path = Path::new("test.l5x");
        let chunks = parse_l5x_chunks(SAMPLE_L5X, path, &parser).unwrap();
        // Should find at least the MainRoutine (either via ST grammar or fallback)
        assert!(!chunks.is_empty(), "Expected at least one chunk from L5X");
        // All chunks should reference ST language
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::StructuredText);
        }
    }

    #[test]
    fn test_no_st_content() {
        let source = r#"<?xml version="1.0"?>
<RSLogix5000Content>
  <Controller Name="Empty">
    <Programs/>
  </Controller>
</RSLogix5000Content>"#;
        let parser = Parser::new().unwrap();
        let chunks = parse_l5x_chunks(source, Path::new("empty.l5x"), &parser).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_ladder_only_skipped() {
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
        let regions = extract_st_regions(source);
        assert!(
            regions.is_empty(),
            "Ladder-only files should have no ST regions"
        );
    }
}
