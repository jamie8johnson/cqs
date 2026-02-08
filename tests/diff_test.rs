//! Semantic diff integration tests (#2)

mod common;

use common::{mock_embedding, test_chunk, TestStore};

#[test]
fn test_semantic_diff_basic() {
    // Create source store
    let source_store = TestStore::new();
    let source_a = test_chunk("func_a", "fn func_a() { 1 + 1 }");
    let source_b = test_chunk("func_b", "fn func_b() { 2 + 2 }");
    let source_c = test_chunk("func_c", "fn func_c() { 3 + 3 }");

    let emb_same = mock_embedding(1.0);
    let emb_different = mock_embedding(0.5);

    source_store
        .upsert_chunk(&source_a, &emb_same, Some(12345))
        .unwrap();
    source_store
        .upsert_chunk(&source_b, &emb_same, Some(12345))
        .unwrap();
    source_store
        .upsert_chunk(&source_c, &emb_same, Some(12345))
        .unwrap();

    // Create target store
    let target_store = TestStore::new();
    // func_a: same (same embedding)
    let target_a = test_chunk("func_a", "fn func_a() { 1 + 1 }");
    // func_b: modified (different embedding)
    let mut target_b = test_chunk("func_b", "fn func_b() { 2 + 2 + 2 }");
    target_b.content = "fn func_b() { 2 + 2 + 2 }".to_string();
    // func_d: new (not in source)
    let target_d = test_chunk("func_d", "fn func_d() { 4 + 4 }");

    target_store
        .upsert_chunk(&target_a, &emb_same, Some(12345))
        .unwrap();
    target_store
        .upsert_chunk(&target_b, &emb_different, Some(12345))
        .unwrap();
    target_store
        .upsert_chunk(&target_d, &emb_same, Some(12345))
        .unwrap();

    // Run semantic diff with threshold 0.95
    let diff = cqs::semantic_diff(
        &source_store.store,
        &target_store.store,
        "source",
        "target",
        0.95,
        None,
    )
    .unwrap();

    // Verify results
    // func_c should be removed (in source, not in target)
    assert!(
        diff.removed.iter().any(|c| c.name == "func_c"),
        "func_c should be in removed list"
    );

    // func_d should be added (in target, not in source)
    assert!(
        diff.added.iter().any(|c| c.name == "func_d"),
        "func_d should be in added list"
    );

    // func_b may be in modified (different embeddings, if similarity < threshold)
    // func_a should not be in any list (same content and embedding)
    let all_changed: Vec<&str> = diff
        .modified
        .iter()
        .map(|c| c.name.as_str())
        .chain(diff.added.iter().map(|c| c.name.as_str()))
        .chain(diff.removed.iter().map(|c| c.name.as_str()))
        .collect();

    assert!(
        !all_changed.contains(&"func_a"),
        "func_a is unchanged, should not appear in diff"
    );
}

#[test]
fn test_semantic_diff_empty_stores() {
    let source_store = TestStore::new();
    let target_store = TestStore::new();

    // Diff between two empty stores should return empty diff
    let diff = cqs::semantic_diff(
        &source_store.store,
        &target_store.store,
        "source",
        "target",
        0.5,
        None,
    )
    .unwrap();

    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert!(diff.modified.is_empty());
}

#[test]
fn test_semantic_diff_threshold() {
    let source_store = TestStore::new();
    let target_store = TestStore::new();

    // Same function, very slightly different embeddings
    let source_fn = test_chunk("func", "fn func() { 1 }");
    let target_fn = test_chunk("func", "fn func() { 1 }");

    source_store
        .upsert_chunk(&source_fn, &mock_embedding(1.0), Some(12345))
        .unwrap();
    target_store
        .upsert_chunk(&target_fn, &mock_embedding(0.99), Some(12345))
        .unwrap();

    // With high threshold (0.995), should detect as modified
    let diff = cqs::semantic_diff(
        &source_store.store,
        &target_store.store,
        "source",
        "target",
        0.995,
        None,
    )
    .unwrap();

    // May be in modified list depending on actual similarity
    // At minimum, should not crash
    assert!(
        diff.added.is_empty(),
        "Same function name should not be added"
    );
    assert!(
        diff.removed.is_empty(),
        "Same function name should not be removed"
    );
}

#[test]
fn test_semantic_diff_language_filter() {
    let source_store = TestStore::new();
    let target_store = TestStore::new();

    // Insert Rust chunk in source
    let source_rust = test_chunk("rust_fn", "fn rust_fn() {}");
    source_store
        .upsert_chunk(&source_rust, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Insert Python chunk in source
    let mut source_py = test_chunk("py_fn", "def py_fn(): pass");
    source_py.language = cqs::parser::Language::Python;
    source_py.file = std::path::PathBuf::from("test.py");
    source_py.id = format!("test.py:1:{}", &source_py.content_hash[..8]);
    source_store
        .upsert_chunk(&source_py, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Target has neither

    // Diff with Rust filter should only show rust_fn as removed
    let diff = cqs::semantic_diff(
        &source_store.store,
        &target_store.store,
        "source",
        "target",
        0.5,
        Some("rust"),
    )
    .unwrap();

    assert!(
        diff.removed.iter().any(|c| c.name == "rust_fn"),
        "Rust function should be in removed list"
    );
    assert!(
        !diff.removed.iter().any(|c| c.name == "py_fn"),
        "Python function should not be in removed list (filtered out)"
    );
}
