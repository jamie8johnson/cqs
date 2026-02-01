//! Store tests

use cqs::embedder::Embedding;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::store::{normalize_for_fts, ModelInfo, SearchFilter, Store};
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

    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Stats should show empty index
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_chunks, 0);
    assert_eq!(stats.total_files, 0);
    assert_eq!(stats.schema_version, 3); // v3: NL-based embeddings
    assert_eq!(stats.model_name, "nomic-embed-text-v1.5");
}

#[test]
fn test_upsert_and_search() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let store = Store::open(&db_path).unwrap();
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

    let store = Store::open(&db_path).unwrap();
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

    let store = Store::open(&db_path).unwrap();
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

    let store = Store::open(&db_path).unwrap();
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

    let store = Store::open(&db_path).unwrap();
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

    let store = Store::open(&db_path).unwrap();
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
fn test_get_embeddings_by_hashes() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert two chunks with different content
    let chunk1 = create_test_chunk("fn1", "fn fn1() { 1 }");
    let chunk2 = create_test_chunk("fn2", "fn fn2() { 2 }");
    let emb1 = create_mock_embedding(0.1);
    let emb2 = create_mock_embedding(0.2);

    store.upsert_chunk(&chunk1, &emb1, 12345).unwrap();
    store.upsert_chunk(&chunk2, &emb2, 12345).unwrap();

    // Query both hashes + one non-existent
    let hashes = vec![
        chunk1.content_hash.as_str(),
        chunk2.content_hash.as_str(),
        "nonexistent_hash",
    ];
    let result = store.get_embeddings_by_hashes(&hashes);

    // Should find exactly 2
    assert_eq!(result.len(), 2);
    assert!(result.contains_key(&chunk1.content_hash));
    assert!(result.contains_key(&chunk2.content_hash));
    assert!(!result.contains_key("nonexistent_hash"));

    // Empty input should return empty map
    let empty_result = store.get_embeddings_by_hashes(&[]);
    assert!(empty_result.is_empty());
}

#[test]
fn test_stats() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let store = Store::open(&db_path).unwrap();
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

#[test]
fn test_fts_search() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert chunks with distinctive names
    let chunk1 = create_test_chunk(
        "parseConfigFile",
        "fn parseConfigFile() { /* parse config */ }",
    );
    let chunk2 = create_test_chunk(
        "loadUserSettings",
        "fn loadUserSettings() { /* load settings */ }",
    );
    let chunk3 = create_test_chunk("calculateTotal", "fn calculateTotal() { /* math */ }");

    store
        .upsert_chunk(&chunk1, &create_mock_embedding(0.1), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk2, &create_mock_embedding(0.2), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk3, &create_mock_embedding(0.3), 12345)
        .unwrap();

    // FTS search for "config" should find parseConfigFile
    let results = store.search_fts("config", 5).unwrap();
    assert!(
        !results.is_empty(),
        "FTS should find 'config' in parseConfigFile"
    );
    assert!(results
        .iter()
        .any(|id| id.contains("parseConfigFile") || id.starts_with("test.rs")));

    // FTS search for "parse file" should also find parseConfigFile (normalized)
    let results = store.search_fts("parse file", 5).unwrap();
    assert!(
        !results.is_empty(),
        "FTS should find 'parse file' via normalization"
    );

    // FTS search for "settings" should find loadUserSettings
    let results = store.search_fts("settings", 5).unwrap();
    assert!(!results.is_empty(), "FTS should find 'settings'");

    // FTS search for nonexistent term
    let results = store.search_fts("xyznonexistent", 5).unwrap();
    assert!(
        results.is_empty(),
        "FTS should return empty for nonexistent term"
    );
}

#[test]
fn test_rrf_search() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");

    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Insert chunks
    let chunk1 = create_test_chunk("handleError", "fn handleError(err: Error) { log(err); }");
    let chunk2 = create_test_chunk(
        "processData",
        "fn processData(data: Vec<u8>) { /* process */ }",
    );

    store
        .upsert_chunk(&chunk1, &create_mock_embedding(0.5), 12345)
        .unwrap();
    store
        .upsert_chunk(&chunk2, &create_mock_embedding(0.5), 12345)
        .unwrap();

    // Search with RRF enabled
    let filter = SearchFilter {
        enable_rrf: true,
        query_text: "error handling".to_string(),
        ..Default::default()
    };

    let results = store
        .search_filtered(&create_mock_embedding(0.5), &filter, 5, 0.0)
        .unwrap();

    // Should return results (RRF combines semantic + FTS)
    assert!(!results.is_empty(), "RRF search should return results");
}

#[test]
fn test_normalize_for_fts() {
    // camelCase
    assert_eq!(normalize_for_fts("parseConfigFile"), "parse config file");

    // snake_case
    assert_eq!(normalize_for_fts("parse_config_file"), "parse config file");

    // PascalCase
    assert_eq!(normalize_for_fts("ParseConfigFile"), "parse config file");

    // Mixed with punctuation
    assert_eq!(
        normalize_for_fts("fn parseConfig() { return value; }"),
        "fn parse config return value"
    );

    // Numbers preserved
    assert_eq!(
        normalize_for_fts("parseVersion2Config"),
        "parse version2 config"
    );

    // Already normalized
    assert_eq!(normalize_for_fts("hello world"), "hello world");

    // Empty string
    assert_eq!(normalize_for_fts(""), "");

    // Single word
    assert_eq!(normalize_for_fts("parse"), "parse");
}
