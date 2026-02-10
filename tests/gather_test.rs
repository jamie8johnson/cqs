//! Gather integration tests (#3)
//!
//! Note: gather requires embeddings and call graph, so these are basic
//! integration tests that verify the function executes without crashing.

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::{CallSite, FunctionCalls};
use cqs::{GatherDirection, GatherOptions};
use std::path::PathBuf;

#[test]
fn test_gather_basic() {
    let store = TestStore::new();

    // Insert chunks with call graph
    let chunk_a = test_chunk("func_a", "fn func_a() { func_b(); }");
    let chunk_b = test_chunk("func_b", "fn func_b() { func_c(); }");
    let chunk_c = test_chunk("func_c", "fn func_c() {}");

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_a, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_b, &emb, Some(12345)).unwrap();
    store.upsert_chunk(&chunk_c, &emb, Some(12345)).unwrap();

    // Insert call edges using function_calls
    let function_calls = vec![
        FunctionCalls {
            name: "func_a".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "func_b".to_string(),
                line_number: 1,
            }],
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

    // Run gather with basic options
    let opts = GatherOptions {
        expand_depth: 1,
        direction: GatherDirection::Both,
        limit: 10,
        ..GatherOptions::default()
    };
    let query = mock_embedding(1.0);
    let result = cqs::gather(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
    );

    assert!(result.is_ok(), "Gather should execute without error");
    let gather_result = result.unwrap();

    // Verify gather results are well-formed when present
    for chunk in &gather_result.chunks {
        assert!(!chunk.name.is_empty(), "Gathered chunk should have a name");
        assert!(
            chunk.depth <= 1,
            "With expand_depth=1, max depth should be 1"
        );
    }
}

#[test]
fn test_gather_no_expansion() {
    let store = TestStore::new();

    let chunk = test_chunk("single_fn", "fn single_fn() {}");
    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk, &emb, Some(12345)).unwrap();

    // Gather with no expansion (depth=0)
    let opts = GatherOptions {
        expand_depth: 0,
        direction: GatherDirection::Both,
        limit: 10,
        ..GatherOptions::default()
    };
    let query = mock_embedding(1.0);
    let result = cqs::gather(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
    )
    .unwrap();

    // Should only return seed results (no expansion)
    // Depth should be 0 for all results
    for chunk in &result.chunks {
        assert_eq!(chunk.depth, 0, "No expansion means depth=0 for all chunks");
    }
    assert!(!result.expansion_capped);
}

#[test]
fn test_gather_callers_only() {
    let store = TestStore::new();

    let chunk_a = test_chunk("caller", "fn caller() { target(); }");
    let chunk_target = test_chunk("target", "fn target() {}");
    let chunk_callee = test_chunk("callee", "fn callee() {}");

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_a, &emb, Some(12345)).unwrap();
    store
        .upsert_chunk(&chunk_target, &emb, Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk_callee, &emb, Some(12345))
        .unwrap();

    // caller → target → callee
    let function_calls = vec![
        FunctionCalls {
            name: "caller".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "target".to_string(),
                line_number: 1,
            }],
        },
        FunctionCalls {
            name: "target".to_string(),
            line_start: 5,
            calls: vec![CallSite {
                callee_name: "callee".to_string(),
                line_number: 5,
            }],
        },
    ];
    store
        .upsert_function_calls(&PathBuf::from("test.rs"), &function_calls)
        .unwrap();

    // Gather with callers direction (should expand up the call graph)
    let opts = GatherOptions {
        expand_depth: 1,
        direction: GatherDirection::Callers,
        limit: 10,
        ..GatherOptions::default()
    };
    let query = mock_embedding(1.0);
    let result = cqs::gather(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
    );

    assert!(result.is_ok(), "Gather with callers direction should work");
}

#[test]
fn test_gather_callees_only() {
    let store = TestStore::new();

    let chunk_a = test_chunk("caller", "fn caller() { target(); }");
    let chunk_target = test_chunk("target", "fn target() {}");

    let emb = mock_embedding(1.0);
    store.upsert_chunk(&chunk_a, &emb, Some(12345)).unwrap();
    store
        .upsert_chunk(&chunk_target, &emb, Some(12345))
        .unwrap();

    let function_calls = vec![FunctionCalls {
        name: "caller".to_string(),
        line_start: 1,
        calls: vec![CallSite {
            callee_name: "target".to_string(),
            line_number: 1,
        }],
    }];
    store
        .upsert_function_calls(&PathBuf::from("test.rs"), &function_calls)
        .unwrap();

    // Gather with callees direction (should expand down the call graph)
    let opts = GatherOptions {
        expand_depth: 1,
        direction: GatherDirection::Callees,
        limit: 10,
        ..GatherOptions::default()
    };
    let query = mock_embedding(1.0);
    let result = cqs::gather(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
    );

    assert!(result.is_ok(), "Gather with callees direction should work");
}
