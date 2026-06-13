//! Search path tests (P3 #36, #37)
//!
//! Tests for HNSW-guided search, brute-force search, glob filtering,
//! name_boost hybrid scoring, and unified code+notes search.

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::embedder::Embedding;
use cqs::index::{IndexResult, VectorIndex};
use cqs::note::Note;
use cqs::parser::{ChunkType, Language};
use cqs::store::{SearchFilter, UnifiedResult};
use std::path::PathBuf;

// ============ Mock VectorIndex ============

/// A mock vector index that returns pre-configured results
struct MockIndex {
    results: Vec<IndexResult>,
}

impl MockIndex {
    fn new(results: Vec<IndexResult>) -> Self {
        Self { results }
    }
}

impl VectorIndex for MockIndex {
    fn search(&self, _query: &Embedding, k: usize) -> Vec<IndexResult> {
        self.results.iter().take(k).cloned().collect()
    }

    fn len(&self) -> usize {
        self.results.len()
    }

    fn name(&self) -> &'static str {
        "Mock"
    }

    fn dim(&self) -> usize {
        cqs::EMBEDDING_DIM
    }
}

// ============ Helpers ============

/// Create a chunk with a specific file path and language
fn chunk_with_path(name: &str, file: &str, lang: Language) -> cqs::Chunk {
    let content = format!("fn {}() {{ /* body */ }}", name);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    cqs::Chunk {
        id: format!("{}:1:{}", file, &hash[..8]),
        file: PathBuf::from(file),
        language: lang,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content,
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash: hash,
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    }
}

/// Insert chunks with identical embeddings and return their IDs
fn insert_chunks(store: &TestStore, chunks: &[cqs::Chunk], seed: f32) -> Vec<String> {
    let emb = mock_embedding(seed);
    let pairs: Vec<_> = chunks.iter().map(|c| (c.clone(), emb.clone())).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();
    chunks.iter().map(|c| c.id.clone()).collect()
}

/// Insert a note
fn insert_note(store: &TestStore, id: &str, text: &str, sentiment: f32) {
    let note = Note {
        id: id.to_string(),
        text: text.to_string(),
        sentiment,
        mentions: vec![],
        kind: None,
    };
    store
        .upsert_notes_batch(&[note], &PathBuf::from("notes.toml"), 12345)
        .unwrap();
}

// ===== #36: search_by_candidate_ids =====

#[test]
fn test_search_by_candidate_ids_basic() {
    let store = TestStore::new();
    let c1 = test_chunk("foo", "fn foo() { 1 + 1 }");
    let c2 = test_chunk("bar", "fn bar() { 2 + 2 }");
    let c3 = test_chunk("baz", "fn baz() { 3 + 3 }");

    let ids = insert_chunks(&store, &[c1, c2, c3], 1.0);
    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Search only for c1 and c2
    let candidate_ids: Vec<&str> = ids[..2].iter().map(|s| s.as_str()).collect();
    let results = store
        .search_by_candidate_ids(&candidate_ids, &query, &filter, 10, 0.0)
        .unwrap();

    assert_eq!(results.len(), 2, "Should find exactly 2 candidates");
    let found_ids: Vec<&str> = results.iter().map(|r| r.chunk.id.as_str()).collect();
    assert!(!found_ids.contains(&ids[2].as_str()), "Should not find c3");
}

#[test]
fn test_search_by_candidate_ids_empty() {
    let store = TestStore::new();
    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    let results = store
        .search_by_candidate_ids(&[], &query, &filter, 10, 0.0)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_by_candidate_ids_respects_threshold() {
    let store = TestStore::new();
    let c1 = test_chunk("foo", "fn foo() { opposite }");
    let emb = mock_embedding(-1.0);
    store
        .upsert_chunks_batch(&[(c1.clone(), emb)], Some(12345))
        .unwrap();

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    let results = store
        .search_by_candidate_ids(&[c1.id.as_str()], &query, &filter, 10, 0.99)
        .unwrap();
    assert!(
        results.is_empty(),
        "Opposite embedding should not meet 0.99 threshold"
    );
}

#[test]
fn test_search_by_candidate_ids_with_glob_filter() {
    let store = TestStore::new();
    let c1 = chunk_with_path("foo", "src/main.rs", Language::Rust);
    let c2 = chunk_with_path("bar", "tests/test.rs", Language::Rust);

    let ids = insert_chunks(&store, &[c1, c2], 1.0);
    let query = mock_embedding(1.0);
    let filter = {
        let mut f = SearchFilter::default();
        f.path_pattern = Some("src/**".to_string());
        f
    };

    let candidate_ids: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    let results = store
        .search_by_candidate_ids(&candidate_ids, &query, &filter, 10, 0.0)
        .unwrap();

    assert_eq!(results.len(), 1, "Glob should filter to src/ only");
    assert!(results[0].chunk.file.to_string_lossy().contains("src/"));
}

// ===== #36: search_filtered_with_index =====

#[test]
fn test_search_filtered_with_index_uses_index() {
    let store = TestStore::new();
    let c1 = test_chunk("indexed_fn", "fn indexed_fn() { indexed }");
    let c2 = test_chunk("other_fn", "fn other_fn() { other }");

    let ids = insert_chunks(&store, &[c1, c2], 1.0);
    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Mock index returns only c1
    let mock = MockIndex::new(vec![IndexResult {
        id: ids[0].clone(),
        score: 0.9,
    }]);

    let results = store
        .search_filtered_with_index(&query, &filter, 10, 0.0, Some(&mock))
        .unwrap();

    // Should only return c1 (the one the index returned)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.id, ids[0]);
}

#[test]
fn test_search_filtered_with_index_falls_back_without_index() {
    let store = TestStore::new();
    let c1 = test_chunk("brute_fn", "fn brute_fn() { brute }");
    insert_chunks(&store, &[c1], 1.0);

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // No index provided — should fall back to brute-force
    let results = store
        .search_filtered_with_index(&query, &filter, 10, 0.0, None)
        .unwrap();

    assert_eq!(results.len(), 1);
}

// ===== #36: search_code_results (SQ-9: code-only; renamed CQ-V1.33.0-10) =====

#[test]
fn test_search_code_results_returns_code_only() {
    let store = TestStore::new();
    let c1 = test_chunk("unified_fn", "fn unified_fn() { code }");
    let ids = insert_chunks(&store, &[c1], 1.0);

    insert_note(&store, "note1", "Important pattern", 0.5);

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Mock index returns chunk and legacy note: prefixed entry
    let mock = MockIndex::new(vec![
        IndexResult {
            id: ids[0].clone(),
            score: 0.9,
        },
        IndexResult {
            id: "note:note1".to_string(),
            score: 0.85,
        },
    ]);

    let results = store
        .search_code_results(&query, &filter, 10, 0.0, Some(&mock))
        .unwrap();

    let has_code = results.iter().any(|r| matches!(r, UnifiedResult::Code(_)));
    assert!(has_code, "Should include code results");
    // Notes no longer appear in unified results (SQ-9)
    assert!(
        results.iter().all(|r| matches!(r, UnifiedResult::Code(_))),
        "All results should be code"
    );
}

#[test]
fn test_search_unified_without_index() {
    let store = TestStore::new();
    let c1 = test_chunk("no_idx_fn", "fn no_idx_fn() { stuff }");
    insert_chunks(&store, &[c1], 1.0);

    insert_note(&store, "note2", "Another note", 0.0);

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // No index -- brute-force
    let results = store
        .search_code_results(&query, &filter, 10, 0.0, None)
        .unwrap();

    let has_code = results.iter().any(|r| matches!(r, UnifiedResult::Code(_)));
    assert!(has_code, "Should include code from brute-force");
    // Notes no longer appear in unified results (SQ-9)
    assert!(
        results.iter().all(|r| matches!(r, UnifiedResult::Code(_))),
        "All results should be code"
    );
}

// ===== search_hybrid: the production search path =====
//
// `search_hybrid` is what the daemon/CLI search surface calls. With SPLADE
// disabled (the default `SearchFilter`), it delegates to the index-guided
// path when a vector index is supplied, or brute-force when it is not. These
// tests pin that delegation, result-name fidelity, limit truncation, and that
// the index-guided and brute-force legs agree on the same seeded corpus.

#[test]
fn test_search_hybrid_uses_index_and_returns_indexed_chunk() {
    let store = TestStore::new();
    let c1 = test_chunk("hybrid_target", "fn hybrid_target() { target }");
    let c2 = test_chunk("hybrid_other", "fn hybrid_other() { other }");

    let ids = insert_chunks(&store, &[c1, c2], 1.0);
    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Index returns only the first chunk. SPLADE disabled (default filter) +
    // splade=None → search_hybrid routes to search_filtered_with_index.
    let mock = MockIndex::new(vec![IndexResult {
        id: ids[0].clone(),
        score: 0.9,
    }]);

    let results = store
        .search_hybrid(&query, &filter, 10, 0.0, Some(&mock), None)
        .unwrap();

    assert_eq!(results.len(), 1, "only the indexed chunk should return");
    assert_eq!(results[0].chunk.name, "hybrid_target");
}

#[test]
fn test_search_hybrid_truncates_to_limit() {
    let store = TestStore::new();
    let chunks: Vec<cqs::Chunk> = (0..5)
        .map(|i| test_chunk(&format!("fn_{i}"), &format!("fn fn_{i}() {{ body{i} }}")))
        .collect();
    let ids = insert_chunks(&store, &chunks, 1.0);
    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Index offers all five; request only two.
    let index_results: Vec<IndexResult> = ids
        .iter()
        .map(|id| IndexResult {
            id: id.clone(),
            score: 0.9,
        })
        .collect();
    let mock = MockIndex::new(index_results);

    let results = store
        .search_hybrid(&query, &filter, 2, 0.0, Some(&mock), None)
        .unwrap();

    assert_eq!(results.len(), 2, "results must be truncated to limit=2");
}

#[test]
fn test_search_hybrid_index_guided_agrees_with_brute_force() {
    let store = TestStore::new();
    let c1 = test_chunk("agree_a", "fn agree_a() { a }");
    let c2 = test_chunk("agree_b", "fn agree_b() { b }");
    let c3 = test_chunk("agree_c", "fn agree_c() { c }");

    let ids = insert_chunks(&store, &[c1, c2, c3], 1.0);
    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Index exposes every chunk, so the index-guided candidate set equals the
    // full corpus that brute-force scans. Identical embeddings make the score
    // ties deterministic via the id tie-break, so the two paths must agree on
    // the returned set.
    let index_results: Vec<IndexResult> = ids
        .iter()
        .map(|id| IndexResult {
            id: id.clone(),
            score: 1.0,
        })
        .collect();
    let mock = MockIndex::new(index_results);

    let guided = store
        .search_hybrid(&query, &filter, 10, 0.0, Some(&mock), None)
        .unwrap();
    let brute = store
        .search_hybrid(&query, &filter, 10, 0.0, None, None)
        .unwrap();

    let mut guided_names: Vec<&str> = guided.iter().map(|r| r.chunk.name.as_str()).collect();
    let mut brute_names: Vec<&str> = brute.iter().map(|r| r.chunk.name.as_str()).collect();
    guided_names.sort_unstable();
    brute_names.sort_unstable();

    assert_eq!(
        guided_names, brute_names,
        "index-guided and brute-force should return the same chunk set"
    );
    assert!(
        brute_names.contains(&"agree_a")
            && brute_names.contains(&"agree_b")
            && brute_names.contains(&"agree_c"),
        "all three seeded chunks should be returned, got: {brute_names:?}"
    );
}

// ===== SPLADE sparse leg surfaces in rank_signals =====
//
// The sparse (SPLADE) leg is consumed inside `search_hybrid` before
// `finalize_results`, so its per-result rank is threaded out to the recording
// seam. These pin (a) bit-identical scores with recording on vs off on the
// SPLADE path, and (b) that a chunk the sparse leg contributed to records a
// `sparse` signal.

/// Build a SPLADE filter + index + sparse query over a seeded corpus, run
/// `search_hybrid` with recording off and on, and assert the (id, score)
/// sequence is byte-for-byte identical — the recorder is a pure side channel
/// even on the hybrid fusion path.
#[test]
fn search_hybrid_splade_rank_signals_bit_identical() {
    use cqs::splade::index::SpladeIndex;

    let store = TestStore::new();
    let c1 = test_chunk("spladeAlpha", "fn spladeAlpha() { a }");
    let c2 = test_chunk("spladeBeta", "fn spladeBeta() { b }");
    let c3 = test_chunk("spladeGamma", "fn spladeGamma() { c }");
    let ids = insert_chunks(&store, &[c1, c2, c3], 1.0);

    // SPLADE index keyed by the real chunk ids so the fused path resolves them.
    let splade_index = SpladeIndex::build(vec![
        (ids[0].clone(), vec![(1, 0.5), (2, 0.3)]),
        (ids[1].clone(), vec![(1, 0.9), (3, 0.4)]),
        (ids[2].clone(), vec![(2, 0.8)]),
    ]);
    let sparse_query: cqs::splade::SparseVector = vec![(1, 1.0), (2, 1.0)];

    let query = mock_embedding(1.0);
    let mock = MockIndex::new(
        ids.iter()
            .map(|id| IndexResult {
                id: id.clone(),
                score: 0.9,
            })
            .collect(),
    );

    let mk = |record: bool| {
        let mut f = SearchFilter::default();
        f.enable_splade = true;
        f.splade_alpha = 0.5;
        f.record_rank_signals = record;
        f
    };

    let off = store
        .search_hybrid(
            &query,
            &mk(false),
            10,
            0.0,
            Some(&mock),
            Some((&splade_index, &sparse_query)),
        )
        .unwrap();
    let on = store
        .search_hybrid(
            &query,
            &mk(true),
            10,
            0.0,
            Some(&mock),
            Some((&splade_index, &sparse_query)),
        )
        .unwrap();

    assert_eq!(off.len(), on.len(), "SPLADE result count changed");
    for (a, b) in off.iter().zip(on.iter()) {
        assert_eq!(a.chunk.id, b.chunk.id, "SPLADE result order changed");
        assert_eq!(
            a.score.to_bits(),
            b.score.to_bits(),
            "SPLADE score bits changed for {} (off={}, on={})",
            a.chunk.id,
            a.score,
            b.score
        );
    }
    assert!(
        off.iter().all(|r| r.rank_signals.is_empty()),
        "recording-off SPLADE run must carry no rank_signals"
    );
}

/// The `sparse` signal is recorded for a result the sparse leg ranked.
#[test]
fn search_hybrid_records_sparse_signal() {
    use cqs::splade::index::SpladeIndex;

    let store = TestStore::new();
    let c1 = test_chunk("sparseHit", "fn sparseHit() { a }");
    let c2 = test_chunk("sparseOther", "fn sparseOther() { b }");
    let ids = insert_chunks(&store, &[c1, c2], 1.0);

    let splade_index = SpladeIndex::build(vec![
        (ids[0].clone(), vec![(7, 0.9)]),
        (ids[1].clone(), vec![(7, 0.2)]),
    ]);
    let sparse_query: cqs::splade::SparseVector = vec![(7, 1.0)];

    let query = mock_embedding(1.0);
    let mock = MockIndex::new(
        ids.iter()
            .map(|id| IndexResult {
                id: id.clone(),
                score: 0.9,
            })
            .collect(),
    );
    let mut filter = SearchFilter::default();
    filter.enable_splade = true;
    filter.splade_alpha = 0.5;
    filter.record_rank_signals = true;

    let results = store
        .search_hybrid(
            &query,
            &filter,
            10,
            0.0,
            Some(&mock),
            Some((&splade_index, &sparse_query)),
        )
        .unwrap();

    // Every result here came through the sparse leg (both chunks carry token 7),
    // so the top sparse hit must record a `sparse` signal.
    let any_sparse = results
        .iter()
        .any(|r| r.rank_signals.iter().any(|s| s.signal == "sparse"));
    assert!(
        any_sparse,
        "a SPLADE query must record the sparse leg; signals: {:?}",
        results
            .iter()
            .map(|r| (&r.chunk.name, &r.rank_signals))
            .collect::<Vec<_>>()
    );
}

// ===== #37: search_filtered with glob =====

#[test]
fn test_search_filtered_glob_pattern() {
    let store = TestStore::new();
    let c1 = chunk_with_path("src_fn", "src/lib.rs", Language::Rust);
    let c2 = chunk_with_path("test_fn", "tests/test.rs", Language::Rust);
    let c3 = chunk_with_path("bench_fn", "benches/bench.rs", Language::Rust);

    insert_chunks(&store, &[c1, c2, c3], 1.0);

    let query = mock_embedding(1.0);
    let filter = {
        let mut f = SearchFilter::default();
        f.path_pattern = Some("src/**".to_string());
        f
    };

    let results = store.search_filtered(&query, &filter, 10, 0.0).unwrap();

    assert_eq!(results.len(), 1, "Glob should filter to src/ only");
    assert_eq!(results[0].chunk.name, "src_fn");
}

// ===== #37: search_filtered with language filter =====

#[test]
fn test_search_filtered_language() {
    let store = TestStore::new();
    let c1 = chunk_with_path("rust_fn", "src/main.rs", Language::Rust);
    let c2 = chunk_with_path("py_fn", "src/main.py", Language::Python);

    insert_chunks(&store, &[c1, c2], 1.0);

    let query = mock_embedding(1.0);
    let filter = {
        let mut f = SearchFilter::default();
        f.languages = Some(vec![Language::Rust]);
        f
    };

    let results = store.search_filtered(&query, &filter, 10, 0.0).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "rust_fn");
}

// ===== #37: search_by_name FTS =====

#[test]
fn test_search_by_name() {
    let store = TestStore::new();
    let c1 = test_chunk("parse_config", "fn parse_config() { parse }");
    let c2 = test_chunk("render_ui", "fn render_ui() { render }");
    let c3 = test_chunk("parse_args", "fn parse_args() { args }");

    insert_chunks(&store, &[c1, c2, c3], 1.0);

    let results = store.search_by_name("parse", 10).unwrap();
    assert!(results.len() >= 2, "Should find at least 2 'parse' chunks");

    for r in &results {
        assert!(
            r.chunk.name.contains("parse"),
            "FTS results should match 'parse', got: {}",
            r.chunk.name
        );
    }
}

// ===== #5: search_reference_by_name =====

#[test]
fn test_search_reference_by_name() {
    use cqs::reference::ReferenceIndex;

    let store = TestStore::new();
    let c1 = test_chunk("search_fn", "fn search_fn() { search }");
    let c2 = test_chunk("find_fn", "fn find_fn() { find }");

    insert_chunks(&store, &[c1, c2], 1.0);

    // Create a reference index (open separate Store to same DB)
    let ref_store = cqs::Store::open_readonly(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.8,
        db_path: std::path::PathBuf::new(),
        loaded_identity: None,
    };

    // Search by name
    let results =
        cqs::reference::search_reference_by_name(&ref_idx, "search_fn", 10, 0.0, true).unwrap();

    assert!(!results.is_empty(), "Should find search_fn");
    assert_eq!(results[0].chunk.name, "search_fn");

    // Score should be scaled by weight (0.8)
    assert!(
        results[0].score <= 0.8,
        "Score should be scaled by weight 0.8, got {}",
        results[0].score
    );
}

#[test]
fn test_search_reference_by_name_threshold() {
    use cqs::reference::ReferenceIndex;

    let store = TestStore::new();
    let c1 = test_chunk("test_fn", "fn test_fn() {}");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open_readonly(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.5, // Low weight
        db_path: std::path::PathBuf::new(),
        loaded_identity: None,
    };

    // High threshold should filter out results (score * weight < threshold)
    let results =
        cqs::reference::search_reference_by_name(&ref_idx, "test_fn", 10, 0.9, true).unwrap();

    assert!(
        results.is_empty(),
        "High threshold should filter out results with low weight"
    );
}
