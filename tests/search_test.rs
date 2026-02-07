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
        parent_id: None,
        window_idx: None,
    }
}

/// Insert chunks with identical embeddings and return their IDs
fn insert_chunks(store: &TestStore, chunks: &[cqs::Chunk], seed: f32) -> Vec<String> {
    let emb = mock_embedding(seed);
    let pairs: Vec<_> = chunks.iter().map(|c| (c.clone(), emb.clone())).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();
    chunks.iter().map(|c| c.id.clone()).collect()
}

/// Insert a note and return its ID
fn insert_note(store: &TestStore, id: &str, text: &str, sentiment: f32, seed: f32) {
    let note = Note {
        id: id.to_string(),
        text: text.to_string(),
        sentiment,
        mentions: vec![],
    };
    let emb = mock_embedding(seed);
    store
        .upsert_notes_batch(&[(note, emb)], &PathBuf::from("notes.toml"), 12345)
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
    let filter = SearchFilter {
        path_pattern: Some("src/**".to_string()),
        ..Default::default()
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

// ===== #36: search_unified_with_index =====

#[test]
fn test_search_unified_with_index_returns_both() {
    let store = TestStore::new();
    let c1 = test_chunk("unified_fn", "fn unified_fn() { code }");
    let ids = insert_chunks(&store, &[c1], 1.0);

    insert_note(&store, "note1", "Important pattern", 0.5, 1.0);

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // Mock index returns both chunk and note
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
        .search_unified_with_index(&query, &filter, 10, 0.0, Some(&mock))
        .unwrap();

    let has_code = results.iter().any(|r| matches!(r, UnifiedResult::Code(_)));
    let has_note = results.iter().any(|r| matches!(r, UnifiedResult::Note(_)));

    assert!(has_code, "Should include code results");
    assert!(has_note, "Should include note results");
}

#[test]
fn test_search_unified_without_index() {
    let store = TestStore::new();
    let c1 = test_chunk("no_idx_fn", "fn no_idx_fn() { stuff }");
    insert_chunks(&store, &[c1], 1.0);

    insert_note(&store, "note2", "Another note", 0.0, 1.0);

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // No index — brute-force for both
    let results = store
        .search_unified_with_index(&query, &filter, 10, 0.0, None)
        .unwrap();

    let has_code = results.iter().any(|r| matches!(r, UnifiedResult::Code(_)));
    let has_note = results.iter().any(|r| matches!(r, UnifiedResult::Note(_)));

    assert!(has_code, "Should include code from brute-force");
    assert!(has_note, "Should include notes from brute-force");
}

#[test]
fn test_search_unified_note_weight_zero_excludes_notes() {
    let store = TestStore::new();
    let c1 = test_chunk("weighted_fn", "fn weighted_fn() { w }");
    insert_chunks(&store, &[c1], 1.0);
    insert_note(&store, "note3", "Excluded note", 0.0, 1.0);

    let query = mock_embedding(1.0);
    let filter = SearchFilter {
        note_weight: 0.0,
        ..Default::default()
    };

    let results = store
        .search_unified_with_index(&query, &filter, 10, 0.0, None)
        .unwrap();

    let has_note = results.iter().any(|r| matches!(r, UnifiedResult::Note(_)));
    assert!(!has_note, "note_weight=0 should exclude notes");
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
    let filter = SearchFilter {
        path_pattern: Some("src/**".to_string()),
        ..Default::default()
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
    let filter = SearchFilter {
        languages: Some(vec![Language::Rust]),
        ..SearchFilter::new()
    };

    let results = store.search_filtered(&query, &filter, 10, 0.0).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "rust_fn");
}

// ===== #37: brute-force note search =====

#[test]
fn test_search_notes_brute_force() {
    let store = TestStore::new();
    insert_note(&store, "n1", "First note about errors", -0.5, 1.0);
    insert_note(&store, "n2", "Second note about patterns", 0.5, 1.0);
    insert_note(&store, "n3", "Unrelated note", 0.0, -1.0);

    let query = mock_embedding(1.0);
    let results = store.search_notes(&query, 10, 0.0).unwrap();

    // n1 and n2 have same direction as query, n3 is opposite
    assert!(results.len() >= 2, "Should find at least 2 matching notes");

    // Check ordering (highest score first)
    for window in results.windows(2) {
        assert!(
            window[0].score >= window[1].score,
            "Results should be sorted by score"
        );
    }
}

#[test]
fn test_search_notes_brute_force_threshold() {
    let store = TestStore::new();
    insert_note(&store, "n1", "Matching note", 0.0, 1.0);
    insert_note(&store, "n2", "Opposite note", 0.0, -1.0);

    let query = mock_embedding(1.0);
    let results = store.search_notes(&query, 10, 0.9).unwrap();

    // Only n1 should match (same direction), n2 is opposite
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].note.id, "n1");
}

#[test]
fn test_search_notes_brute_force_limit() {
    let store = TestStore::new();
    for i in 0..10 {
        insert_note(
            &store,
            &format!("n{}", i),
            &format!("Note number {}", i),
            0.0,
            1.0,
        );
    }

    let query = mock_embedding(1.0);
    let results = store.search_notes(&query, 3, 0.0).unwrap();

    assert_eq!(results.len(), 3, "Should respect limit");
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
    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.8,
    };

    // Search by name
    let results = cqs::reference::search_reference_by_name(&ref_idx, "search_fn", 10, 0.0);

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

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.5, // Low weight
    };

    // High threshold should filter out results (score * weight < threshold)
    let results = cqs::reference::search_reference_by_name(&ref_idx, "test_fn", 10, 0.9);

    assert!(
        results.is_empty(),
        "High threshold should filter out results with low weight"
    );
}
