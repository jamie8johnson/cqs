//! Self-tests for the in-process integration fixture.
//!
//! These verify the harness itself before any of the five `cli_*_test.rs`
//! binaries are converted to use it (phase 1 of the slow-tests
//! elimination — see `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`).
//!
//! All tests use the `MockEmbedder` so they're hermetic and ~ms-fast.
//! The `with_real_embedder()` path is exercised by the eventual converted
//! tests; testing the cold-load here would re-introduce the cost we're
//! trying to delete.

mod common;

use common::{mock_embed_text, InProcessFixture, MockEmbedder, TestEmbedder};

#[test]
fn empty_fixture_starts_clean() {
    let f = InProcessFixture::new();
    let stats = f.store.stats().expect("stats");
    assert_eq!(stats.total_chunks, 0, "fresh fixture must have no chunks");
    assert_eq!(stats.total_files, 0, "fresh fixture must have no files");
}

#[test]
fn write_file_then_index_inserts_chunks() {
    let mut f = InProcessFixture::new();
    f.write_file(
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\
         pub fn sub(a: i32, b: i32) -> i32 { a - b }\n",
    )
    .expect("write");
    let inserted = f.index().expect("index");
    assert!(
        inserted >= 2,
        "expected ≥2 chunks from a 2-fn file, got {inserted}"
    );

    let stats = f.store.stats().expect("stats");
    assert!(
        stats.total_chunks >= 2,
        "store should report the inserted chunks, got {}",
        stats.total_chunks
    );
}

#[test]
fn with_corpus_helper_round_trips() {
    let f = InProcessFixture::with_corpus(&[
        (
            "src/math.rs",
            "pub fn multiply(a: i32, b: i32) -> i32 { a * b }\n",
        ),
        (
            "src/text.rs",
            "pub fn greet(name: &str) -> String { format!(\"hello {name}\") }\n",
        ),
    ]);
    let stats = f.store.stats().expect("stats");
    assert!(
        stats.total_chunks >= 2,
        "expected at least one chunk per fixture file, got {}",
        stats.total_chunks
    );
    assert!(
        stats.total_files >= 2,
        "expected the two fixture files to be tracked, got {}",
        stats.total_files
    );
}

#[test]
fn search_returns_indexed_chunk_via_mock_embedder() {
    // Mock embedder hashes content deterministically, so `search("foo")`
    // matches the chunk that contains literal "foo" in its content.
    // This is the property converted tests will rely on.
    let f = InProcessFixture::with_corpus(&[(
        "src/lib.rs",
        "pub fn unique_marker_token() -> i32 { 42 }\n",
    )]);

    let hits = f.search("unique_marker_token", 5).expect("search");
    assert!(
        !hits.is_empty(),
        "search by content token should return the indexed chunk"
    );
    let names: Vec<&str> = hits.iter().map(|r| r.chunk.name.as_str()).collect();
    assert!(
        names.contains(&"unique_marker_token"),
        "expected unique_marker_token in results, got {names:?}"
    );
}

#[test]
fn fixture_isolation_per_instance() {
    // Two fixtures must not share storage — each has its own tempdir.
    let a = InProcessFixture::with_corpus(&[("src/a.rs", "pub fn fn_a() {}\n")]);
    let b = InProcessFixture::with_corpus(&[("src/b.rs", "pub fn fn_b() {}\n")]);

    let a_hits = a.search("fn_b", 5).expect("search a for b");
    assert!(
        a_hits.iter().all(|r| r.chunk.name != "fn_b"),
        "fixture a must not see fixture b's chunks"
    );

    let b_hits = b.search("fn_a", 5).expect("search b for a");
    assert!(
        b_hits.iter().all(|r| r.chunk.name != "fn_a"),
        "fixture b must not see fixture a's chunks"
    );
}

#[test]
fn mock_embedder_is_deterministic() {
    let m = MockEmbedder;
    let v1 = m.embed_query("hello world");
    let v2 = m.embed_query("hello world");
    assert_eq!(v1.as_slice(), v2.as_slice(), "same text → same vector");

    let v3 = m.embed_query("totally different content");
    assert_ne!(
        v1.as_slice(),
        v3.as_slice(),
        "different text → different vector"
    );
}

#[test]
fn mock_embed_text_helper_matches_trait_impl() {
    // The free function and the trait impl must agree so tests can use
    // either form interchangeably.
    let direct = mock_embed_text("for-the-trait");
    let via_trait = MockEmbedder.embed_query("for-the-trait");
    assert_eq!(direct.as_slice(), via_trait.as_slice());
}
