//! Frozen-artifact guard: PARSER_VERSION 8 -> 9 markdown chunk-id migration.
//!
//! # The version boundary
//!
//! Before the table-chunk id fix, `chunk_id()` built `{path}:{line}:{byte}:{hash8}`
//! and markdown table chunks used that bare base with NO structural suffix. The
//! whole-file (no-heading) chunk and a top-of-file table chunk both legitimately
//! sit at `(line 1, byte 0)`. When a file IS exactly one table with no heading and
//! no trailing newline, `blake3(file) == blake3(table)` — so all four base fields
//! collided and the two chunks shared ONE id. The store's PRIMARY KEY on
//! `chunks.id` then UPSERTed the second write onto the first: silent data loss, a
//! v8 index holding ONE row where there should be two.
//!
//! The fix added `chunk_id_suffixed()`; table chunks now carry a `:t{idx}`
//! suffix, so the top-of-file table is `{base}:t0`, distinct from the whole-file
//! `{base}`. PARSER_VERSION bumped 8 -> 9, and a parser-version-drift reindex
//! re-parses already-indexed files to migrate them (and heal the live collision).
//!
//! # Why this guard is needed (the blind spot)
//!
//! Every fresh-state fixture is born at v9. The current parser CANNOT emit the
//! colliding suffix-less table id — it always routes through `chunk_id_suffixed`.
//! So no test built from current code can construct a v8 index at rest: a
//! pre-migration row whose id only a v8 binary could have written. The landed
//! injectivity guards prove v9 emits distinct ids and that re-id preserves the
//! suffix, but NONE exercises the old-bytes -> new-code migration: a v8 index
//! being re-indexed by v9.
//!
//! # What this guard constructs and asserts
//!
//! It synthesizes the v8 index AT REST by hand (the only way to reach the null —
//! current code won't emit it): the suffix-less colliding row(s) the v8 binary
//! would have left behind, written directly into a store. It then runs the v9
//! re-index path (current parser -> `upsert_chunks_calls_and_prune` with the
//! fresh live-id set + per-file phantom prune) and asserts:
//!
//!   (a) the P1 collision is HEALED — the whole-file chunk and the table chunk
//!       end up at two DISTINCT ids; and
//!   (b) the orphan old-format (suffix-less) id is SWEPT by the phantom-chunk
//!       prune — it is gone from the store, not left dangling.
//!
//! Calibration (proven, see the module-level note on each test): reverting the
//! table chunk to a bare suffix-less `chunk_id` (the v8 logic) makes (a) fail —
//! the freshly parsed table re-collides with the whole-file chunk; passing an
//! empty `live_ids` set or skipping `prune_file` makes (b) fail — the v8 orphan
//! survives the reindex.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cqs::store::{ModelInfo, Store};
use cqs::{Chunk, Embedding, Parser};

/// A neutral, dim-correct embedding for a synthesized row.
fn dummy_embedding() -> Embedding {
    Embedding::new(vec![0.1_f32; cqs::EMBEDDING_DIM])
}

/// Build the v8 chunk id the OLD parser would have written for a chunk at the
/// given coordinates: the bare four-field base with NO structural suffix. This
/// is the exact format a v8 binary emitted for markdown table chunks — the form
/// the current parser can no longer produce.
fn v8_suffixless_id(
    path_display: &str,
    line_start: u32,
    byte_start: u32,
    content_hash: &str,
) -> String {
    // Deliberately NOT `chunk_id_suffixed`: the whole point is the suffix-less
    // shape only v8 could write. `cqs::parser::chunk_id` is the v8 base format,
    // unchanged across the bump (only the table chunk's *use* of it changed).
    cqs::parser::chunk_id(path_display, line_start, byte_start, content_hash)
}

/// Synthesize a single persisted chunk row at an arbitrary (possibly legacy) id.
/// `byte_start` / `line_start` mirror a markdown chunk; the body is a placeholder
/// — the id is the only load-bearing field for the migration relation.
fn legacy_md_row(origin: &str, id: &str, name: &str, line_start: u32, byte_start: u32) -> Chunk {
    Chunk {
        id: id.to_string(),
        file: PathBuf::from(origin),
        language: cqs::parser::Language::Markdown,
        chunk_type: cqs::parser::ChunkType::Section,
        name: name.to_string(),
        signature: name.to_string(),
        content: format!("legacy v8 content for {name}"),
        doc: None,
        line_start,
        line_end: line_start,
        byte_start,
        content_hash: "legacyhash".to_string(),
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        // The defining fact: this row was written by parser version 8.
        parser_version: 8,
    }
}

/// Open a fresh on-disk store under `dir`.
fn open_store(dir: &Path) -> Store {
    let store = Store::open(&dir.join("index.db")).unwrap();
    store.init(&ModelInfo::default()).unwrap();
    store
}

/// Run the v9 re-index path for `origin`: parse `source` with the CURRENT parser
/// (path rewritten to the relative `origin` so stored ids match the parser-native
/// relative form), then atomically upsert + phantom-prune with the fresh live-id
/// set. This is the same primitive the watch reindex path uses
/// (`upsert_chunks_calls_and_prune_with_file_calls` -> `..._inner`), exercised
/// directly so the test owns the live_ids / prune_file contract.
fn reindex_v9(store: &Store, origin: &str, source: &str) -> Vec<String> {
    let rel = PathBuf::from(origin);
    let parser = Parser::new().unwrap();
    // Parse against the relative path so the parser emits relative ids directly
    // (no abs->rel re-id needed for this single-file in-test reindex; the re-id
    // remap is covered by parser_stage tests). This keeps the guard focused on
    // the store-side heal + sweep across the version boundary.
    let chunks = parser
        .parse_source(source, cqs::parser::Language::Markdown, &rel)
        .unwrap();
    let emb = dummy_embedding();
    let pairs: Vec<(Chunk, Embedding)> = chunks.iter().cloned().map(|c| (c, emb.clone())).collect();
    let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
    store
        .upsert_chunks_calls_and_prune(&pairs, None, &[], Some(&rel), &live_ids)
        .unwrap();
    pairs.iter().map(|(c, _)| c.id.clone()).collect()
}

/// Return the set of stored chunk ids for `origin`.
fn stored_ids(store: &Store, origin: &str) -> Vec<String> {
    store
        .get_chunks_by_origin(origin)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect()
}

/// GUARD (a): the P1 whole-file/table collision is HEALED across the v8 -> v9
/// migration.
///
/// A v8 index of a no-heading single-table no-trailing-newline file holds ONE
/// row at `{path}:1:0:{hash8}` — the whole-file chunk and the table chunk
/// collapsed onto one id under v8 (silent UPSERT loss). After the v9 reindex the
/// store must hold TWO distinct rows: the whole-file `{base}` and the table
/// `{base}:t0`.
///
/// Calibration (RED proof): revert the markdown table chunk in
/// `src/parser/markdown/tables.rs` to a bare suffix-less `chunk_id(...)` (the v8
/// logic). The freshly parsed table then re-collides with the whole-file chunk —
/// the store holds ONE row again and the `assert_eq!(healed.len(), 2)` below
/// fails. GREEN with the shipped `chunk_id_suffixed` table id.
#[test]
fn v8_whole_file_table_collision_is_healed_by_v9_reindex() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = open_store(tmp.path());

    // The v8-colliding shape: no heading, exactly one table, NO trailing newline.
    // blake3(file) == blake3(table) -> v8 wrote ONE row at the shared base id.
    let origin = "only_table.md";
    let source = "| A | B |\n|---|---|\n| 1 | 2 |";

    // Reconstruct the v8 colliding id from the file bytes (this is the id BOTH
    // the whole-file and the table chunk shared under v8 — only one survived).
    let file_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let collided_id = v8_suffixless_id(origin, 1, 0, &file_hash);

    // FROZEN ARTIFACT: the v8 index at rest — a single row at the colliding id.
    // No fresh-state fixture can construct this: the current parser always emits
    // the `:t0`-suffixed table, so it can never leave one row at the bare base.
    let v8_row = legacy_md_row(origin, &collided_id, "only_table", 1, 0);
    store
        .upsert_embedded_batch(&[(v8_row, dummy_embedding())], &[], &HashMap::new())
        .unwrap();
    assert_eq!(
        stored_ids(&store, origin).len(),
        1,
        "v8 artifact precondition: exactly one colliding row at rest"
    );

    // v9 re-index: current parser -> two distinct ids -> fused upsert + prune.
    let new_ids = reindex_v9(&store, origin, source);
    assert_eq!(
        new_ids.len(),
        2,
        "v9 parser must emit two chunks (whole-file + :t0 table) for the colliding shape; got {new_ids:?}"
    );

    // (a) HEAL: the store now holds two DISTINCT rows.
    let healed = stored_ids(&store, origin);
    let distinct: std::collections::HashSet<&str> = healed.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        healed.len(),
        2,
        "collision not healed: expected 2 stored rows after v9 reindex, got {healed:?}"
    );
    assert_eq!(
        distinct.len(),
        2,
        "stored ids are not injective after v9 reindex: {healed:?}"
    );

    // The two healed ids are exactly the whole-file base and the suffixed table.
    let whole_file = cqs::parser::chunk_id(origin, 1, 0, &file_hash);
    let table = cqs::parser::chunk_id_suffixed(origin, 1, 0, &file_hash, "t0");
    assert!(
        distinct.contains(whole_file.as_str()),
        "healed set missing whole-file id {whole_file}: {healed:?}"
    );
    assert!(
        distinct.contains(table.as_str()),
        "healed set missing suffixed table id {table}: {healed:?}"
    );
    // The whole-file id equals the old v8 colliding base, so the v8 row was
    // refreshed in place; the table id is the NEW row that v8 lost.
    assert_eq!(
        whole_file, collided_id,
        "whole-file id must equal the v8 base"
    );
}

/// GUARD (b): a v8 suffix-less table-chunk row whose id is NOT in the v9 live set
/// is SWEPT by the phantom-chunk prune.
///
/// A no-heading file with TWO tables. Under v8 the SECOND table chunk was written
/// at a bare suffix-less id `{path}:{lineN}:{byteN}:{hash8}` (it begins mid-file,
/// so its base differs from the whole-file base — a clean, non-colliding orphan).
/// Under v9 that same table is `{...}:t1` — a DIFFERENT string. So the v8 row is
/// a phantom: present at rest, absent from the v9 parse output. The per-file
/// phantom prune (`DELETE ... WHERE origin = ? AND id NOT IN live_ids`) must
/// remove it; if it survived, an incremental re-index would leave a stale,
/// unreachable duplicate of the table forever.
///
/// Calibration (RED proof): pass `Some(&rel)` with an EMPTY `live_ids` -> the
/// prune degrades to a full DELETE then the new rows are absent (different
/// failure); the faithful RED is to skip the prune by passing `prune_file=None`
/// in `reindex_v9` — then the orphan survives and the suffix-less id assertion
/// below fails. GREEN with the shipped per-file prune.
#[test]
fn v8_suffixless_table_orphan_is_swept_by_phantom_prune() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = open_store(tmp.path());

    let origin = "multi.md";
    let source = "| A | B |\n|---|---|\n| 1 | 2 |\n\ntext\n\n| C | D |\n|---|---|\n| 3 | 4 |";

    // Parse with the current parser to discover the v9 ids and the second
    // table's coordinates (line_start / byte_start / content_hash). The v8 row
    // for that table is the SAME coordinates with a suffix-LESS id — the orphan.
    let parser = Parser::new().unwrap();
    let v9_chunks = parser
        .parse_source(
            source,
            cqs::parser::Language::Markdown,
            &PathBuf::from(origin),
        )
        .unwrap();
    let table2 = v9_chunks
        .iter()
        .find(|c| c.id.contains(":t1"))
        .expect("v9 parse must yield a second table chunk with a :t1 suffix");

    // The v8 orphan id: the second table's coordinates with NO suffix. This is
    // exactly what a v8 binary stored for that table; the v9 parser can never
    // emit it (it always appends :t1).
    let orphan_id = v8_suffixless_id(
        origin,
        table2.line_start,
        table2.byte_start,
        &table2.content_hash,
    );
    assert!(
        !orphan_id.contains(":t"),
        "orphan must be the suffix-less v8 form: {orphan_id}"
    );
    assert_ne!(
        orphan_id, table2.id,
        "v8 orphan id must DIFFER from the v9 table id (otherwise it's not an orphan)"
    );

    // FROZEN ARTIFACT: a v8 index holding the suffix-less table-2 orphan row
    // (plus an unrelated whole-file row so the origin isn't otherwise empty).
    let v8_whole = legacy_md_row(origin, &format!("{origin}:1:0:legacyfile"), "multi", 1, 0);
    let v8_orphan = legacy_md_row(
        origin,
        &orphan_id,
        "multi (table L7)",
        table2.line_start,
        table2.byte_start,
    );
    store
        .upsert_embedded_batch(
            &[
                (v8_whole, dummy_embedding()),
                (v8_orphan, dummy_embedding()),
            ],
            &[],
            &HashMap::new(),
        )
        .unwrap();
    assert!(
        stored_ids(&store, origin).contains(&orphan_id),
        "precondition: v8 orphan row is present at rest before the v9 reindex"
    );

    // v9 re-index with the fresh live-id set + per-file prune.
    let new_ids = reindex_v9(&store, origin, source);
    let new_set: std::collections::HashSet<&str> = new_ids.iter().map(|s| s.as_str()).collect();
    assert!(
        !new_set.contains(orphan_id.as_str()),
        "sanity: the v9 live set must NOT contain the suffix-less orphan"
    );

    // (b) SWEEP: the suffix-less v8 orphan is gone; every stored id is a v9 id.
    let after = stored_ids(&store, origin);
    assert!(
        !after.contains(&orphan_id),
        "phantom-chunk prune left the v8 suffix-less orphan {orphan_id} dangling: {after:?}"
    );
    for id in &after {
        assert!(
            new_set.contains(id.as_str()),
            "stored id {id} is not in the v9 live set after reindex — stale row survived: {after:?}"
        );
    }
    // And the migration is complete: every v9 id is present.
    let after_set: std::collections::HashSet<&str> = after.iter().map(|s| s.as_str()).collect();
    for id in &new_ids {
        assert!(
            after_set.contains(id.as_str()),
            "v9 id {id} missing after reindex: {after:?}"
        );
    }
}
