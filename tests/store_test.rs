//! Store tests

use cqs::embedder::Embedding;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::store::{ModelInfo, SearchFilter, Store};
use std::collections::HashSet;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_test_chunk(name: &str, content: &str) -> Chunk {
    Chunk {
        id: format!(
            "test.rs:1:{}",
            &blake3::hash(content.as_bytes()).to_hex()[..8]
        ),
        file: PathBuf::from("test.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: content.to_string(),
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
    }
}

fn create_mock_embedding(seed: f32) -> Embedding {
    // Create a simple mock embedding (not L2 normalized, but good enough for testing)
    let mut v = vec![seed; 768];
    // Normalize
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding(v)
}

#[test]
fn test_store_init() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Stats should show empty index
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_chunks, 0);
    assert_eq!(stats.total_files, 0);
    assert_eq!(stats.schema_version, 1);
    assert_eq!(stats.model_name, "nomic-embed-text-v1.5");
}

#[test]
fn test_upsert_and_search() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert a chunk
    let chunk = create_test_chunk("add", "fn add(a: i32, b: i32) -> i32 { a + b }");
    let embedding = create_mock_embedding(1.0);
    store.upsert_chunk(&chunk, &embedding, 12345).unwrap();

    // Search should find it
    let results = store.search(&embedding, 5, 0.0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "add");
    assert!(
        results[0].score > 0.99,
        "Identical embedding should have score ~1.0"
    );
}

#[test]
fn test_search_with_threshold() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert chunks with different embeddings
    let chunk1 = create_test_chunk("add", "fn add(a, b) { a + b }");
    let chunk2 = create_test_chunk("subtract", "fn subtract(a, b) { a - b }");

    store
        .upsert_chunk(&chunk1, &create_mock_embedding(1.0), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk2, &create_mock_embedding(-1.0), 12345)
        .unwrap();

    // Search with query similar to chunk1
    let query = create_mock_embedding(0.9);
    let results = store.search(&query, 5, 0.5).unwrap();

    // Should find chunk1 (similar) but not chunk2 (dissimilar)
    assert!(results.iter().any(|r| r.chunk.name == "add"));
}

#[test]
fn test_search_limit() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert multiple chunks
    for i in 0..10 {
        let chunk = create_test_chunk(&format!("fn{}", i), &format!("fn fn{}() {{}}", i));
        let emb = create_mock_embedding(1.0 + i as f32 * 0.01);
        store.upsert_chunk(&chunk, &emb, 12345).unwrap();
    }

    // Search with limit
    let query = create_mock_embedding(1.0);
    let results = store.search(&query, 3, 0.0).unwrap();

    assert_eq!(results.len(), 3);
}

#[test]
fn test_search_filtered_by_language() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert Rust chunk
    let rust_chunk = create_test_chunk("rust_fn", "fn rust_fn() {}");
    store
        .upsert_chunk(&rust_chunk, &create_mock_embedding(1.0), 12345)
        .unwrap();

    // Insert Python chunk
    let mut py_chunk = create_test_chunk("py_fn", "def py_fn(): pass");
    py_chunk.language = Language::Python;
    py_chunk.file = PathBuf::from("test.py");
    store
        .upsert_chunk(&py_chunk, &create_mock_embedding(1.0), 12345)
        .unwrap();

    // Search for Rust only
    let filter = SearchFilter {
        languages: Some(vec![Language::Rust]),
        path_pattern: None,
        ..Default::default()
    };
    let results = store
        .search_filtered(&create_mock_embedding(1.0), &filter, 10, 0.0)
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "rust_fn");
}

#[test]
fn test_prune_missing() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert chunks from two files
    let chunk1 = create_test_chunk("fn1", "fn fn1() {}");
    let mut chunk2 = create_test_chunk("fn2", "fn fn2() {}");
    chunk2.file = PathBuf::from("other.rs");
    chunk2.id = format!("other.rs:1:{}", &chunk2.content_hash[..8]);

    store
        .upsert_chunk(&chunk1, &create_mock_embedding(1.0), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk2, &create_mock_embedding(1.0), 12345)
        .unwrap();

    // Prune with only test.rs existing
    let existing: HashSet<PathBuf> = vec![PathBuf::from("test.rs")].into_iter().collect();
    let pruned = store.prune_missing(&existing).unwrap();

    assert_eq!(pruned, 1);

    // Only chunk1 should remain
    let results = store.search(&create_mock_embedding(1.0), 10, 0.0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "fn1");
}

#[test]
fn test_get_by_content_hash() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    let content = "fn test() { 42 }";
    let chunk = create_test_chunk("test", content);
    let embedding = create_mock_embedding(0.5);
    store.upsert_chunk(&chunk, &embedding, 12345).unwrap();

    // Should find embedding by content hash
    let found = store.get_by_content_hash(&chunk.content_hash);
    assert!(found.is_some());

    // Should not find non-existent hash
    let not_found = store.get_by_content_hash("nonexistent");
    assert!(not_found.is_none());
}

#[test]
fn test_stats() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let mut store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert various chunks
    let chunk1 = create_test_chunk("fn1", "fn fn1() {}");
    let mut chunk2 = create_test_chunk("fn2", "fn fn2() {}");
    chunk2.file = PathBuf::from("other.rs");
    chunk2.id = format!("other.rs:1:{}", &chunk2.content_hash[..8]);

    let mut chunk3 = create_test_chunk("method1", "fn method1(&self) {}");
    chunk3.chunk_type = ChunkType::Method;

    store
        .upsert_chunk(&chunk1, &create_mock_embedding(1.0), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk2, &create_mock_embedding(1.0), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk3, &create_mock_embedding(1.0), 12345)
        .unwrap();

    let stats = store.stats().unwrap();

    assert_eq!(stats.total_chunks, 3);
    assert_eq!(stats.total_files, 2);
    assert_eq!(
        *stats.chunks_by_language.get(&Language::Rust).unwrap_or(&0),
        3
    );
    assert_eq!(
        *stats.chunks_by_type.get(&ChunkType::Function).unwrap_or(&0),
        2
    );
    assert_eq!(
        *stats.chunks_by_type.get(&ChunkType::Method).unwrap_or(&0),
        1
    );
}
