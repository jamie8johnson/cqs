//! Call graph tests (T3, T17)
//!
//! Tests for upsert_calls, get_callers, get_callees, and call_stats.

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::CallSite;

// ===== upsert_calls tests =====

#[test]
fn test_upsert_calls_batch_insert() {
    let store = TestStore::new();

    // Insert a chunk first
    let chunk = test_chunk("caller_fn", "fn caller_fn() { foo(); bar(); }");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Insert calls for the chunk
    let calls = vec![
        CallSite {
            callee_name: "foo".to_string(),
            line_number: 1,
        },
        CallSite {
            callee_name: "bar".to_string(),
            line_number: 1,
        },
    ];
    store.upsert_calls(&chunk.id, &calls).unwrap();

    // Verify calls were inserted
    let callees = store.get_callees(&chunk.id).unwrap();
    assert_eq!(callees.len(), 2);
    assert!(callees.contains(&"foo".to_string()));
    assert!(callees.contains(&"bar".to_string()));
}

#[test]
fn test_upsert_calls_replace() {
    let store = TestStore::new();

    let chunk = test_chunk("caller_fn", "fn caller_fn() { foo(); }");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Insert initial calls
    let calls1 = vec![CallSite {
        callee_name: "foo".to_string(),
        line_number: 1,
    }];
    store.upsert_calls(&chunk.id, &calls1).unwrap();

    // Verify initial state
    let callees = store.get_callees(&chunk.id).unwrap();
    assert_eq!(callees, vec!["foo"]);

    // Replace with new calls
    let calls2 = vec![
        CallSite {
            callee_name: "bar".to_string(),
            line_number: 1,
        },
        CallSite {
            callee_name: "baz".to_string(),
            line_number: 2,
        },
    ];
    store.upsert_calls(&chunk.id, &calls2).unwrap();

    // Verify replacement (foo should be gone, bar and baz present)
    let callees = store.get_callees(&chunk.id).unwrap();
    assert_eq!(callees.len(), 2);
    assert!(!callees.contains(&"foo".to_string()));
    assert!(callees.contains(&"bar".to_string()));
    assert!(callees.contains(&"baz".to_string()));
}

#[test]
fn test_upsert_calls_empty() {
    let store = TestStore::new();

    let chunk = test_chunk("caller_fn", "fn caller_fn() { foo(); }");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Insert some calls first
    let calls = vec![CallSite {
        callee_name: "foo".to_string(),
        line_number: 1,
    }];
    store.upsert_calls(&chunk.id, &calls).unwrap();

    // Upsert with empty list should clear calls
    store.upsert_calls(&chunk.id, &[]).unwrap();

    let callees = store.get_callees(&chunk.id).unwrap();
    assert!(
        callees.is_empty(),
        "Empty upsert should clear existing calls"
    );
}

// ===== get_callers tests =====

#[test]
fn test_get_callers_found() {
    let store = TestStore::new();

    // Create two chunks that call the same function
    let chunk1 = test_chunk("fn1", "fn fn1() { target(); }");
    let mut chunk2 = test_chunk("fn2", "fn fn2() { target(); }");
    chunk2.id = format!("test.rs:10:{}", &chunk2.content_hash[..8]);

    store
        .upsert_chunk(&chunk1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Both call "target"
    store
        .upsert_calls(
            &chunk1.id,
            &[CallSite {
                callee_name: "target".to_string(),
                line_number: 1,
            }],
        )
        .unwrap();
    store
        .upsert_calls(
            &chunk2.id,
            &[CallSite {
                callee_name: "target".to_string(),
                line_number: 1,
            }],
        )
        .unwrap();

    // Get callers of "target"
    let callers = store.get_callers("target").unwrap();
    assert_eq!(callers.len(), 2);

    let caller_names: Vec<_> = callers.iter().map(|c| c.name.as_str()).collect();
    assert!(caller_names.contains(&"fn1"));
    assert!(caller_names.contains(&"fn2"));
}

#[test]
fn test_get_callers_not_found() {
    let store = TestStore::new();

    // No calls inserted
    let callers = store.get_callers("nonexistent").unwrap();
    assert!(callers.is_empty());
}

#[test]
fn test_get_callers_empty_string() {
    let store = TestStore::new();

    // Edge case: empty callee name
    let callers = store.get_callers("").unwrap();
    assert!(callers.is_empty());
}

// ===== get_callees tests =====

#[test]
fn test_get_callees_found() {
    let store = TestStore::new();

    let chunk = test_chunk("caller", "fn caller() { a(); b(); c(); }");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    let calls = vec![
        CallSite {
            callee_name: "a".to_string(),
            line_number: 1,
        },
        CallSite {
            callee_name: "b".to_string(),
            line_number: 2,
        },
        CallSite {
            callee_name: "c".to_string(),
            line_number: 3,
        },
    ];
    store.upsert_calls(&chunk.id, &calls).unwrap();

    let callees = store.get_callees(&chunk.id).unwrap();
    assert_eq!(callees.len(), 3);
    // Should be ordered by line_number
    assert_eq!(callees, vec!["a", "b", "c"]);
}

#[test]
fn test_get_callees_not_found() {
    let store = TestStore::new();

    // Non-existent chunk
    let callees = store.get_callees("nonexistent_chunk_id").unwrap();
    assert!(callees.is_empty());
}

// ===== call_stats tests =====

#[test]
fn test_call_stats_empty() {
    let store = TestStore::new();

    let stats = store.call_stats().unwrap();
    assert_eq!(stats.total_calls, 0);
    assert_eq!(stats.unique_callees, 0);
}

#[test]
fn test_call_stats_populated() {
    let store = TestStore::new();

    let chunk1 = test_chunk("fn1", "fn fn1() { foo(); bar(); }");
    let mut chunk2 = test_chunk("fn2", "fn fn2() { foo(); baz(); }");
    chunk2.id = format!("test.rs:10:{}", &chunk2.content_hash[..8]);

    store
        .upsert_chunk(&chunk1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk2, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // fn1 calls foo, bar
    store
        .upsert_calls(
            &chunk1.id,
            &[
                CallSite {
                    callee_name: "foo".to_string(),
                    line_number: 1,
                },
                CallSite {
                    callee_name: "bar".to_string(),
                    line_number: 1,
                },
            ],
        )
        .unwrap();

    // fn2 calls foo, baz (foo is duplicated across chunks)
    store
        .upsert_calls(
            &chunk2.id,
            &[
                CallSite {
                    callee_name: "foo".to_string(),
                    line_number: 1,
                },
                CallSite {
                    callee_name: "baz".to_string(),
                    line_number: 1,
                },
            ],
        )
        .unwrap();

    let stats = store.call_stats().unwrap();
    assert_eq!(stats.total_calls, 4, "Total calls: foo, bar, foo, baz");
    assert_eq!(stats.unique_callees, 3, "Unique callees: foo, bar, baz");
}
