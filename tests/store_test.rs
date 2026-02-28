//! Store tests

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::normalize_for_fts;
use cqs::parser::{ChunkType, Language};
use cqs::store::SearchFilter;
use std::collections::HashSet;
use std::path::PathBuf;

#[test]
fn test_store_init() {
    let store = TestStore::new();

    // Stats should show empty index
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_chunks, 0);
    assert_eq!(stats.total_files, 0);
    assert_eq!(stats.schema_version, 11); // v11: type_edges table
    assert_eq!(stats.model_name, "intfloat/e5-base-v2");
}

#[test]
fn test_upsert_and_search() {
    let store = TestStore::new();

    // Insert a chunk
    let chunk = test_chunk("add", "fn add(a: i32, b: i32) -> i32 { a + b }");
    let embedding = mock_embedding(1.0);
    store.upsert_chunk(&chunk, &embedding, Some(12345)).unwrap();

    // Search should find it
    let results = store.search_embedding_only(&embedding, 5, 0.0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "add");
    assert!(
        results[0].score > 0.99,
        "Identical embedding should have score ~1.0"
    );
}

#[test]
fn test_search_with_threshold() {
    let store = TestStore::new();

    // Insert chunks with different embeddings
    let chunk1 = test_chunk("add", "fn add(a, b) { a + b }");
    let chunk2 = test_chunk("subtract", "fn subtract(a, b) { a - b }");

    store
        .upsert_chunk(&chunk1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(-1.0), Some(12345))
        .unwrap();

    // Search with query similar to chunk1
    let query = mock_embedding(0.9);
    let results = store.search_embedding_only(&query, 5, 0.5).unwrap();

    // Should find chunk1 (similar) but not chunk2 (dissimilar)
    assert!(results.iter().any(|r| r.chunk.name == "add"));
}

#[test]
fn test_search_limit() {
    let store = TestStore::new();

    // Insert multiple chunks
    for i in 0..10 {
        let chunk = test_chunk(&format!("fn{}", i), &format!("fn fn{}() {{}}", i));
        let emb = mock_embedding(1.0 + i as f32 * 0.01);
        store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();
    }

    // Search with limit
    let query = mock_embedding(1.0);
    let results = store.search_embedding_only(&query, 3, 0.0).unwrap();

    assert_eq!(results.len(), 3);
}

#[test]
fn test_search_filtered_by_language() {
    let store = TestStore::new();

    // Insert Rust chunk
    let rust_chunk = test_chunk("rust_fn", "fn rust_fn() {}");
    store
        .upsert_chunk(&rust_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Insert Python chunk
    let mut py_chunk = test_chunk("py_fn", "def py_fn(): pass");
    py_chunk.language = Language::Python;
    py_chunk.file = PathBuf::from("test.py");
    store
        .upsert_chunk(&py_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Search for Rust only
    let filter = SearchFilter {
        languages: Some(vec![Language::Rust]),
        path_pattern: None,
        ..Default::default()
    };
    let results = store
        .search_filtered(&mock_embedding(1.0), &filter, 10, 0.0)
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "rust_fn");
}

#[test]
fn test_needs_reindex_not_indexed() {
    let store = TestStore::new();

    // Create a temp file that's not indexed
    let dir = tempfile::TempDir::new().unwrap();
    let file_path = dir.path().join("new_file.rs");
    std::fs::write(&file_path, "fn test() {}").unwrap();

    // File not in index should need reindexing (returns Some(mtime))
    let needs = store.needs_reindex(&file_path).unwrap();
    assert!(
        needs.is_some(),
        "File not in index should need reindexing (return Some(mtime))"
    );
}

#[test]
fn test_delete_by_origin() {
    let store = TestStore::new();

    // Insert chunks from two files
    let chunk1 = test_chunk("fn1", "fn fn1() {}");
    let mut chunk2 = test_chunk("fn2", "fn fn2() {}");
    chunk2.file = PathBuf::from("other.rs");
    chunk2.id = format!("other.rs:1:{}", &chunk2.content_hash[..8]);

    store
        .upsert_chunk(&chunk1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Delete chunks from test.rs
    let deleted = store.delete_by_origin(&PathBuf::from("test.rs")).unwrap();
    assert_eq!(deleted, 1);

    // Only chunk2 should remain
    let results = store
        .search_embedding_only(&mock_embedding(1.0), 10, 0.0)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "fn2");

    // Deleting again should return 0
    let deleted_again = store.delete_by_origin(&PathBuf::from("test.rs")).unwrap();
    assert_eq!(deleted_again, 0);
}

#[test]
fn test_prune_missing() {
    let store = TestStore::new();

    // Insert chunks from two files
    let chunk1 = test_chunk("fn1", "fn fn1() {}");
    let mut chunk2 = test_chunk("fn2", "fn fn2() {}");
    chunk2.file = PathBuf::from("other.rs");
    chunk2.id = format!("other.rs:1:{}", &chunk2.content_hash[..8]);

    store
        .upsert_chunk(&chunk1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Prune with only test.rs existing
    let existing: HashSet<PathBuf> = vec![PathBuf::from("test.rs")].into_iter().collect();
    let pruned = store.prune_missing(&existing).unwrap();

    assert_eq!(pruned, 1);

    // Only chunk1 should remain
    let results = store
        .search_embedding_only(&mock_embedding(1.0), 10, 0.0)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.name, "fn1");
}

#[test]
fn test_get_by_content_hash() {
    let store = TestStore::new();

    let content = "fn test() { 42 }";
    let chunk = test_chunk("test", content);
    let embedding = mock_embedding(0.5);
    store.upsert_chunk(&chunk, &embedding, Some(12345)).unwrap();

    // Should find embedding by content hash
    let found = store.get_by_content_hash(&chunk.content_hash);
    assert!(found.is_some());

    // Should not find non-existent hash
    let not_found = store.get_by_content_hash("nonexistent");
    assert!(not_found.is_none());
}

#[test]
fn test_get_embeddings_by_hashes() {
    let store = TestStore::new();

    // Insert two chunks with different content
    let chunk1 = test_chunk("fn1", "fn fn1() { 1 }");
    let chunk2 = test_chunk("fn2", "fn fn2() { 2 }");
    let emb1 = mock_embedding(0.1);
    let emb2 = mock_embedding(0.2);

    store.upsert_chunk(&chunk1, &emb1, Some(12345)).unwrap();
    store.upsert_chunk(&chunk2, &emb2, Some(12345)).unwrap();

    // Query both hashes + one non-existent
    let hashes = vec![
        chunk1.content_hash.as_str(),
        chunk2.content_hash.as_str(),
        "nonexistent_hash",
    ];
    let result = store.get_embeddings_by_hashes(&hashes).unwrap();

    // Should find exactly 2
    assert_eq!(result.len(), 2);
    assert!(result.contains_key(&chunk1.content_hash));
    assert!(result.contains_key(&chunk2.content_hash));
    assert!(!result.contains_key("nonexistent_hash"));

    // Empty input should return empty map
    let empty_result = store.get_embeddings_by_hashes(&[]).unwrap();
    assert!(empty_result.is_empty());
}

#[test]
fn test_stats() {
    let store = TestStore::new();

    // Insert various chunks
    let chunk1 = test_chunk("fn1", "fn fn1() {}");
    let mut chunk2 = test_chunk("fn2", "fn fn2() {}");
    chunk2.file = PathBuf::from("other.rs");
    chunk2.id = format!("other.rs:1:{}", &chunk2.content_hash[..8]);

    let mut chunk3 = test_chunk("method1", "fn method1(&self) {}");
    chunk3.chunk_type = ChunkType::Method;

    store
        .upsert_chunk(&chunk1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk3, &mock_embedding(1.0), Some(12345))
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
    let store = TestStore::new();

    // Insert chunks with distinctive names
    let chunk1 = test_chunk(
        "parseConfigFile",
        "fn parseConfigFile() { /* parse config */ }",
    );
    let chunk2 = test_chunk(
        "loadUserSettings",
        "fn loadUserSettings() { /* load settings */ }",
    );
    let chunk3 = test_chunk("calculateTotal", "fn calculateTotal() { /* math */ }");

    store
        .upsert_chunk(&chunk1, &mock_embedding(0.1), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(0.2), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk3, &mock_embedding(0.3), Some(12345))
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
    let store = TestStore::new();

    // Insert chunks
    let chunk1 = test_chunk("handleError", "fn handleError(err: Error) { log(err); }");
    let chunk2 = test_chunk(
        "processData",
        "fn processData(data: Vec<u8>) { /* process */ }",
    );

    store
        .upsert_chunk(&chunk1, &mock_embedding(0.5), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(0.5), Some(12345))
        .unwrap();

    // Search with RRF enabled
    let filter = SearchFilter {
        enable_rrf: true,
        query_text: "error handling".to_string(),
        ..Default::default()
    };

    let results = store
        .search_filtered(&mock_embedding(0.5), &filter, 5, 0.0)
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

#[test]
fn test_normalize_for_fts_strips_fts5_special_chars() {
    // FTS5 special characters should be filtered out to prevent query manipulation
    // See: https://www.sqlite.org/fts5.html#full_text_query_syntax

    // Wildcards - stripped
    assert_eq!(normalize_for_fts("test*"), "test");
    assert_eq!(normalize_for_fts("*test*"), "test");

    // Phrase quotes - stripped
    assert_eq!(normalize_for_fts("\"exact phrase\""), "exact phrase");

    // Column filters - colon stripped
    assert_eq!(normalize_for_fts("content:test"), "content test");
    assert_eq!(normalize_for_fts("name:foo"), "name foo");

    // Boolean-like words become harmless lowercase tokens
    // (FTS5 default mode doesn't treat AND/OR as operators anyway)
    assert_eq!(normalize_for_fts("-excluded"), "excluded");
    assert_eq!(normalize_for_fts("+required"), "required");

    // Grouping parens - stripped
    assert_eq!(normalize_for_fts("(test)"), "test");

    // Boost/caret - stripped, number becomes separate token
    assert_eq!(normalize_for_fts("test^2"), "test 2");

    // Slash - stripped
    assert_eq!(normalize_for_fts("test/other"), "test other");

    // Mixed potentially malicious input - all special chars stripped
    // Note: ALL_CAPS words get split letter-by-letter by tokenize_identifier
    // (designed for camelCase, treats each capital as word boundary)
    assert_eq!(normalize_for_fts("*\"content:*\""), "content");
    assert_eq!(
        normalize_for_fts("test; DROP TABLE--"),
        "test d r o p t a b l e"
    );
}

// ===== Schema Error Path Tests =====

#[test]
fn test_future_schema_version_rejected() {
    // Manually create a database with a schema version higher than current
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("future.db");

    // Create database with future schema version
    {
        let store = cqs::store::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();
    }

    // Now manually update the schema version to a future value
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(&format!("sqlite://{}", db_path.display()))
            .await
            .unwrap();
        sqlx::query("UPDATE metadata SET value = '999' WHERE key = 'schema_version'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    });

    // Re-opening should fail with "newer than cq" error
    let result = cqs::store::Store::open(&db_path);
    match result {
        Ok(_) => panic!("Future schema version should be rejected"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("newer") || err_msg.contains("upgrade"),
                "Error should mention newer version: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_old_schema_version_rejected() {
    // Create a database with an old schema version
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("old.db");

    // Create database
    {
        let store = cqs::store::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();
    }

    // Manually downgrade the schema version
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(&format!("sqlite://{}", db_path.display()))
            .await
            .unwrap();
        sqlx::query("UPDATE metadata SET value = '5' WHERE key = 'schema_version'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    });

    // Re-opening should fail with schema mismatch
    let result = cqs::store::Store::open(&db_path);
    match result {
        Ok(_) => panic!("Old schema version should be rejected"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("mismatch") || err_msg.contains("--force"),
                "Error should mention mismatch or rebuild: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_model_mismatch_rejected() {
    // Create a database with a different model name
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("model.db");

    // Create database
    {
        let store = cqs::store::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();
    }

    // Change the model name to something different
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(&format!("sqlite://{}", db_path.display()))
            .await
            .unwrap();
        sqlx::query("UPDATE metadata SET value = 'different-model' WHERE key = 'model_name'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    });

    // Re-opening should fail with model mismatch
    let result = cqs::store::Store::open(&db_path);
    match result {
        Ok(_) => panic!("Model mismatch should be rejected"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("mismatch") || err_msg.contains("Model"),
                "Error should mention model mismatch: {}",
                err_msg
            );
        }
    }
}

// ===== Streaming Embeddings Tests =====

#[test]
fn test_embedding_batches() {
    let store = TestStore::new();

    // Insert 25 chunks
    for i in 0..25 {
        let chunk = test_chunk(&format!("fn{}", i), &format!("fn fn{}() {{}}", i));
        let emb = mock_embedding(i as f32);
        store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();
    }

    // Fetch in batches of 10
    let batches: Vec<_> = store.embedding_batches(10).collect();

    // Should have 3 batches (10, 10, 5)
    assert_eq!(batches.len(), 3, "Expected 3 batches for 25 chunks");

    let batch1 = batches[0].as_ref().unwrap();
    let batch2 = batches[1].as_ref().unwrap();
    let batch3 = batches[2].as_ref().unwrap();

    assert_eq!(batch1.len(), 10, "First batch should have 10 embeddings");
    assert_eq!(batch2.len(), 10, "Second batch should have 10 embeddings");
    assert_eq!(batch3.len(), 5, "Third batch should have 5 embeddings");

    // Total embeddings should match chunk count
    let total: usize = batches
        .iter()
        .filter_map(|b| b.as_ref().ok())
        .map(|b| b.len())
        .sum();
    assert_eq!(total, 25);
}

#[test]
fn test_embedding_batches_empty() {
    let store = TestStore::new();

    // No chunks inserted
    let batches: Vec<_> = store.embedding_batches(10).collect();
    assert!(batches.is_empty(), "Empty store should yield no batches");
}

#[test]
fn test_embedding_batches_exact_multiple() {
    let store = TestStore::new();

    // Insert exactly 20 chunks (divisible by batch size)
    for i in 0..20 {
        let chunk = test_chunk(&format!("fn{}", i), &format!("fn fn{}() {{}}", i));
        let emb = mock_embedding(i as f32);
        store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();
    }

    let batches: Vec<_> = store.embedding_batches(10).collect();
    assert_eq!(batches.len(), 2, "20 chunks / 10 batch = 2 batches");

    for batch in &batches {
        let b = batch.as_ref().unwrap();
        assert_eq!(b.len(), 10);
    }
}

// ===== Unicode FTS tests (T16) =====

#[test]
fn test_fts_unicode_function_names() {
    let store = TestStore::new();

    // Insert chunks with Unicode in names
    let mut chunk = test_chunk("计算", "fn 计算() { /* calculate */ }");
    chunk.content = "fn 计算() { /* calculate */ }".to_string();
    chunk.name = "计算".to_string();
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // FTS should normalize and find it (using query text)
    let filter = SearchFilter::new().with_query("计算");
    let results = store.search_filtered(&mock_embedding(1.0), &filter, 5, 0.0);
    assert!(results.is_ok(), "FTS search should not fail on Unicode");
    // Note: Whether it actually finds depends on FTS tokenization; test ensures no crash
}

#[test]
fn test_fts_emoji_in_comments() {
    let store = TestStore::new();

    // Insert chunk with emoji in content
    let mut chunk = test_chunk("emoji_fn", "fn emoji_fn() { /* 🚀 launch */ }");
    chunk.content = "fn emoji_fn() { /* 🚀 launch */ }".to_string();
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Should not crash on emoji content
    let filter = SearchFilter::new().with_query("launch");
    let results = store.search_filtered(&mock_embedding(1.0), &filter, 5, 0.0);
    assert!(
        results.is_ok(),
        "FTS search should not fail with emoji in content"
    );
}

#[test]
fn test_normalize_for_fts_unicode() {
    // Test that normalization handles various Unicode correctly
    assert!(!normalize_for_fts("hello 世界").is_empty());
    assert!(!normalize_for_fts("emoji 🎉 test").is_empty());
    assert!(!normalize_for_fts("diacritics: café résumé").is_empty());
    // RTL text
    assert!(!normalize_for_fts("rtl: שלום").is_empty());
}

// ===== Call Graph Tests (#7) =====

#[test]
fn test_get_call_graph() {
    use cqs::parser::{CallSite, FunctionCalls};

    let store = TestStore::new();

    // Insert chunks
    let chunk_a = test_chunk("func_a", "fn func_a() { func_b(); func_c(); }");
    let chunk_b = test_chunk("func_b", "fn func_b() { func_c(); }");
    let chunk_c = test_chunk("func_c", "fn func_c() {}");

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_a, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_b, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_c, &emb, Some(12345)).unwrap();

    // Insert call edges: func_a → func_b, func_a → func_c, func_b → func_c
    let function_calls = vec![
        FunctionCalls {
            name: "func_a".to_string(),
            line_start: 1,
            calls: vec![
                CallSite {
                    callee_name: "func_b".to_string(),
                    line_number: 1,
                },
                CallSite {
                    callee_name: "func_c".to_string(),
                    line_number: 1,
                },
            ],
        },
        FunctionCalls {
            name: "func_b".to_string(),
            line_start: 5,
            calls: vec![CallSite {
                callee_name: "func_c".to_string(),
                line_number: 5,
            }],
        },
    ];
    store
        .upsert_function_calls(&PathBuf::from("test.rs"), &function_calls)
        .unwrap();

    // Get call graph
    let graph = store.get_call_graph().unwrap();

    // Verify forward edges (caller → callees)
    assert_eq!(
        graph.forward.get("func_a").map(|v| v.len()),
        Some(2),
        "func_a should call 2 functions"
    );
    assert_eq!(
        graph.forward.get("func_b").map(|v| v.len()),
        Some(1),
        "func_b should call 1 function"
    );
    assert!(
        !graph.forward.contains_key("func_c"),
        "func_c should call nothing"
    );

    // Verify reverse edges (callee → callers)
    assert_eq!(
        graph.reverse.get("func_c").map(|v| v.len()),
        Some(2),
        "func_c should be called by 2 functions"
    );
    assert_eq!(
        graph.reverse.get("func_b").map(|v| v.len()),
        Some(1),
        "func_b should be called by 1 function"
    );
    assert!(
        !graph.reverse.contains_key("func_a"),
        "func_a should not be called by anyone"
    );
}

// ===== Chunk Identities Test (#6) =====

#[test]
fn test_all_chunk_identities() {
    let store = TestStore::new();

    // Insert chunks with various properties
    let chunk1 = test_chunk("fn1", "fn fn1() {}");
    let mut chunk2 = test_chunk("fn2", "fn fn2() {}");
    chunk2.file = PathBuf::from("other.rs");
    chunk2.id = format!("other.rs:1:{}", &chunk2.content_hash[..8]);
    chunk2.line_start = 10;

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk1, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk2, &emb, Some(12345)).unwrap();

    // Get all identities
    let identities = store.all_chunk_identities().unwrap();

    assert_eq!(identities.len(), 2, "Should return 2 chunk identities");

    // Find chunk1 identity
    let id1 = identities.iter().find(|i| i.name == "fn1").unwrap();
    assert_eq!(id1.origin, "test.rs");
    assert_eq!(id1.language, Language::Rust);
    assert_eq!(id1.line_start, 1);

    // Find chunk2 identity
    let id2 = identities.iter().find(|i| i.name == "fn2").unwrap();
    assert_eq!(id2.origin, "other.rs");
    assert_eq!(id2.line_start, 10);
}

// ===== Get Chunk With Embedding Test (#6) =====

#[test]
fn test_get_chunk_with_embedding() {
    let store = TestStore::new();

    let chunk = test_chunk("test_fn", "fn test_fn() { 42 }");
    let embedding = mock_embedding(0.75);
    store.upsert_chunk(&chunk, &embedding, Some(12345)).unwrap();

    // Retrieve chunk with embedding
    let result = store.get_chunk_with_embedding(&chunk.id).unwrap();
    assert!(result.is_some(), "Should find the chunk");

    let (retrieved_chunk, retrieved_emb) = result.unwrap();
    assert_eq!(retrieved_chunk.name, "test_fn");
    assert_eq!(retrieved_chunk.id, chunk.id);

    // Embedding should match (dimensions should be same)
    assert_eq!(retrieved_emb.as_slice().len(), embedding.as_slice().len());
}

#[test]
fn test_get_chunk_with_embedding_nonexistent() {
    let store = TestStore::new();

    // Query for non-existent chunk
    let result = store
        .get_chunk_with_embedding("nonexistent:1:abcd1234")
        .unwrap();
    assert!(
        result.is_none(),
        "Should return None for non-existent chunk"
    );
}

// ===== Store::close() Test (#239) =====

#[test]
fn test_store_close() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test_close.db");

    // Open and initialize store
    {
        let store = cqs::store::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Insert a chunk to have some data
        let chunk = test_chunk("test_fn", "fn test_fn() {}");
        let emb = mock_embedding(1.0);
        store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();

        // Close the store (consumes it)
        store.close().unwrap();
    }

    // Reopen to verify database is consistent after close
    let store = cqs::store::Store::open(&db_path).unwrap();
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_chunks, 1, "Chunk should persist after close");

    // Search should still work
    let results = store
        .search_embedding_only(&mock_embedding(1.0), 5, 0.0)
        .unwrap();
    assert_eq!(results.len(), 1, "Should find the persisted chunk");
}

// ===== FTS Edge Cases Tests (#239) =====

#[test]
fn test_fts_empty_string() {
    let store = TestStore::new();

    // Insert a chunk
    let chunk = test_chunk("test_fn", "fn test_fn() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Empty string query should not panic
    let results = store.search_fts("", 5).unwrap();
    assert!(
        results.is_empty(),
        "Empty query should return no results or all results"
    );
}

#[test]
fn test_fts_special_characters() {
    let store = TestStore::new();

    // Insert chunks with special characters in names
    let chunk1 = test_chunk("foo::bar", "fn foo::bar() {}");
    let chunk2 = test_chunk("Vec<T>", "struct Vec<T> {}");
    let chunk3 = test_chunk("quoted_name", "fn \"quoted\" name");

    store
        .upsert_chunk(&chunk1, &mock_embedding(0.1), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(0.2), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk3, &mock_embedding(0.3), Some(12345))
        .unwrap();

    // Search with special characters - should not panic
    let results = store.search_fts("foo::bar", 5);
    assert!(results.is_ok(), "FTS should handle :: in query");

    let results = store.search_fts("Vec<T>", 5);
    assert!(results.is_ok(), "FTS should handle angle brackets in query");

    let results = store.search_fts("\"quoted\"", 5);
    assert!(results.is_ok(), "FTS should handle quotes in query");
}

#[test]
fn test_fts_sql_injection_characters() {
    let store = TestStore::new();

    let chunk = test_chunk("test_fn", "fn test_fn() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // SQL-like characters should be sanitized by normalize_for_fts
    let results = store.search_fts("'; DROP TABLE chunks--", 5);
    assert!(
        results.is_ok(),
        "FTS should safely handle SQL injection attempts"
    );

    // Should not break or drop tables
    let stats = store.stats().unwrap();
    assert_eq!(stats.total_chunks, 1, "Database should remain intact");
}

#[test]
fn test_fts_unicode_queries() {
    let store = TestStore::new();

    // Insert chunks with Unicode content
    let mut chunk1 = test_chunk("calculate", "fn calculate() {}");
    chunk1.content = "fn calculate() { /* 计算 */ }".to_string();

    let mut chunk2 = test_chunk("über", "fn über() {}");
    chunk2.name = "über".to_string();

    store
        .upsert_chunk(&chunk1, &mock_embedding(0.1), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(0.2), Some(12345))
        .unwrap();

    // CJK query
    let results = store.search_fts("计算", 5);
    assert!(results.is_ok(), "FTS should handle CJK characters");

    // Diacritics query
    let results = store.search_fts("über", 5);
    assert!(results.is_ok(), "FTS should handle diacritics");
}

#[test]
fn test_fts_very_long_query() {
    let store = TestStore::new();

    let chunk = test_chunk("test_fn", "fn test_fn() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Very long query string (1000+ chars)
    let long_query = "test ".repeat(250); // 1250 characters
    let results = store.search_fts(&long_query, 5);
    assert!(
        results.is_ok(),
        "FTS should handle very long queries without panic"
    );
}

// ===== search_chunks_by_signature tests (P3-15) =====

#[test]
fn test_search_chunks_by_signature_finds_matching() {
    let store = TestStore::new();

    // Insert chunks with different signatures
    let mut chunk1 = test_chunk("parse_config", "fn parse_config(cfg: Config) -> Result {}");
    chunk1.signature = "fn parse_config(cfg: Config) -> Result".to_string();
    chunk1.chunk_type = ChunkType::Function;

    let mut chunk2 = test_chunk(
        "validate_config",
        "fn validate_config(cfg: Config) -> bool {}",
    );
    chunk2.id = "test.rs:10:deadbeef".to_string();
    chunk2.signature = "fn validate_config(cfg: Config) -> bool".to_string();
    chunk2.chunk_type = ChunkType::Function;

    let mut chunk3 = test_chunk("render_ui", "fn render_ui(state: AppState) {}");
    chunk3.id = "test.rs:20:cafebabe".to_string();
    chunk3.signature = "fn render_ui(state: AppState)".to_string();
    chunk3.chunk_type = ChunkType::Function;

    store
        .upsert_chunk(&chunk1, &mock_embedding(0.1), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(0.2), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk3, &mock_embedding(0.3), Some(12345))
        .unwrap();

    // Search for "Config" in signatures — should find chunk1 and chunk2
    let results = store.search_chunks_by_signature("Config").unwrap();
    let names: HashSet<String> = results.iter().map(|c| c.name.clone()).collect();
    assert!(
        names.contains("parse_config"),
        "Should find parse_config (signature contains Config)"
    );
    assert!(
        names.contains("validate_config"),
        "Should find validate_config (signature contains Config)"
    );
    assert!(
        !names.contains("render_ui"),
        "Should NOT find render_ui (signature has AppState, not Config)"
    );
}

#[test]
fn test_search_chunks_by_signature_no_match() {
    let store = TestStore::new();

    let mut chunk = test_chunk("simple_fn", "fn simple_fn(x: i32) -> i32 {}");
    chunk.signature = "fn simple_fn(x: i32) -> i32".to_string();
    chunk.chunk_type = ChunkType::Function;

    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    let results = store.search_chunks_by_signature("NonExistentType").unwrap();
    assert!(
        results.is_empty(),
        "Should return empty for non-matching type name"
    );
}

#[test]
fn test_search_chunks_by_signature_only_functions_and_methods() {
    let store = TestStore::new();

    // Insert a struct chunk (not function/method)
    let mut chunk = test_chunk("MyStruct", "struct MyStruct { field: Config }");
    chunk.signature = "struct MyStruct".to_string();
    chunk.chunk_type = ChunkType::Struct;

    // Insert a function chunk
    let mut fn_chunk = test_chunk("use_config", "fn use_config(c: Config) {}");
    fn_chunk.id = "test.rs:10:abcd1234".to_string();
    fn_chunk.signature = "fn use_config(c: Config)".to_string();
    fn_chunk.chunk_type = ChunkType::Function;

    store
        .upsert_chunk(&chunk, &mock_embedding(0.1), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&fn_chunk, &mock_embedding(0.2), Some(12345))
        .unwrap();

    // search_chunks_by_signature only returns function/method chunks
    let results = store.search_chunks_by_signature("Config").unwrap();
    assert_eq!(
        results.len(),
        1,
        "Should only find the function, not struct"
    );
    assert_eq!(results[0].name, "use_config");
}

// ===== check_origins_stale batch boundary test (TC-3) =====

#[test]
fn test_check_origins_stale_across_batch_boundary() {
    use tempfile::TempDir;

    let store = TestStore::new();
    let project_dir = TempDir::new().unwrap();
    let root = project_dir.path();

    // Create 950 distinct origin files — this crosses the 900-item batch boundary
    // in check_origins_stale (BATCH_SIZE = 900).
    let count = 950;
    let emb = mock_embedding(1.0);

    for i in 0..count {
        let filename = format!("file_{:04}.rs", i);
        let filepath = root.join(&filename);

        // Create the file on disk
        std::fs::write(&filepath, format!("fn f{}() {{}}", i)).unwrap();

        // Create a chunk with this origin
        let content = format!("fn f{}() {{ {} }}", i, i);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = cqs::parser::Chunk {
            id: format!("{}:1:{}", &filename, &hash[..8]),
            file: PathBuf::from(&filename),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: format!("f{}", i),
            signature: format!("fn f{}()", i),
            content,
            doc: None,
            line_start: 1,
            line_end: 3,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };

        // Use a synthetic mtime: first half are old (stale), second half are current
        let mtime = if i < count / 2 {
            // Old mtime — file on disk will be newer, so these should be stale
            1000i64
        } else {
            // Future mtime — file on disk will be older, so these should be fresh
            i64::MAX / 2
        };

        store
            .upsert_chunks_batch(&[(chunk, emb.clone())], Some(mtime))
            .unwrap();
    }

    // Build the full list of origins
    let origins: Vec<String> = (0..count).map(|i| format!("file_{:04}.rs", i)).collect();
    let origin_refs: Vec<&str> = origins.iter().map(|s| s.as_str()).collect();

    // Call check_origins_stale with all 950 origins (crosses 900 batch boundary)
    let stale = store.check_origins_stale(&origin_refs, root).unwrap();

    // First half (0..475) had mtime=1000, files on disk are newer → stale
    for i in 0..count / 2 {
        let origin = format!("file_{:04}.rs", i);
        assert!(
            stale.contains(&origin),
            "Origin {} should be stale (old mtime), batch boundary at 900",
            origin
        );
    }

    // Second half (475..950) had mtime=MAX/2, files on disk are older → fresh
    for i in count / 2..count {
        let origin = format!("file_{:04}.rs", i);
        assert!(
            !stale.contains(&origin),
            "Origin {} should be fresh (future mtime)",
            origin
        );
    }

    // Verify counts
    let expected_stale = count / 2;
    assert_eq!(
        stale.len(),
        expected_stale,
        "Expected {} stale origins across batch boundary, got {}",
        expected_stale,
        stale.len()
    );
}
