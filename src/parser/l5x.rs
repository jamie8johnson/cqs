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

use super::types::{
    capture_name_to_chunk_type, Chunk, ChunkType, ChunkTypeRefs, FunctionCalls, Language,
    ParserError, TypeRef,
};
use super::ParseAllResult;
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
    /// File-relative byte offset of where the region begins in the original
    /// file. Each chunk's region-relative `byte_start` is lifted by this base
    /// so chunk ids stay injective ACROSS regions: two regions can begin on the
    /// same file line and contain byte-identical leading ST, which would
    /// otherwise collide on (line_start, region-relative byte_start,
    /// content_hash). Regions occupy distinct byte offsets, so the lifted
    /// `byte_start` is unique per chunk file-wide.
    byte_offset: u32,
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
                // Clamp on the `usize -> u32` cast so a pathological file with
                // >4 B lines saturates instead of silently truncating to a
                // tiny line_count.
                let line_count: u32 = content.lines().count().try_into().unwrap_or(u32::MAX);
                let sig = content.lines().next().unwrap_or("").to_string();
                let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
                // No tree-sitter node for the synthetic whole-routine chunk —
                // fall back to whitespace-collapse only.
                let canonical = super::chunk::canonical_hash_fallback(&content);
                all_chunks.push(Chunk {
                    // Synthetic whole-routine chunk: one per region. Use the
                    // region's file-relative base offset (not 0) so two regions
                    // beginning on the same file line with byte-identical source
                    // get distinct ids — region.line_start alone does NOT
                    // disambiguate when regions share a file line.
                    id: super::chunk::chunk_id(
                        &path.display().to_string(),
                        region.line_start,
                        region.byte_offset,
                        &content_hash,
                    ),
                    name: name.clone(),
                    chunk_type: ChunkType::Function,
                    content,
                    file: path.to_path_buf(),
                    line_start: region.line_start,
                    byte_start: region.byte_offset,
                    // line_end is inclusive 1-indexed. saturating_add so a high
                    // `region.line_start` plus a large line_count clamps at
                    // u32::MAX instead of overflowing.
                    line_end: region
                        .line_start
                        .saturating_add(line_count.saturating_sub(1)),
                    language: st_lang,
                    signature: sig,
                    doc: None,
                    canonical_hash: canonical,
                    content_hash,
                    parent_id: None,
                    window_idx: None,
                    parent_type_name: region.program_name.clone(),
                    parser_version: super::chunk::PARSER_VERSION,
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
    // tree-sitter row indices are usize internally; the `as u32` cast would
    // silently truncate on files >4 B lines. `try_into + unwrap_or(u32::MAX)`
    // clamps the cast so the saturating_add below reaches u32::MAX
    // deterministically rather than wrapping.
    let start_row: u32 = node.start_position().row.try_into().unwrap_or(u32::MAX);
    let end_row: u32 = node.end_position().row.try_into().unwrap_or(u32::MAX);
    // saturating_add so `region.line_start + tree-sitter row` clamps at
    // u32::MAX on a pathological L5X (large `Routine` with many rungs and a
    // high `region.line_start` offset). Without it, an overflow produces a
    // chunk with `line_start ≈ 0`, `line_end ≈ u32::MAX` — a nonsense span
    // that corrupts the index for that chunk.
    let line_start = region.line_start.saturating_add(start_row);
    let line_end = region.line_start.saturating_add(end_row);

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
    // Tree-precise: the ST node is available, so strip comment descendants.
    let canonical = super::chunk::canonical_hash(node, &content);
    // Lift the region-relative start byte to a file-relative offset by adding
    // the region's base offset. Region source is reconstructed from independent
    // CDATA matches with no enforced line-disjointness, so two regions can begin
    // on the same file line and carry byte-identical leading ST — colliding on
    // (line_start, region-relative byte_start, content_hash) and dropping a
    // chunk via the id PRIMARY KEY. The lift makes byte_start unique per chunk
    // file-wide because regions occupy distinct byte offsets. saturating_add
    // clamps at u32::MAX on a pathological file rather than wrapping.
    let region_byte_start: u32 = node.byte_range().start.try_into().unwrap_or(u32::MAX);
    let byte_start = region.byte_offset.saturating_add(region_byte_start);

    Ok(Chunk {
        id: super::chunk::chunk_id(
            &file_path.display().to_string(),
            line_start,
            byte_start,
            &content_hash,
        ),
        name: mutable_name,
        chunk_type: mutable_type,
        content,
        file: file_path.to_path_buf(),
        line_start,
        line_end,
        byte_start,
        language,
        signature,
        doc: None,
        canonical_hash: canonical,
        content_hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: region.program_name.clone(),
        parser_version: super::chunk::PARSER_VERSION,
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
        // group 0 is the full match (always present when an iter yields a
        // Captures); group 1 is the unconditional capture in
        // `<STContent>(.*?)</STContent>`. `.expect()` documents the invariant
        // — a regex tweak that adds an alternation or optional group surfaces
        // here at panic time instead of hiding behind `.unwrap()`.
        let full = st_match.get(0).expect("captures group 0 always present");
        let inner = st_match
            .get(1)
            .expect("L5X_ST_CONTENT_RE: group 1 is unconditional");
        let start_byte = full.start();
        let line_start = line_of(source, start_byte);
        // File-relative base offset for lifting each chunk's region-relative
        // byte_start. Clamp the usize->u32 cast so a >4 GB file saturates.
        let byte_offset: u32 = start_byte.try_into().unwrap_or(u32::MAX);

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
            byte_offset,
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
///
/// `[^\x00]*?` is non-greedy but still scans forward through
/// the entire remaining input when `END_ROUTINE` is missing. On malformed
/// files with many unterminated ROUTINE blocks the cost is
/// O(N * unterminated_blocks). The regex crate's linear-time guarantee
/// prevents catastrophic backtracking, but the constant factor is high
/// for large inputs. A streaming/line-based parser would avoid this.
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
        // groups 1 and 2 are unconditional captures in L5K_ROUTINE_BLOCK_RE
        // (`(\w+)\b([^\x00]*?)`); group 0 is always present on an iter
        // Captures. `.expect()` over `.unwrap()` makes the invariant survive a
        // regex refactor at panic message time.
        let routine_name = block
            .get(1)
            .expect("L5K_ROUTINE_BLOCK_RE: group 1 (routine name) unconditional")
            .as_str()
            .to_string();
        let block_content = block
            .get(2)
            .expect("L5K_ROUTINE_BLOCK_RE: group 2 (block content) unconditional")
            .as_str();
        let block_start = block
            .get(0)
            .expect("captures group 0 always present")
            .start();

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
        // File-relative base offset for lifting each chunk's region-relative
        // byte_start. Clamp the usize->u32 cast so a >4 GB file saturates.
        let byte_offset: u32 = block_start.try_into().unwrap_or(u32::MAX);

        // Try ST_CONTENT := [ ... ]; block first
        let st_source = if let Some(st_block) = L5K_ST_CONTENT_BLOCK_RE.captures(block_content) {
            // group 1 is the unconditional `(.*?)` in
            // `ST_CONTENT\s*:=\s*\[(.*?)\]\s*;` — present whenever the outer
            // captures matched.
            let inner = st_block
                .get(1)
                .expect("L5K_ST_CONTENT_BLOCK_RE: group 1 (inner) unconditional")
                .as_str();
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
            byte_offset,
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
// Production entry point (custom_all_parser)
// ===========================================================================

/// Chunks-only extractor for Rockwell PLC exports.
///
/// Registered as `LanguageDef::custom_chunk_parser` so the chunks-only path
/// (`Parser::parse_source`) routes `.l5x`/`.l5k` to the ST extractor too,
/// keeping a single declarative routing surface (the old explicit `if
/// ext == "l5x"` block in `Parser::parse_file` is removed). Dispatches by
/// extension to the format-specific extractor.
pub fn parse_l5x_chunks_dispatch(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<Vec<Chunk>, ParserError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "l5k" => parse_l5k_chunks(source, path, parser),
        // Default to the XML form for "l5x" and any future alias routed here.
        _ => parse_l5x_chunks(source, path, parser),
    }
}

/// Combined chunks + calls + type-refs extractor for Rockwell PLC exports.
///
/// Registered as `LanguageDef::custom_all_parser` on the L5X/L5K language row,
/// so the production index path (`Parser::parse_file_all_inner`) routes
/// `.l5x`/`.l5k` here instead of the generic XML grammar. Dispatches by
/// extension to the format-specific chunk extractor (`parse_l5x_chunks` for the
/// XML form, `parse_l5k_chunks` for the legacy ASCII form), then derives
/// file-level call and type-reference relationships from each emitted chunk.
///
/// The chunks carry `Language::StructuredText`, whose definition has both a
/// call and a type query, so relationships are extracted with the same
/// per-chunk machinery the grammar-less indexing path independently runs for
/// `chunk_calls`. Calls are grouped under the chunk's name (the
/// `function_calls` table joins on `caller_name` + `file`); types are grouped
/// per chunk like `parse_aspx_all` does, with the chunk's own name filtered out
/// of its type-ref set.
pub fn parse_l5x_all(
    source: &str,
    path: &Path,
    parser: &Parser,
) -> Result<ParseAllResult, ParserError> {
    let _span = tracing::info_span!("parse_l5x_all", path = %path.display()).entered();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    let chunks = match ext.as_str() {
        "l5k" => parse_l5k_chunks(source, path, parser)?,
        // Default to the XML form for "l5x" and any future alias routed here.
        _ => parse_l5x_chunks(source, path, parser)?,
    };

    let mut all_calls: Vec<FunctionCalls> = Vec::new();
    let mut all_types: Vec<ChunkTypeRefs> = Vec::new();

    for chunk in &chunks {
        // Reuse the per-chunk extractors so file-level relationships match the
        // `chunk_calls`/candidate path the grammar-less indexing route runs.
        let calls = parser.extract_calls_from_chunk(chunk);
        if !calls.is_empty() {
            all_calls.push(FunctionCalls {
                name: chunk.name.clone(),
                line_start: chunk.line_start,
                calls,
            });
        }

        let type_refs = extract_chunk_type_refs(chunk, parser);
        if !type_refs.is_empty() {
            all_types.push(ChunkTypeRefs {
                name: chunk.name.clone(),
                line_start: chunk.line_start,
                type_refs,
            });
        }
    }

    tracing::info!(
        chunks = chunks.len(),
        calls = all_calls.len(),
        types = all_types.len(),
        "L5X/L5K parse_all complete"
    );

    Ok((chunks, all_calls, all_types))
}

/// Extract type references from a single ST chunk's content via a standalone
/// tree-sitter parse, dropping the chunk's own name (a self-reference is not a
/// dependency edge — same filter `parse_aspx_all` applies).
fn extract_chunk_type_refs(chunk: &Chunk, parser: &Parser) -> Vec<TypeRef> {
    let st_lang = Language::StructuredText;
    let grammar = match st_lang.try_grammar() {
        Some(g) => g,
        None => {
            tracing::warn!("Structured Text grammar unavailable; skipping ST type refs");
            return Vec::new();
        }
    };
    let mut ts_parser = tree_sitter::Parser::new();
    if ts_parser.set_language(&grammar).is_err() {
        tracing::warn!("Failed to set ST grammar for type-ref extraction");
        return Vec::new();
    }
    let tree = match ts_parser.parse(&chunk.content, None) {
        Some(t) => t,
        None => {
            tracing::warn!("ST type-ref parse returned no tree");
            return Vec::new();
        }
    };
    let mut refs = parser.extract_types(&chunk.content, &tree, st_lang, 0, chunk.content.len());
    refs.retain(|t| t.type_name != chunk.name);
    refs
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

    /// FILE-WIDE INJECTIVITY (the byte_start lift).
    ///
    /// Two `<STContent>` regions begin on the SAME file line and carry
    /// byte-identical leading ST. With a region-relative `byte_start` both
    /// chunks would share (line_start, byte_start, content_hash) → identical
    /// id → one chunk dropped by the `chunks.id` PRIMARY KEY. Lifting
    /// `byte_start` to a file-relative offset (regions occupy distinct byte
    /// offsets) separates them.
    ///
    /// RED before the lift: revert `byte_start` to `node.byte_range().start`
    /// (and the synthetic chunk to 0) and this asserts a collision.
    #[test]
    fn test_l5x_same_line_regions_ids_injective() {
        // Two ST routines whose `<STContent>` tags sit on ONE physical line,
        // each containing byte-identical ST (`x := 1;`). The two `<Routine>`
        // wrappers and `<STContent>` blocks are on the same source line, so
        // both regions get the same `line_start`; the ST is identical, so the
        // region-relative byte_start and content_hash also match pre-lift.
        let source = concat!(
            "<?xml version=\"1.0\"?>\n",
            "<RSLogix5000Content><Controller Name=\"C\"><Programs><Program Name=\"P\"><Routines>",
            // both routines + their STContent on the SAME line:
            "<Routine Name=\"A\" Type=\"ST\"><STContent><Line Number=\"0\"><![CDATA[x := 1;]]></Line></STContent></Routine>",
            "<Routine Name=\"B\" Type=\"ST\"><STContent><Line Number=\"0\"><![CDATA[x := 1;]]></Line></STContent></Routine>",
            "</Routines></Program></Programs></Controller></RSLogix5000Content>\n",
        );

        let regions = extract_l5x_regions(source);
        assert_eq!(regions.len(), 2, "expected two ST regions");
        assert_eq!(
            regions[0].line_start, regions[1].line_start,
            "test precondition: both regions must start on the same file line"
        );
        assert_eq!(
            regions[0].source, regions[1].source,
            "test precondition: both regions must have byte-identical ST"
        );
        assert_ne!(
            regions[0].byte_offset, regions[1].byte_offset,
            "regions must occupy distinct file byte offsets (the disambiguator)"
        );

        let parser = Parser::new().unwrap();
        let chunks = parse_l5x_chunks(source, Path::new("twins.l5x"), &parser).unwrap();
        assert!(
            chunks.len() >= 2,
            "expected at least two chunks (one per region), got {}",
            chunks.len()
        );

        let mut seen = std::collections::HashSet::new();
        for c in &chunks {
            assert!(
                seen.insert(c.id.clone()),
                "L5X chunk id collision: {} reused by a second distinct chunk \
                 (name={:?}, line_start={}, byte_start={}) — file-wide \
                 injectivity violated",
                c.id,
                c.name,
                c.line_start,
                c.byte_start,
            );
        }
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

    // --- Audit finding tests ---

    /// `<STContent>` present but empty (no CDATA inside).
    /// extract_l5x_regions must produce zero regions, not panic or produce
    /// an empty-source region.
    #[test]
    fn test_l5x_stcontent_present_but_empty() {
        let source = r#"<?xml version="1.0" encoding="UTF-8"?>
<RSLogix5000Content>
  <Controller Name="C1">
    <Programs>
      <Program Name="P1">
        <Routines>
          <Routine Name="EmptyRoutine" Type="ST">
            <STContent>
            </STContent>
          </Routine>
        </Routines>
      </Program>
    </Programs>
  </Controller>
</RSLogix5000Content>"#;
        let regions = extract_l5x_regions(source);
        assert!(
            regions.is_empty(),
            "STContent with no CDATA should produce zero regions, got {}",
            regions.len()
        );

        // Also verify it does not panic when fed through the full parser
        let parser = Parser::new().unwrap();
        let chunks = parse_l5x_chunks(source, Path::new("empty_st.l5x"), &parser).unwrap();
        assert!(chunks.is_empty());
    }

    /// L5K ST type check only scans `.take(5)` lines of block content.
    /// If the `Type := ST` declaration appears on line 6 or later (after
    /// DESCRIPTION, tags, comments, etc.), the routine is silently skipped.
    #[test]
    fn test_l5k_type_declaration_beyond_line_5_is_missed() {
        let source = "
PROGRAM Prog1
  ROUTINE DeepTypeDecl
    DESCRIPTION := \"A routine with many preamble lines\"
    TAG tag1 := DINT
    TAG tag2 := BOOL
    TAG tag3 := REAL
    TAG tag4 := STRING
    Type := ST
    ST_CONTENT := [
      x := 1;
      y := x + 2;
    ];
  END_ROUTINE
END_PROGRAM
";
        let regions = extract_l5k_regions(source);
        // The current implementation only checks .take(5) lines for the type
        // declaration. With 5 preamble lines before `Type := ST`, the routine
        // is NOT detected as ST. This test documents the limitation.
        assert!(
            regions.is_empty(),
            "Expected 0 regions because Type := ST is beyond the 5-line scan window, got {}",
            regions.len()
        );
    }

    /// Malformed/unclosed CDATA blocks.
    /// `<![CDATA[...` without a closing `]]>` should not match the CDATA regex,
    /// so the content is silently dropped. Verify no panic and zero regions.
    #[test]
    fn test_l5x_malformed_unclosed_cdata() {
        let source = r#"<?xml version="1.0"?>
<RSLogix5000Content>
  <Controller Name="C1">
    <Programs>
      <Program Name="P1">
        <Routines>
          <Routine Name="BadCdata" Type="ST">
            <STContent>
              <Line Number="0"><![CDATA[x := 1;</Line>
              <Line Number="1"><![CDATA[y := 2;]]></Line>
            </STContent>
          </Routine>
        </Routines>
      </Program>
    </Programs>
  </Controller>
</RSLogix5000Content>"#;
        let regions = extract_l5x_regions(source);
        // Line 0 has unclosed CDATA (no `]]>`), so the CDATA regex skips it.
        // Line 1 is well-formed, so we should get exactly one region with
        // only the second line's content.
        assert_eq!(
            regions.len(),
            1,
            "Should still extract region from valid CDATA"
        );
        assert!(
            !regions[0].source.contains("x := 1;"),
            "Unclosed CDATA content should not appear in extracted source"
        );
        assert!(
            regions[0].source.contains("y := 2;"),
            "Valid CDATA on line 1 should be extracted"
        );

        // Full parser should not panic either
        let parser = Parser::new().unwrap();
        let result = parse_l5x_chunks(source, Path::new("bad_cdata.l5x"), &parser);
        assert!(result.is_ok(), "Parser should not panic on malformed CDATA");
    }

    /// L5K_ROUTINE_BLOCK_RE uses `[^\x00]*?` which on malformed input
    /// (unterminated ROUTINE blocks with no END_ROUTINE) causes the regex
    /// engine to scan to the end of the input for each unmatched ROUTINE.
    /// Worst case: O(N * unterminated_blocks).
    ///
    /// This test documents the behavior: with no END_ROUTINE, the regex
    /// finds no matches (which is correct), but the time cost grows with
    /// input size. The `[^\x00]*?` pattern is non-greedy but still must
    /// attempt all positions before failing.
    #[test]
    fn test_l5k_unterminated_routine_no_panic() {
        // Build a moderately-sized malformed input: many ROUTINE keywords
        // with no matching END_ROUTINE
        let mut source = String::from("PROGRAM MalformedProg\n");
        for i in 0..20 {
            source.push_str(&format!(
                "  ROUTINE Orphan{i}\n    Type := ST\n    x := {i};\n"
            ));
            // Deliberately omit END_ROUTINE
        }
        source.push_str("END_PROGRAM\n");

        // Should not panic and should produce no regions (no END_ROUTINE
        // to close any block). The regex simply fails to match.
        let regions = extract_l5k_regions(&source);
        // NOTE: The regex requires END_ROUTINE to close a block. Without it,
        // no captures are produced -- but the engine still scans the full
        // input for each ROUTINE keyword. On very large malformed files this
        // is O(N * unterminated_blocks).
        assert!(
            regions.is_empty(),
            "Unterminated ROUTINE blocks should produce no regions"
        );
    }

    /// L5X CRLF ordering invariant.
    /// Windows-originated L5X files use CRLF line endings. Verify that:
    /// 1. CDATA extraction works with CRLF
    /// 2. Line counting (`line_of`) handles CRLF correctly
    /// 3. Extracted source is usable (ST parser doesn't choke on \r)
    #[test]
    fn test_l5x_crlf_line_endings() {
        // Build the same L5X sample but with CRLF endings
        let source = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\
            <RSLogix5000Content>\r\n\
            <Controller Name=\"C1\">\r\n\
            <Programs>\r\n\
            <Program Name=\"CrlfProg\">\r\n\
            <Routines>\r\n\
            <Routine Name=\"CrlfRoutine\" Type=\"ST\">\r\n\
            <STContent>\r\n\
            <Line Number=\"0\"><![CDATA[myVar := 10;]]></Line>\r\n\
            <Line Number=\"1\"><![CDATA[IF myVar > 5 THEN]]></Line>\r\n\
            <Line Number=\"2\"><![CDATA[  output := TRUE;]]></Line>\r\n\
            <Line Number=\"3\"><![CDATA[END_IF;]]></Line>\r\n\
            </STContent>\r\n\
            </Routine>\r\n\
            </Routines>\r\n\
            </Program>\r\n\
            </Programs>\r\n\
            </Controller>\r\n\
            </RSLogix5000Content>\r\n";

        let regions = extract_l5x_regions(source);
        assert_eq!(regions.len(), 1, "CRLF source should produce one region");
        assert_eq!(
            regions[0].routine_name.as_deref(),
            Some("CrlfRoutine"),
            "Routine name extraction must work with CRLF"
        );
        assert_eq!(
            regions[0].program_name.as_deref(),
            Some("CrlfProg"),
            "Program name extraction must work with CRLF"
        );
        assert!(
            regions[0].source.contains("myVar := 10;"),
            "CDATA content must be extracted with CRLF line endings"
        );
        assert!(
            regions[0].source.contains("END_IF;"),
            "Multi-line CDATA extraction must work with CRLF"
        );

        // Verify line_start is reasonable (CRLF has same \n count as LF)
        assert!(regions[0].line_start > 0, "line_start should be positive");

        // Full parser should handle CRLF source without error
        let parser = Parser::new().unwrap();
        let chunks = parse_l5x_chunks(source, Path::new("crlf.l5x"), &parser).unwrap();
        assert!(!chunks.is_empty(), "CRLF L5X source should produce chunks");
        for chunk in &chunks {
            assert_eq!(chunk.language, Language::StructuredText);
        }
    }

    /// Synthetic-chunk path must saturate on overflow.
    ///
    /// When the routine has a name but tree-sitter produces no matches
    /// (no parseable ST inside), the synthetic-chunk fallback computes
    /// `line_end = region.line_start + (line_count - 1)`. With a high
    /// `region.line_start` (near `u32::MAX`) and a multi-line region,
    /// `saturating_add` clamps at `u32::MAX` and produces a valid chunk
    /// instead of overflowing.
    #[test]
    fn test_l5x_synthetic_chunk_saturates_on_line_overflow() {
        // The fallback only triggers when the ST source has *no* tree-sitter
        // matches. Use a comment-only payload so `parse_st_regions` produces
        // zero matches and falls through to the synthetic path.
        let source = "// just a comment\n// no statements\n// nothing parseable";
        let regions = vec![StRegion {
            source: source.to_string(),
            line_start: u32::MAX - 1, // forces overflow on `+ (line_count - 1)`
            byte_offset: 0,
            routine_name: Some("OverflowRoutine".to_string()),
            program_name: Some("OverflowProg".to_string()),
        }];

        let parser = Parser::new().unwrap();
        let chunks = parse_st_regions(&regions, Path::new("overflow.l5x"), &parser)
            .expect("parse_st_regions must not panic on near-MAX line_start");

        assert_eq!(
            chunks.len(),
            1,
            "Synthetic fallback should produce exactly one chunk"
        );
        let c = &chunks[0];
        assert_eq!(
            c.line_start,
            u32::MAX - 1,
            "line_start should match region.line_start"
        );
        assert_eq!(
            c.line_end,
            u32::MAX,
            "line_end must saturate at u32::MAX, not wrap"
        );
        assert!(
            c.line_end >= c.line_start,
            "line_end must be >= line_start (no wrap)"
        );
    }

    /// tree-sitter chunk path must saturate on overflow. With a high
    /// `region.line_start` and a tree-sitter row index,
    /// `saturating_add` and a clamping cast keep both line bounds at
    /// `u32::MAX` instead of overflowing.
    #[test]
    fn test_l5x_tree_sitter_chunk_saturates_on_line_overflow() {
        // Real ST function so the tree-sitter parser produces a chunk.
        let source = "FUNCTION_BLOCK MyFB\nVAR x : INT; END_VAR\nx := 1;\nEND_FUNCTION_BLOCK";
        let regions = vec![StRegion {
            source: source.to_string(),
            line_start: u32::MAX, // any tree-sitter row added overflows u32
            byte_offset: 0,
            routine_name: Some("Saturate".to_string()),
            program_name: Some("Prog".to_string()),
        }];

        let parser = Parser::new().unwrap();
        let chunks = parse_st_regions(&regions, Path::new("saturate.l5x"), &parser)
            .expect("parse_st_regions must not panic on u32::MAX line_start");

        for c in &chunks {
            assert_eq!(
                c.line_start,
                u32::MAX,
                "line_start must saturate at u32::MAX, not wrap"
            );
            assert_eq!(
                c.line_end,
                u32::MAX,
                "line_end must saturate at u32::MAX, not wrap"
            );
        }
    }

    // --- Production-path wiring tests ---
    //
    // These exercise the PRODUCTION entry `Parser::parse_file_all` (NOT
    // `parse_file`), which routes by `Language::from_extension`. Before the
    // wireup, `.l5x`/`.l5k` resolved to `Language::Xml` (generic XML grammar)
    // and yielded XML-element chunks; after, they resolve to the grammar-less
    // L5X def and yield Structured-Text routine chunks.

    /// Write `content` to a temp file with the given extension so the
    /// extension-based production router sees a real `.l5x`/`.l5k` path.
    fn write_temp_with_ext(content: &str, ext: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{ext}"))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    /// PRODUCTION PATH: `parse_file_all` on a `.l5x` file must return the
    /// custom ST routine chunk, not generic XML-element chunks.
    ///
    /// FAIL-BEFORE: with `.l5x` registered under `LANG_XML`, this path resolved
    /// to the XML grammar and produced chunks whose language is `Xml` and whose
    /// names are XML element/attribute names (`Routine`, `STContent`, ...),
    /// never a `StructuredText` chunk named `MainRoutine`.
    #[test]
    fn test_l5x_production_path_yields_st_chunks() {
        let parser = Parser::new().unwrap();
        let f = write_temp_with_ext(SAMPLE_L5X, "l5x");
        let (chunks, _calls, _types) = parser.parse_file_all(f.path()).unwrap();

        assert!(
            !chunks.is_empty(),
            "production parse_file_all must produce chunks for .l5x"
        );
        assert!(
            chunks
                .iter()
                .all(|c| c.language == Language::StructuredText),
            "every production chunk must be StructuredText, not Xml: got {:?}",
            chunks.iter().map(|c| c.language).collect::<Vec<_>>()
        );
        assert!(
            chunks.iter().any(|c| c.name == "MainRoutine"),
            "expected the ST routine 'MainRoutine'; got {:?}",
            chunks.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        // The ladder (RLL) routine carries no ST and must NOT surface as a chunk.
        assert!(
            !chunks.iter().any(|c| c.content.contains("XIC")),
            "RLL ladder content must not be extracted as ST"
        );
    }

    /// PRODUCTION PATH: `parse_file_all` on a `.l5k` file must return the
    /// custom ST routine chunk, not generic XML-element chunks.
    ///
    /// FAIL-BEFORE: `.l5k` was an XML extension, so the XML grammar parsed this
    /// ASCII file (yielding either no chunks or junk element chunks), never a
    /// `StructuredText` chunk named `MainRoutine`.
    #[test]
    fn test_l5k_production_path_yields_st_chunks() {
        let parser = Parser::new().unwrap();
        let f = write_temp_with_ext(SAMPLE_L5K, "l5k");
        let (chunks, _calls, _types) = parser.parse_file_all(f.path()).unwrap();

        assert!(
            !chunks.is_empty(),
            "production parse_file_all must produce chunks for .l5k"
        );
        assert!(
            chunks
                .iter()
                .all(|c| c.language == Language::StructuredText),
            "every production chunk must be StructuredText, not Xml: got {:?}",
            chunks.iter().map(|c| c.language).collect::<Vec<_>>()
        );
        assert!(
            chunks.iter().any(|c| c.name == "MainRoutine"),
            "expected the ST routine 'MainRoutine'; got {:?}",
            chunks.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    // --- Relationship-extraction tests (`parse_l5x_all` calls + type refs) ---
    //
    // The production parse_file_all path emits a third and fourth value
    // (calls, types) alongside chunks; the chunk-only production tests above
    // discard them. These exercise the call-graph + type-ref surface
    // (`parse_l5x_all` -> `extract_calls_from_chunk` / `extract_chunk_type_refs`)
    // that powers callers/impact/dead for PLC code.
    //
    // The ST grammar only yields `call_expression` / typed-`var_decl_item`
    // nodes when statements sit inside a program unit (FUNCTION_BLOCK /
    // PROGRAM / FUNCTION), so these routine bodies carry the wrapper an
    // Add-On-Instruction export does. A bare statement list parses to an
    // ERROR node and yields no relationships — covered by the malformed test.

    /// An L5X ST routine whose body is a FUNCTION_BLOCK with calls and typed
    /// declarations: `parse_l5x_all` must surface those calls and type refs,
    /// grouped under the chunk name, with syntactic `Call` edge kind.
    #[test]
    fn test_l5x_parse_all_extracts_calls_and_types() {
        let source = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<RSLogix5000Content><Controller Name=\"C\"><Programs><Program Name=\"P\"><Routines>\n",
            "<Routine Name=\"PumpFB\" Type=\"ST\"><STContent>\n",
            "<Line Number=\"0\"><![CDATA[FUNCTION_BLOCK PumpFB]]></Line>\n",
            "<Line Number=\"1\"><![CDATA[VAR lvl : REAL; t : TON; END_VAR]]></Line>\n",
            "<Line Number=\"2\"><![CDATA[t(IN := startButton, PT := T#5s);]]></Line>\n",
            "<Line Number=\"3\"><![CDATA[lvl := ReadLevel(sensor);]]></Line>\n",
            "<Line Number=\"4\"><![CDATA[StartPump(speed := 100);]]></Line>\n",
            "<Line Number=\"5\"><![CDATA[END_FUNCTION_BLOCK]]></Line>\n",
            "</STContent></Routine>\n",
            "</Routines></Program></Programs></Controller></RSLogix5000Content>",
        );
        let parser = Parser::new().unwrap();
        let (chunks, calls, types) = parse_l5x_all(source, Path::new("pump.l5x"), &parser).unwrap();

        assert_eq!(chunks.len(), 1, "expected one ST routine chunk");
        assert_eq!(chunks[0].name, "PumpFB");

        // Calls are grouped under the chunk's name (the function_calls join key).
        assert_eq!(
            calls.len(),
            1,
            "calls grouped under one chunk, got {calls:?}"
        );
        assert_eq!(calls[0].name, "PumpFB");
        let callees: Vec<&str> = calls[0]
            .calls
            .iter()
            .map(|c| c.callee_name.as_str())
            .collect();
        assert!(
            callees.contains(&"t"),
            "FB-instance invocation `t(...)` must be a call edge; got {callees:?}"
        );
        assert!(
            callees.contains(&"ReadLevel"),
            "expression-context call `ReadLevel(...)` must be a call edge; got {callees:?}"
        );
        assert!(
            callees.contains(&"StartPump"),
            "statement call `StartPump(...)` must be a call edge; got {callees:?}"
        );
        // Pin the call-graph edge provenance: ST routine calls are syntactic.
        for cs in &calls[0].calls {
            assert_eq!(
                cs.kind,
                crate::parser::types::CallEdgeKind::Call,
                "ST routine calls are syntactic, not heuristic: {cs:?}"
            );
        }

        // Typed declarations surface as type refs grouped under the chunk name.
        assert_eq!(
            types.len(),
            1,
            "types grouped under one chunk, got {types:?}"
        );
        assert_eq!(types[0].name, "PumpFB");
        let type_names: Vec<&str> = types[0]
            .type_refs
            .iter()
            .map(|t| t.type_name.as_str())
            .collect();
        assert!(
            type_names.contains(&"REAL"),
            "basic data type REAL must be a type ref; got {type_names:?}"
        );
        assert!(
            type_names.contains(&"TON"),
            "derived data type TON must be a type ref; got {type_names:?}"
        );
    }

    /// The chunk's own name must be filtered out of its type-ref set
    /// (a self-reference is not a dependency edge — the `extract_chunk_type_refs`
    /// retain filter). With `FUNCTION_BLOCK DerivedFB EXTENDS BaseFB`, `BaseFB`
    /// is an Impl edge and `DerivedFB` (the definition's own name) is dropped.
    #[test]
    fn test_l5x_parse_all_filters_self_type_ref() {
        let source = concat!(
            "<?xml version=\"1.0\"?>\n",
            "<RSLogix5000Content><Controller Name=\"C\"><Programs><Program Name=\"P\"><Routines>\n",
            "<Routine Name=\"DerivedFB\" Type=\"ST\"><STContent>\n",
            "<Line Number=\"0\"><![CDATA[FUNCTION_BLOCK DerivedFB EXTENDS BaseFB]]></Line>\n",
            "<Line Number=\"1\"><![CDATA[VAR x : INT; END_VAR]]></Line>\n",
            "<Line Number=\"2\"><![CDATA[y := Compute(x);]]></Line>\n",
            "<Line Number=\"3\"><![CDATA[END_FUNCTION_BLOCK]]></Line>\n",
            "</STContent></Routine>\n",
            "</Routines></Program></Programs></Controller></RSLogix5000Content>",
        );
        let parser = Parser::new().unwrap();
        let (_chunks, calls, types) =
            parse_l5x_all(source, Path::new("derived.l5x"), &parser).unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "DerivedFB");
        let callees: Vec<&str> = calls[0]
            .calls
            .iter()
            .map(|c| c.callee_name.as_str())
            .collect();
        assert_eq!(callees, vec!["Compute"], "single call edge expected");

        assert_eq!(types.len(), 1);
        let type_names: Vec<&str> = types[0]
            .type_refs
            .iter()
            .map(|t| t.type_name.as_str())
            .collect();
        assert!(
            type_names.contains(&"BaseFB"),
            "EXTENDS base must be a type ref (Impl edge); got {type_names:?}"
        );
        assert!(
            !type_names.contains(&"DerivedFB"),
            "chunk's own name must be filtered from its type-ref set; got {type_names:?}"
        );
        // The base must carry the Impl classification (FB EXTENDS).
        let base = types[0]
            .type_refs
            .iter()
            .find(|t| t.type_name == "BaseFB")
            .expect("BaseFB ref must exist");
        assert_eq!(
            base.kind,
            Some(crate::parser::types::TypeEdgeKind::Impl),
            "EXTENDS base must classify as Impl; got {:?}",
            base.kind
        );
    }

    /// L5K (legacy ASCII) routine bodies route through the same per-chunk
    /// extractor: a FUNCTION_BLOCK inside an ST_CONTENT block must yield calls
    /// and type refs, so `parse_l5x_all`'s L5K branch is exercised too.
    #[test]
    fn test_l5k_parse_all_extracts_calls_and_types() {
        let source = concat!(
            "\nPROGRAM MainProgram\n",
            "  ROUTINE WrappedFB\n",
            "    Type := ST\n",
            "    ST_CONTENT := [\n",
            "      FUNCTION_BLOCK WrappedFB\n",
            "      VAR n : DINT; END_VAR\n",
            "      z := Tally(n);\n",
            "      END_FUNCTION_BLOCK\n",
            "    ];\n",
            "  END_ROUTINE\n",
            "END_PROGRAM\n",
        );
        let parser = Parser::new().unwrap();
        let (chunks, calls, types) =
            parse_l5x_all(source, Path::new("wrapped.l5k"), &parser).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "WrappedFB");

        assert_eq!(calls.len(), 1, "got {calls:?}");
        assert_eq!(calls[0].name, "WrappedFB");
        let callees: Vec<&str> = calls[0]
            .calls
            .iter()
            .map(|c| c.callee_name.as_str())
            .collect();
        assert_eq!(callees, vec!["Tally"], "L5K call edge expected");

        assert_eq!(types.len(), 1, "got {types:?}");
        let type_names: Vec<&str> = types[0]
            .type_refs
            .iter()
            .map(|t| t.type_name.as_str())
            .collect();
        assert!(
            type_names.contains(&"DINT"),
            "L5K typed declaration DINT must be a type ref; got {type_names:?}"
        );
    }

    /// Malformed / error-recovered ST must not panic and must still yield
    /// partial relationships. The body has an unclosed call paren
    /// (`Mangle(q;`) and a dangling `IF r THEN` with no `END_IF`, forcing
    /// tree-sitter ERROR nodes. `parse_l5x_all` must return Ok and recover the
    /// extractable call/type fragments rather than dropping everything.
    #[test]
    fn test_l5x_parse_all_malformed_st_recovers_partial() {
        let source = concat!(
            "<?xml version=\"1.0\"?>\n",
            "<RSLogix5000Content><Controller Name=\"C\"><Programs><Program Name=\"P\"><Routines>\n",
            "<Routine Name=\"Broken\" Type=\"ST\"><STContent>\n",
            "<Line Number=\"0\"><![CDATA[FUNCTION_BLOCK Broken]]></Line>\n",
            "<Line Number=\"1\"><![CDATA[VAR q : BOOL; END_VAR]]></Line>\n",
            "<Line Number=\"2\"><![CDATA[r := Mangle(q;]]></Line>\n",
            "<Line Number=\"3\"><![CDATA[IF r THEN]]></Line>\n",
            "</STContent></Routine>\n",
            "</Routines></Program></Programs></Controller></RSLogix5000Content>",
        );
        let parser = Parser::new().unwrap();
        // Must not panic on tree-sitter error nodes.
        let result = parse_l5x_all(source, Path::new("broken.l5x"), &parser);
        let (chunks, calls, types) = result.expect("parse_l5x_all must not error on malformed ST");

        // The routine chunk still surfaces (the chunk path is independent of the
        // per-chunk relationship parse).
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "Broken");

        // Error recovery extracts the salvageable call fragment.
        assert_eq!(
            calls.len(),
            1,
            "partial call recovery expected; got {calls:?}"
        );
        let callees: Vec<&str> = calls[0]
            .calls
            .iter()
            .map(|c| c.callee_name.as_str())
            .collect();
        assert!(
            callees.contains(&"Mangle"),
            "the recoverable call `Mangle` must survive error recovery; got {callees:?}"
        );

        // And the typed declaration before the malformed line.
        assert_eq!(
            types.len(),
            1,
            "partial type recovery expected; got {types:?}"
        );
        let type_names: Vec<&str> = types[0]
            .type_refs
            .iter()
            .map(|t| t.type_name.as_str())
            .collect();
        assert!(
            type_names.contains(&"BOOL"),
            "the recoverable type `BOOL` must survive error recovery; got {type_names:?}"
        );
    }

    /// PRODUCTION PATH: `.l5x`/`.l5k` must resolve to the grammar-less L5X
    /// LanguageDef (not XML), so both `parse_file` and `parse_file_all` share a
    /// single declarative routing path.
    #[test]
    fn test_l5x_l5k_resolve_to_grammarless_l5x_def() {
        let lang_l5x = Language::from_extension("l5x").expect("l5x must resolve");
        let lang_l5k = Language::from_extension("l5k").expect("l5k must resolve");
        let def_l5x = lang_l5x.def();
        let def_l5k = lang_l5k.def();
        assert!(
            def_l5x.grammar.is_none(),
            "l5x must route to a grammar-less custom parser, not the XML grammar"
        );
        assert!(
            def_l5k.grammar.is_none(),
            "l5k must route to a grammar-less custom parser, not the XML grammar"
        );
        assert!(def_l5x.custom_all_parser.is_some());
        assert!(def_l5k.custom_all_parser.is_some());
        // And XML must NO LONGER claim these extensions.
        let xml = Language::from_extension("xml")
            .expect("xml must resolve")
            .def();
        assert!(
            !xml.extensions.contains(&"l5x"),
            "XML def must not claim .l5x after the wireup"
        );
        assert!(
            !xml.extensions.contains(&"l5k"),
            "XML def must not claim .l5k after the wireup"
        );
    }
}
