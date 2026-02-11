//! Proactive hints tests (compute_hints)

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::compute_hints;
use cqs::parser::{CallSite, FunctionCalls};
use std::path::Path;

/// Helper: insert a chunk and its embedding
fn insert_chunk(store: &TestStore, name: &str, content: &str) {
    let chunk = test_chunk(name, content);
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();
}

/// Helper: insert function call graph entries
fn insert_calls(store: &TestStore, file: &str, calls: &[(&str, u32, &[(&str, u32)])]) {
    let fc: Vec<FunctionCalls> = calls
        .iter()
        .map(|(name, line, callees)| FunctionCalls {
            name: name.to_string(),
            line_start: *line,
            calls: callees
                .iter()
                .map(|(callee, cline)| CallSite {
                    callee_name: callee.to_string(),
                    line_number: *cline,
                })
                .collect(),
        })
        .collect();
    store.upsert_function_calls(Path::new(file), &fc).unwrap();
}

#[test]
fn test_compute_hints_with_callers_and_tests() {
    let store = TestStore::new();

    // target_fn: the function under test
    insert_chunk(&store, "target_fn", "fn target_fn() { stuff() }");
    // caller_fn calls target_fn
    insert_chunk(&store, "caller_fn", "fn caller_fn() { target_fn() }");
    // test_target calls target_fn
    insert_chunk(
        &store,
        "test_target",
        "#[test] fn test_target() { target_fn() }",
    );

    insert_calls(
        &store,
        "test.rs",
        &[
            ("caller_fn", 1, &[("target_fn", 2)]),
            ("test_target", 1, &[("target_fn", 2)]),
        ],
    );

    let hints = compute_hints(&store, "target_fn", None).unwrap();
    assert!(hints.caller_count >= 1, "Should have at least 1 caller");
    assert!(hints.test_count >= 1, "Should have at least 1 test");
}

#[test]
fn test_compute_hints_no_callers() {
    let store = TestStore::new();

    insert_chunk(&store, "lonely_fn", "fn lonely_fn() {}");
    // No call graph entries → 0 callers

    let hints = compute_hints(&store, "lonely_fn", None).unwrap();
    assert_eq!(hints.caller_count, 0);
}

#[test]
fn test_compute_hints_no_tests() {
    let store = TestStore::new();

    insert_chunk(&store, "untested_fn", "fn untested_fn() {}");
    insert_chunk(&store, "app_caller", "fn app_caller() { untested_fn() }");

    insert_calls(
        &store,
        "test.rs",
        &[("app_caller", 1, &[("untested_fn", 2)])],
    );

    let hints = compute_hints(&store, "untested_fn", None).unwrap();
    // Has a caller but no test functions in the call chain
    assert!(hints.caller_count >= 1);
    assert_eq!(hints.test_count, 0);
}

#[test]
fn test_compute_hints_prefetched_callers() {
    let store = TestStore::new();

    insert_chunk(&store, "some_fn", "fn some_fn() {}");

    // Pass prefetched caller count — should skip the query
    let hints = compute_hints(&store, "some_fn", Some(42)).unwrap();
    assert_eq!(hints.caller_count, 42, "Should use prefetched value");
}

#[test]
fn test_compute_hints_empty_call_graph() {
    let store = TestStore::new();

    // No chunks, no calls — fresh index
    let hints = compute_hints(&store, "nonexistent", None).unwrap();
    assert_eq!(hints.caller_count, 0);
    assert_eq!(hints.test_count, 0);
}
