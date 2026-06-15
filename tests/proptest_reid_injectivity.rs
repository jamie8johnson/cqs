//! Property-based tests for chunk-id injectivity within a file.
//!
//! This file is a DIFFERENTLY-SHAPED auditor than the example suite. The
//! hand-written parser tests assert one source → expected chunks (happy/sad
//! examples). The bug this file guards is RELATIONAL across a whole file's
//! chunk set: the id `{path}:{line_start}:{byte_start}:{hash8}` must be
//! INJECTIVE — two byte-distinct chunks of one file must never share an id.
//! No single-example test expresses "no two of these collide"; that is a
//! property over the cross-product of a file's chunks.
//!
//! Before the `byte_start` disambiguator the id was `{path}:{line_start}:{hash8}`
//! and was NOT injective. Two reachable, real-parser cases collided.
//! First, `m!{} m!{}`: two macro invocations on ONE line are byte-identical, so
//! they share `line_start` AND the full `content_hash`; the id collapsed them,
//! the downstream `id_map` (HashMap of old id to new id) lost an entry, and the
//! `chunks.id` PRIMARY KEY UPSERT silently overwrote one chunk — a source region
//! dropped from the index with no error. Second, `struct A; impl A {}`: two
//! elements begin on line 1, so `line_start` alone never disambiguated same-line
//! siblings. `byte_start` (the element's start byte offset; tree-sitter nodes and
//! markdown sections occupy disjoint byte ranges) breaks both: distinct chunks
//! of a file have distinct start offsets.
//!
//! The generator below builds per-file chunk sets with DELIBERATE `line_start`
//! repeats and byte-identical-content duplicates (distinct only by byte span),
//! and asserts the id is injective over byte-span identity. It REDISCOVERS the
//! collision if the `byte_start` term is removed from `chunk_id`.
//!
//! Tuning: proptest runs 256 cases per property by default. Override with the
//! standard `PROPTEST_CASES` env var.

use std::collections::HashSet;
use std::io::Write;

use cqs::parser::{chunk_id, Parser};
use proptest::prelude::*;

/// A synthetic chunk coordinate: the inputs `chunk_id` folds into an id. Two
/// coordinates are "the same chunk" iff they share `(line_start, byte_start)` —
/// byte_start is the per-file-unique identity. The generator deliberately lets
/// `line_start` and `content_hash` repeat across DISTINCT byte_starts so the
/// property exercises exactly the collision the old format suffered.
#[derive(Debug, Clone)]
struct ChunkCoord {
    line_start: u32,
    byte_start: u32,
    content_hash: String,
}

/// Generate a set of chunk coordinates for ONE file with structural collisions
/// baked in:
///   - `line_start` drawn from a SMALL pool (0..=4) so repeats are frequent
///     (same-line siblings like `struct A; impl A {}`).
///   - `content_hash` drawn from a SMALL pool so byte-identical duplicates
///     occur (same-line macro twins `m!{} m!{}`).
///   - `byte_start` drawn over a wider range, then DEDUPED so each coordinate
///     has a unique byte_start — modelling distinct source elements (no two
///     elements share a start offset within a file).
fn chunk_coords() -> impl Strategy<Value = Vec<ChunkCoord>> {
    // Small hash pool: forces byte-identical-content collisions.
    let hash_pool = prop::sample::select(vec![
        "aaaaaaaa".to_string(),
        "bbbbbbbb".to_string(),
        "cccccccc".to_string(),
    ]);
    // line_start 0..=4, byte_start 0..=200, hash from the small pool.
    let one = (0u32..5, 0u32..201, hash_pool).prop_map(|(line_start, byte_start, content_hash)| {
        ChunkCoord {
            line_start,
            byte_start,
            content_hash,
        }
    });
    prop::collection::vec(one, 1..40).prop_map(|mut coords| {
        // Dedup by byte_start: a real file never has two distinct elements at
        // the same start offset, so the generator must not either. This keeps
        // the property honest — every retained coordinate is a genuinely
        // distinct chunk that injectivity MUST separate.
        let mut seen = HashSet::new();
        coords.retain(|c| seen.insert(c.byte_start));
        coords
    })
}

proptest! {
    /// INJECTIVITY: for one file path, distinct chunks (distinct byte_start)
    /// produce distinct ids. With `byte_start` in the format this holds even
    /// when `line_start` and `content_hash` both repeat. Remove `byte_start`
    /// from `chunk_id` and this property reds on the first same-line
    /// same-hash pair.
    #[test]
    fn chunk_id_injective_within_file(coords in chunk_coords()) {
        let path = "src/some/file.rs";
        let mut ids = HashSet::with_capacity(coords.len());
        for c in &coords {
            let id = chunk_id(path, c.line_start, c.byte_start, &c.content_hash);
            prop_assert!(
                ids.insert(id.clone()),
                "id collision: {id} produced by a second distinct chunk \
                 (coords with repeated line_start/hash but distinct byte_start \
                 must still get distinct ids)",
            );
        }
        // Sanity: every distinct coordinate got its own id.
        prop_assert_eq!(ids.len(), coords.len());
    }

    /// DETERMINISM / STABILITY: the same coordinates always produce the same
    /// id. (A re-index of unchanged source must not churn ids.)
    #[test]
    fn chunk_id_deterministic(
        line_start in 0u32..100_000,
        byte_start in 0u32..10_000_000,
        hash in "[0-9a-f]{8,64}",
    ) {
        let path = "a/b/c.rs";
        let a = chunk_id(path, line_start, byte_start, &hash);
        let b = chunk_id(path, line_start, byte_start, &hash);
        prop_assert_eq!(a, b);
    }

    /// Differs in byte_start ⇒ differs in id, holding everything else equal.
    /// This is the core disambiguation guarantee in isolation.
    #[test]
    fn chunk_id_byte_start_disambiguates(
        line_start in 0u32..1000,
        hash in "[0-9a-f]{8}",
        b1 in 0u32..1_000_000,
        b2 in 0u32..1_000_000,
    ) {
        prop_assume!(b1 != b2);
        let path = "x.rs";
        let id1 = chunk_id(path, line_start, b1, &hash);
        let id2 = chunk_id(path, line_start, b2, &hash);
        prop_assert_ne!(id1, id2);
    }
}

// ── Real-parser injectivity over the MARKDOWN path ──────────────────────────
//
// The `chunk_coords` property above asserts injectivity over the *arithmetic*
// of `chunk_id`, but its generator DEDUPS by `byte_start` (`coords.retain` on a
// `seen` set) — it bakes in the assumption "no two distinct chunks of a file
// share a start offset." That assumption is FALSE on the markdown path: a
// no-heading (or single-heading) file emits a whole-file/section chunk at
// `byte_start = 0, line_start = 1`, and `extract_table_chunks` then emits a
// table chunk for a table that begins at byte 0 — SAME `byte_start`, SAME
// `line_start`. The byte_start disambiguator collapses, and when the file is
// *exactly* one table with no trailing newline the whole-file `content_hash`
// also equals the table `content_hash` (the only textual difference is the
// trailing `\n` the whole-file chunk would otherwise carry). All four base id
// fields collide → two distinct chunks shared an id. The fix appends a `:t{idx}`
// suffix to every table chunk (`chunk_id_suffixed`), so the table id sits below
// the section id even when all four base fields match.
//
// The example suite cannot express this: it requires the precise combination
// {no heading} × {file IS exactly one table} × {no trailing newline}, and the
// arithmetic property's generator structurally excludes equal byte_starts.
//
// This property runs the REAL markdown parser over generated table-bearing
// documents and asserts whole-file id injectivity (every chunk a distinct id).

/// Generate markdown documents that stress the section/table byte_start overlap:
/// a (possibly absent) leading heading, then a table, optionally followed by a
/// trailing newline. Coverage claim: spans {0 heading, 1 heading} × {table at
/// byte 0, table after a heading} × {trailing newline present/absent} × varying
/// table widths and a leading-blank-line option — i.e. it deliberately VISITS
/// the {no-heading} × {table-at-byte-0} × {no-trailing-newline} corner the
/// arithmetic generator's byte_start-dedup can never reach.
///
/// DISTRUST: a generator that always appended a trailing `\n` would make the
/// whole-file/table content hashes differ and hide the collision; the
/// `trailing_nl: bool` dimension is the load-bearing axis, so it is sampled,
/// not fixed.
fn markdown_with_table() -> impl Strategy<Value = String> {
    (
        prop::option::of("[A-Za-z]{1,6}"), // optional leading heading text
        2usize..4,                         // table column count (>=2: detector needs pipes)
        1usize..4,                         // table data-row count
        any::<bool>(),                     // trailing newline?
        any::<bool>(),                     // blank line between heading and table?
    )
        .prop_map(|(heading, cols, rows, trailing_nl, blank)| {
            let mut s = String::new();
            if let Some(h) = heading {
                s.push_str(&format!("# {h}\n"));
                if blank {
                    s.push('\n');
                }
            }
            // Header row + separator + data rows.
            let header: String = (0..cols).map(|c| format!(" h{c} |")).collect();
            s.push('|');
            s.push_str(&header);
            s.push('\n');
            let sep: String = (0..cols).map(|_| " --- |").collect();
            s.push('|');
            s.push_str(&sep);
            s.push('\n');
            for r in 0..rows {
                let row: String = (0..cols).map(|c| format!(" r{r}c{c} |")).collect();
                s.push('|');
                s.push_str(&row);
                s.push('\n');
            }
            if !trailing_nl {
                // Drop the final newline we just appended.
                while s.ends_with('\n') {
                    s.pop();
                }
            }
            s
        })
}

proptest! {
    /// REAL-PARSER INJECTIVITY (markdown): every chunk emitted for a single
    /// markdown file must have a distinct id. The whole-file/section chunk and
    /// a table chunk that begins at the same byte must not collide.
    ///
    /// Was a production bug: a no-heading file that is exactly one table with no
    /// trailing newline yielded a whole-file chunk and a table chunk with
    /// identical `{path}:1:0:{hash8}` ids (and the table's `parent_id` pointed
    /// at itself), so the downstream `chunks.id` PRIMARY KEY UPSERT silently
    /// overwrote one of the two distinct chunks. The fix salts the table id with
    /// a `:t{idx}` suffix via `chunk_id_suffixed`. proptest shrinks straight to
    /// the minimal table, so it re-verifies the fix. Minimal case pinned below.
    #[test]
    fn markdown_chunk_ids_injective_within_file(doc in markdown_with_table()) {
        let file = write_temp(&doc, "md");
        let parser = Parser::new().expect("init parser");
        let chunks = parser.parse_file(file.path()).expect("parse");

        let mut seen: HashSet<&str> = HashSet::with_capacity(chunks.len());
        for c in &chunks {
            prop_assert!(
                seen.insert(c.id.as_str()),
                "markdown id collision: {} reused by a second distinct chunk \
                 (name={:?}, byte_start={}, parent_id={:?}); source:\n{}",
                c.id, c.name, c.byte_start, c.parent_id, doc,
            );
        }
    }
}

/// Minimal falsifier pin (production bug, not a too-strong property): a
/// no-heading markdown file that is EXACTLY one table with no trailing newline
/// emits a whole-file chunk and a table chunk with the SAME id. Both have
/// `line_start = 1`, `byte_start = 0`, and — because the only textual
/// difference would have been the trailing `\n` — the same `content_hash`. The
/// table chunk's `parent_id` then points at its own id.
///
/// The fix salts the table id with a `:t{idx}` suffix (`chunk_id_suffixed`), so
/// the whole-file section chunk (`{path}:1:0:{hash8}`) and the table chunk
/// (`{path}:1:0:{hash8}:t0`) no longer collide.
#[test]
fn markdown_whole_file_table_no_trailing_newline_ids_injective() {
    // Exactly one 2-col table, no heading, NO trailing newline.
    let src = "| a | b |\n|---|---|\n| 1 | 2 |";
    let file = write_temp(src, "md");
    let parser = Parser::new().expect("init parser");
    let chunks = parser.parse_file(file.path()).expect("parse");

    let mut seen: HashSet<String> = HashSet::new();
    let mut dup: Option<(String, String, String)> = None;
    for c in &chunks {
        if !seen.insert(c.id.clone()) {
            dup = Some((c.id.clone(), c.name.clone(), format!("{:?}", c.parent_id)));
        }
    }
    assert!(
        dup.is_none(),
        "whole-file chunk and table chunk share an id (injectivity violated): {dup:?}; \
         chunks = {:?}",
        chunks
            .iter()
            .map(|c| (c.id.as_str(), c.name.as_str()))
            .collect::<Vec<_>>(),
    );
}

// ── Real-parser injectivity over the L5X path ───────────────────────────────
//
// The L5X parser reconstructs each ST region's source from independent
// `<STContent>` CDATA matches with no enforced line-disjointness. Two regions
// can begin on the SAME file line and carry byte-identical leading ST, so a
// region-relative `byte_start` collided on (line_start, byte_start,
// content_hash) → identical id → one chunk dropped by the `chunks.id` PRIMARY
// KEY. The fix lifts each chunk's region-relative `byte_start` to a
// file-relative offset (regions occupy distinct byte offsets), restoring
// file-wide injectivity. The arithmetic `chunk_coords` property cannot express
// this: its generator DEDUPS by byte_start, structurally excluding the equal
// region-relative offsets two same-line byte-identical regions produce.

/// Generate L5X documents that stress the same-line region overlap: N ST
/// routines whose `<STContent>` tags all sit on ONE physical line, each
/// containing the SAME single ST statement. Pre-lift, the chunks share
/// (line_start, region-relative byte_start, content_hash) and collide; the
/// file-relative lift separates them by region byte offset.
///
/// Coverage: spans {2..=4 same-line routines} × {short ST statements} and
/// deliberately makes the ST byte-identical across routines — the corner the
/// arithmetic generator's byte_start-dedup can never reach.
fn l5x_same_line_regions() -> impl Strategy<Value = String> {
    (
        2usize..5,    // number of same-line ST routines
        "[a-z]{1,4}", // variable name (shared → byte-identical ST)
        prop::sample::select(vec!["1", "2", "TRUE", "FALSE"]),
    )
        .prop_map(|(n, var, rhs)| {
            let mut s = String::from(
                "<?xml version=\"1.0\"?>\n\
                 <RSLogix5000Content><Controller Name=\"C\"><Programs>\
                 <Program Name=\"P\"><Routines>",
            );
            // All routines + their <STContent> on a SINGLE source line, each
            // carrying byte-identical ST so line_start, region-relative
            // byte_start, and content_hash all collide pre-lift.
            for i in 0..n {
                s.push_str(&format!(
                    "<Routine Name=\"R{i}\" Type=\"ST\"><STContent>\
                     <Line Number=\"0\"><![CDATA[{var} := {rhs};]]></Line>\
                     </STContent></Routine>"
                ));
            }
            s.push_str("</Routines></Program></Programs></Controller></RSLogix5000Content>\n");
            s
        })
}

proptest! {
    /// REAL-PARSER INJECTIVITY (L5X): every chunk emitted for a single L5X file
    /// must have a distinct id, even when multiple ST regions begin on the same
    /// file line with byte-identical leading ST. Pre-lift, same-line
    /// byte-identical regions collided on (line_start, region-relative
    /// byte_start, content_hash); the file-relative byte_start lift separates
    /// them. proptest shrinks to the minimal two-region case, re-verifying the
    /// fix. Minimal case pinned below.
    #[test]
    fn l5x_chunk_ids_injective_within_file(doc in l5x_same_line_regions()) {
        let file = write_temp(&doc, "l5x");
        let parser = Parser::new().expect("init parser");
        let chunks = parser.parse_file(file.path()).expect("parse");

        let mut seen: HashSet<&str> = HashSet::with_capacity(chunks.len());
        for c in &chunks {
            prop_assert!(
                seen.insert(c.id.as_str()),
                "L5X id collision: {} reused by a second distinct chunk \
                 (name={:?}, line_start={}, byte_start={}); source:\n{}",
                c.id, c.name, c.line_start, c.byte_start, doc,
            );
        }
    }
}

/// Minimal falsifier pin (L5X same-line region collision): two ST routines
/// whose `<STContent>` tags share a file line and carry byte-identical ST. Pre
/// the file-relative `byte_start` lift, both chunks had id `{path}:N:0:{hash8}`
/// (region-relative byte_start, identical content_hash) — the second dropped by
/// the `chunks.id` PRIMARY KEY. The lift gives each region a distinct
/// file-relative `byte_start`, so the ids diverge.
#[test]
fn l5x_same_line_byte_identical_regions_ids_injective() {
    // Two ST routines + their STContent on ONE physical line, identical ST.
    let src = "<?xml version=\"1.0\"?>\n\
        <RSLogix5000Content><Controller Name=\"C\"><Programs><Program Name=\"P\"><Routines>\
        <Routine Name=\"A\" Type=\"ST\"><STContent><Line Number=\"0\"><![CDATA[x := 1;]]></Line></STContent></Routine>\
        <Routine Name=\"B\" Type=\"ST\"><STContent><Line Number=\"0\"><![CDATA[x := 1;]]></Line></STContent></Routine>\
        </Routines></Program></Programs></Controller></RSLogix5000Content>\n";
    let file = write_temp(src, "l5x");
    let parser = Parser::new().expect("init parser");
    let chunks = parser.parse_file(file.path()).expect("parse");

    assert!(
        chunks.len() >= 2,
        "expected at least two chunks (one per region), got {}",
        chunks.len()
    );
    assert_ids_injective(&chunks, "l5x_same_line_regions");
}

// ── Real-parser pins (falsifier reproductions) ──────────────────────────────
//
// These are the concrete inputs the property generalizes. They run the REAL
// tree-sitter parser end to end and assert the formerly-colliding pairs now get
// distinct ids and resolve unambiguously.

fn write_temp(content: &str, ext: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(&format!(".{ext}"))
        .tempfile()
        .expect("create temp file");
    f.write_all(content.as_bytes()).expect("write temp file");
    f.flush().expect("flush temp file");
    f
}

/// Assert no two chunks of a single parsed file share an id.
fn assert_ids_injective(chunks: &[cqs::Chunk], label: &str) {
    let mut seen = HashSet::new();
    for c in chunks {
        assert!(
            seen.insert(c.id.clone()),
            "{label}: duplicate chunk id {} — id is not injective within the file \
             (name={:?} line_start={} byte_start={})",
            c.id,
            c.name,
            c.line_start,
            c.byte_start,
        );
    }
}

/// The headline falsifier: two macro invocations on one line. Byte-identical
/// content ⇒ identical full `content_hash`, identical `line_start`. The old
/// id collapsed them; with `byte_start` they must be distinct.
#[test]
fn macro_twins_get_distinct_ids() {
    // Both invocations on ONE physical line so line_start collides; identical
    // token text so content_hash collides. Only byte_start differs.
    let src = "macro_rules! m { () => { fn unused() {} }; }\nm!{} m!{}\n";
    let file = write_temp(src, "rs");
    let parser = Parser::new().expect("init parser");
    let chunks = parser.parse_file(file.path()).expect("parse");

    // Find the two macro-invocation chunks (same line_start, same content_hash).
    let invos: Vec<&cqs::Chunk> = chunks
        .iter()
        .filter(|c| c.content.contains("m!{}") || c.content.trim() == "m!{}")
        .collect();

    // Whatever the grammar yields, the WHOLE file's ids must be injective.
    assert_ids_injective(&chunks, "macro_twins");

    // If the grammar surfaced two same-line same-hash chunks, prove the
    // disambiguator did its job on exactly that pair.
    if let Some((a, b)) = find_colliding_legacy_pair(&chunks) {
        assert_ne!(
            a.id, b.id,
            "macro twins still collide: same line_start={} and content_hash, \
             byte_start {} vs {} must yield distinct ids",
            a.line_start, a.byte_start, b.byte_start,
        );
        assert_ne!(
            a.byte_start, b.byte_start,
            "the two same-line same-hash chunks must have distinct byte_start"
        );
    }
    // Touch invos so the binding is meaningful even when the grammar collapses
    // the two invocations (older grammars may emit one chunk); the file-wide
    // injectivity check above is the load-bearing assertion.
    let _ = invos.len();
}

/// `struct A; impl A {}` — two elements that begin on line 1. line_start alone
/// never disambiguated them; byte_start does.
#[test]
fn same_line_struct_and_impl_get_distinct_ids() {
    let src = "struct A; impl A { fn m(&self) {} }\n";
    let file = write_temp(src, "rs");
    let parser = Parser::new().expect("init parser");
    let chunks = parser.parse_file(file.path()).expect("parse");

    assert_ids_injective(&chunks, "struct_impl_same_line");

    // The struct and the impl (or its method) start on line 1; confirm at least
    // two distinct chunks exist with line_start == 1 and that they differ.
    let line1: Vec<&cqs::Chunk> = chunks.iter().filter(|c| c.line_start == 1).collect();
    if line1.len() >= 2 {
        let mut ids = HashSet::new();
        for c in &line1 {
            assert!(
                ids.insert(c.id.clone()),
                "two line-1 chunks share an id: {}",
                c.id
            );
        }
    }
}

/// parent_id resolution must be unambiguous across a formerly-colliding pair:
/// every chunk's `parent_id`, if set, must point at exactly one chunk in the
/// same file. (A collapsed id would make a parent_id resolve to the wrong — or
/// an arbitrary — sibling.)
#[test]
fn parent_id_resolves_unambiguously_with_collisions_present() {
    // A long impl whose methods window, plus same-line siblings, so the file
    // mixes parent/child chunks AND same-line elements.
    let body = "    fn helper(&self) {}\n".repeat(2);
    let src = format!("struct A; impl A {{\n{body}}}\nstruct B; impl B {{ fn g(&self) {{}} }}\n");
    let file = write_temp(&src, "rs");
    let parser = Parser::new().expect("init parser");
    let chunks = parser.parse_file(file.path()).expect("parse");

    assert_ids_injective(&chunks, "parent_resolution");

    // Build the id → chunk index; injectivity (asserted above) guarantees it is
    // a function. Every parent_id must hit exactly one entry.
    let by_id: std::collections::HashMap<&str, &cqs::Chunk> =
        chunks.iter().map(|c| (c.id.as_str(), c)).collect();
    assert_eq!(
        by_id.len(),
        chunks.len(),
        "id index lost entries — ids are not injective"
    );
    for c in &chunks {
        if let Some(pid) = &c.parent_id {
            // A parent_id either points at a real chunk in this file or is a
            // windowing base (the parent chunk itself). Either way it must
            // resolve to AT MOST one chunk — never be ambiguous.
            let matches = chunks.iter().filter(|o| &o.id == pid).count();
            assert!(
                matches <= 1,
                "parent_id {pid} resolves to {matches} chunks — ambiguous \
                 containment from an id collision"
            );
        }
    }
}

/// Helper: find two chunks that the LEGACY id (`{line_start}:{hash8}` without
/// byte_start) would have collided — same line_start and same 8-char hash
/// prefix — to target the disambiguation assertion at exactly that pair.
fn find_colliding_legacy_pair(chunks: &[cqs::Chunk]) -> Option<(&cqs::Chunk, &cqs::Chunk)> {
    for (i, a) in chunks.iter().enumerate() {
        for b in &chunks[i + 1..] {
            if a.line_start == b.line_start
                && a.content_hash.get(..8) == b.content_hash.get(..8)
                && a.byte_start != b.byte_start
            {
                return Some((a, b));
            }
        }
    }
    None
}
