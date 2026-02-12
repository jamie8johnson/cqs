//! Dead code detection tests (#4)

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::{CallSite, FunctionCalls};

#[test]
fn test_find_dead_code_basic() {
    let store = TestStore::new();

    // Create chunks A, B, C
    let chunk_a = test_chunk("func_a", "fn func_a() { func_b(); }");
    let chunk_b = test_chunk("func_b", "fn func_b() { /* called by A */ }");
    let chunk_c = test_chunk("func_c", "fn func_c() { /* never called */ }");

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_a, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_b, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_c, &emb, Some(12345)).unwrap();

    // Insert call edge func_a → func_b using function_calls table
    let function_calls = vec![FunctionCalls {
        name: "func_a".to_string(),
        line_start: 1,
        calls: vec![CallSite {
            callee_name: "func_b".to_string(),
            line_number: 1,
        }],
    }];
    store
        .upsert_function_calls(&std::path::PathBuf::from("test.rs"), &function_calls)
        .unwrap();

    // Find dead code
    let (confident, _possibly_dead_pub) = store.find_dead_code(false).unwrap();

    // A and C should be in the list (no one calls them)
    // B should NOT be in the list (A calls it)
    let dead_names: Vec<&str> = confident.iter().map(|d| d.chunk.name.as_str()).collect();

    assert!(
        dead_names.contains(&"func_a"),
        "func_a has no callers, should be dead"
    );
    assert!(
        dead_names.contains(&"func_c"),
        "func_c has no callers, should be dead"
    );
    assert!(
        !dead_names.contains(&"func_b"),
        "func_b is called by func_a, should NOT be dead"
    );
}

#[test]
fn test_find_dead_code_excludes_main() {
    let store = TestStore::new();

    // Create main function
    let chunk_main = test_chunk("main", "fn main() { println!(\"hello\"); }");
    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_main, &emb, Some(12345)).unwrap();

    // Find dead code
    let (confident, _) = store.find_dead_code(false).unwrap();

    // main should not be in the dead code list
    let dead_names: Vec<&str> = confident.iter().map(|d| d.chunk.name.as_str()).collect();
    assert!(
        !dead_names.contains(&"main"),
        "main entry point should not be flagged as dead"
    );
}

#[test]
fn test_find_dead_code_pub_functions() {
    let store = TestStore::new();

    // Create public and private functions
    let chunk_pub = test_chunk("pub_fn", "pub fn pub_fn() { /* public */ }");
    let chunk_priv = test_chunk("priv_fn", "fn priv_fn() { /* private */ }");

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_pub, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_priv, &emb, Some(12345)).unwrap();

    // include_pub=false: public functions go to possibly_dead_pub
    let (confident, possibly_dead_pub) = store.find_dead_code(false).unwrap();

    let confident_names: Vec<&str> = confident.iter().map(|d| d.chunk.name.as_str()).collect();
    let pub_names: Vec<&str> = possibly_dead_pub
        .iter()
        .map(|d| d.chunk.name.as_str())
        .collect();

    assert!(
        confident_names.contains(&"priv_fn"),
        "Private function should be in confident list"
    );
    assert!(
        pub_names.contains(&"pub_fn"),
        "Public function should be in possibly_dead_pub list"
    );

    // include_pub=true: both should be in confident
    let (confident, possibly_dead_pub) = store.find_dead_code(true).unwrap();

    let confident_names: Vec<&str> = confident.iter().map(|d| d.chunk.name.as_str()).collect();

    assert!(
        confident_names.contains(&"priv_fn"),
        "Private function should be in confident list"
    );
    assert!(
        confident_names.contains(&"pub_fn"),
        "Public function should be in confident list when include_pub=true"
    );
    assert!(
        possibly_dead_pub.is_empty(),
        "possibly_dead_pub should be empty when include_pub=true"
    );
}

#[test]
fn test_find_dead_code_excludes_test_files() {
    let store = TestStore::new();

    // Create a chunk in a test file
    let mut chunk = test_chunk("test_helper", "fn test_helper() { /* test utility */ }");
    chunk.file = std::path::PathBuf::from("tests/helper.rs");
    chunk.id = format!("tests/helper.rs:1:{}", &chunk.content_hash[..8]);

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();

    // Find dead code
    let (confident, _) = store.find_dead_code(false).unwrap();

    // Functions in test files should not be flagged as dead
    let dead_names: Vec<&str> = confident.iter().map(|d| d.chunk.name.as_str()).collect();
    assert!(
        !dead_names.contains(&"test_helper"),
        "Functions in test files should be excluded from dead code detection"
    );
}
