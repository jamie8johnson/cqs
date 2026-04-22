//! In-process integration tests for the init / index / search / stats /
//! callers / callees / gc / dead-code core. Replaces the
//! `slow-tests`-gated `tests/cli_test.rs`, which subprocess-spawned the
//! `cqs` binary and cold-loaded the embedder per invocation.
//!
//! Phase 2 PR-2 of `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`.
//!
//! Pure CLI-surface concerns (`--help`, `--version`, `cqs completions`,
//! `cqs doctor`, exit codes from clap parse failures) live in
//! `tests/cli_surface_test.rs`. Those still spawn the binary, but they
//! don't load any ML model so the cost is bounded at ~50-200 ms per
//! invocation.

mod common;

use common::InProcessFixture;

fn sample_corpus() -> InProcessFixture {
    // Same shape as the old `setup_project()` helper from cli_test.rs:
    // two simple top-level fns with no internal call edges. The chunks
    // are findable, callers/callees come back empty (which is itself the
    // assertion for the dead-code tests below).
    InProcessFixture::with_corpus(&[(
        "src/lib.rs",
        r#"
/// Adds two numbers
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Subtracts b from a
pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}
"#,
    )])
}

// ---------------------------------------------------------------------
// init
// ---------------------------------------------------------------------

#[test]
fn init_creates_database_file() {
    let f = InProcessFixture::new();
    // TestStore::new already calls `Store::open + init`; the database
    // file should exist on disk.
    let db = f.store.db_path();
    assert!(db.exists(), "expected index.db at {}", db.display());
}

#[test]
fn init_is_idempotent() {
    use cqs::store::{ModelInfo, Store};
    let f = InProcessFixture::new();
    let db = f.store.db_path();
    // Re-open + init the same DB; the second init must not fail (matches
    // the old `cqs init && cqs init` subprocess test).
    let store2 = Store::open(&db).expect("re-open");
    store2.init(&ModelInfo::default()).expect("re-init");
}

// ---------------------------------------------------------------------
// index
// ---------------------------------------------------------------------

#[test]
fn index_inserts_chunks_for_indexed_files() {
    let mut f = InProcessFixture::new();
    f.write_file(
        "src/lib.rs",
        "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\n",
    )
    .unwrap();
    let inserted = f.index().expect("index");
    assert!(
        inserted >= 3,
        "expected ≥3 chunks from a 3-fn file, got {inserted}"
    );
    let stats = f.store.stats().expect("stats");
    assert!(stats.total_chunks >= 3);
    assert!(stats.total_files >= 1);
}

#[test]
fn index_then_stats_reports_counts() {
    let f = sample_corpus();
    let stats = f.store.stats().expect("stats");
    assert!(
        stats.total_chunks > 0,
        "stats.total_chunks > 0 after indexing"
    );
    assert!(stats.total_files > 0);
    assert!(!stats.model_name.is_empty());
}

// ---------------------------------------------------------------------
// search
// ---------------------------------------------------------------------

#[test]
fn search_returns_indexed_chunks() {
    let f = sample_corpus();
    // MockEmbedder: `search("add")` matches the chunk whose content
    // contains "add" — same property the harness self-tests rely on.
    let hits = f.search("add", 5).expect("search");
    let names: Vec<&str> = hits.iter().map(|h| h.chunk.name.as_str()).collect();
    assert!(
        names.contains(&"add"),
        "expected `add` in search results (got {names:?})"
    );
}

// ---------------------------------------------------------------------
// callers / callees
// ---------------------------------------------------------------------

#[test]
fn callers_returns_empty_for_uncalled_function() {
    let f = sample_corpus();
    // `add` has no callers in the fixture corpus — the test isn't
    // claiming "callers always finds something", it's claiming the
    // store API returns an empty list rather than erroring.
    let callers = f.store.get_callers_full("add").expect("get_callers_full");
    assert!(
        callers.is_empty(),
        "fixture has no callers of `add`; got {callers:?}"
    );
}

#[test]
fn callees_returns_data_for_caller() {
    // Build a tiny corpus where `outer` calls `inner`, then check
    // get_callees_full reports the call. Returns Vec<(callee_name, line)>.
    let f = InProcessFixture::with_corpus(&[(
        "src/lib.rs",
        "pub fn inner() -> i32 { 42 }\npub fn outer() -> i32 { inner() }\n",
    )]);
    let callees = f
        .store
        .get_callees_full("outer", None)
        .expect("get_callees_full");
    let names: Vec<&str> = callees.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"inner"),
        "expected `inner` in callees of `outer` (got {names:?})"
    );
}

// ---------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------

#[test]
fn gc_on_clean_index_prunes_nothing() {
    use std::collections::HashSet;
    let f = sample_corpus();
    // Build the file set the same way `cmd_gc` does: enumerate from the
    // root, then prune anything not in the set. On a fresh-indexed
    // corpus, every chunk's source file is present.
    let files: HashSet<_> = cqs::enumerate_files(&f.root, &f.parser.supported_extensions(), false)
        .expect("enumerate_files")
        .into_iter()
        .map(|p| f.root.join(p))
        .collect();
    let prune = f.store.prune_all(&files, &f.root).expect("prune_all");
    assert_eq!(
        prune.pruned_chunks, 0,
        "fresh index should prune 0 chunks (got {})",
        prune.pruned_chunks
    );
    assert_eq!(prune.pruned_calls, 0);
    assert_eq!(prune.pruned_type_edges, 0);
}

#[test]
fn gc_prunes_chunks_for_removed_files() {
    use std::collections::HashSet;
    let f = sample_corpus();
    // Remove src/lib.rs to make its chunks orphans, then re-enumerate
    // (which will return an empty file set) and prune.
    std::fs::remove_file(f.root.join("src/lib.rs")).expect("rm");
    let files: HashSet<_> = cqs::enumerate_files(&f.root, &f.parser.supported_extensions(), false)
        .expect("enumerate_files")
        .into_iter()
        .map(|p| f.root.join(p))
        .collect();
    let prune = f.store.prune_all(&files, &f.root).expect("prune_all");
    assert!(
        prune.pruned_chunks > 0,
        "expected pruned chunks after src/lib.rs removed, got {}",
        prune.pruned_chunks
    );
}

// ---------------------------------------------------------------------
// dead
// ---------------------------------------------------------------------

#[test]
fn dead_lists_pub_functions_with_no_callers() {
    let f = sample_corpus();
    // include_pub = false → public fns with no callers go to the
    // "possibly_dead_pub" bucket (the second tuple element).
    let (confident, possibly_dead_pub) = f.store.find_dead_code(false).expect("find_dead_code");

    let possibly_names: Vec<&str> = possibly_dead_pub
        .iter()
        .map(|c| c.chunk.name.as_str())
        .collect();
    assert!(
        possibly_names.contains(&"add") || possibly_names.contains(&"subtract"),
        "expected `add` or `subtract` in possibly_dead_pub (got {possibly_names:?})"
    );
    // confident-dead bucket is for non-pub fns; this fixture has none.
    assert!(
        confident.is_empty(),
        "no non-pub fns in fixture; confident-dead should be empty (got {confident:?})"
    );
}

#[test]
fn dead_with_include_pub_promotes_pub_to_confident() {
    let f = sample_corpus();
    // include_pub = true → public fns with no callers move into the
    // first bucket (confident-dead). Mirrors `cqs dead --include-pub`.
    let (confident, _possibly) = f.store.find_dead_code(true).expect("find_dead_code");
    let names: Vec<&str> = confident.iter().map(|c| c.chunk.name.as_str()).collect();
    assert!(
        names.contains(&"add") || names.contains(&"subtract"),
        "with include_pub, expected `add`/`subtract` in confident-dead (got {names:?})"
    );
}
