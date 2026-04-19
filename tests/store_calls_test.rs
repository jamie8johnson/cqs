//! Call graph tests (T3, T17)
//!
//! Tests for upsert_calls, get_callers_full, get_callees, and call_stats.

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

// ===== get_callers_full tests =====

#[test]
fn test_get_callers_full_found() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();

    // Insert function-level calls (the full call graph)
    let calls = vec![
        FunctionCalls {
            name: "fn1".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "target".to_string(),
                line_number: 5,
            }],
        },
        FunctionCalls {
            name: "fn2".to_string(),
            line_start: 10,
            calls: vec![CallSite {
                callee_name: "target".to_string(),
                line_number: 15,
            }],
        },
    ];
    store
        .upsert_function_calls(std::path::Path::new("test.rs"), &calls)
        .unwrap();

    // Get callers of "target"
    let callers = store.get_callers_full("target").unwrap();
    assert_eq!(callers.len(), 2);

    let caller_names: Vec<_> = callers.iter().map(|c| c.name.as_str()).collect();
    assert!(caller_names.contains(&"fn1"));
    assert!(caller_names.contains(&"fn2"));
}

#[test]
fn test_get_callers_full_not_found() {
    let store = TestStore::new();

    // No calls inserted
    let callers = store.get_callers_full("nonexistent").unwrap();
    assert!(callers.is_empty());
}

#[test]
fn test_get_callers_full_empty_string() {
    let store = TestStore::new();

    // Edge case: empty callee name
    let callers = store.get_callers_full("").unwrap();
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

// ===== E.2 (P1 #17): function_calls cleanup on incremental delete paths =====
//
// `function_calls` has no FK to `chunks` (it stores `caller_name` strings, not
// chunk IDs), so deleting chunks does NOT cascade. Three incremental delete
// paths used to leave orphan call-graph rows that surfaced as ghost callers
// in `cqs callers`/`callees`/`dead`. These tests pin the cleanup contract.

/// Helper: count `function_calls` rows for a given file via `get_callers_full`.
/// Distinct callee name per (file, callee) pair makes this an exact rowcount,
/// not just a "are there any callers" check.
fn count_function_calls_for_file(store: &cqs::store::Store, file: &str, callee: &str) -> usize {
    store
        .get_callers_full(callee)
        .unwrap()
        .into_iter()
        .filter(|c| c.file.to_string_lossy() == file)
        .count()
}

#[test]
fn delete_by_origin_purges_function_calls() {
    use cqs::parser::{CallSite, FunctionCalls};

    let store = TestStore::new();

    // Insert a chunk + a function_calls row referencing src/foo.rs.
    let chunk = test_chunk("foo_caller", "fn foo_caller() { foo_target(); }");
    let mut chunk = chunk;
    chunk.id = "src/foo.rs:1:abc".to_string();
    chunk.file = std::path::PathBuf::from("src/foo.rs");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    store
        .upsert_function_calls(
            std::path::Path::new("src/foo.rs"),
            &[FunctionCalls {
                name: "foo_caller".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "delete_by_origin_target_unique".to_string(),
                    line_number: 2,
                }],
            }],
        )
        .unwrap();

    assert_eq!(
        count_function_calls_for_file(&store, "src/foo.rs", "delete_by_origin_target_unique"),
        1,
        "precondition: function_calls row should be present"
    );

    // delete_by_origin must purge function_calls, not just chunks.
    store
        .delete_by_origin(std::path::Path::new("src/foo.rs"))
        .unwrap();

    assert_eq!(
        count_function_calls_for_file(&store, "src/foo.rs", "delete_by_origin_target_unique"),
        0,
        "function_calls rows should be purged after delete_by_origin"
    );
}

#[test]
fn prune_missing_purges_function_calls_for_removed_files() {
    use cqs::parser::{CallSite, FunctionCalls};
    use std::collections::HashSet;

    let store = TestStore::new();
    let dir = tempfile::tempdir().unwrap();

    // Create two real files on disk so chunks point to legitimate paths.
    let kept_path = dir.path().join("src/keep.rs");
    let gone_path = dir.path().join("src/gone.rs");
    std::fs::create_dir_all(kept_path.parent().unwrap()).unwrap();
    std::fs::write(&kept_path, "fn keep() {}").unwrap();
    std::fs::write(&gone_path, "fn gone() {}").unwrap();

    // Insert chunks for both files.
    let mut keep_chunk = test_chunk("keep", "fn keep() {}");
    keep_chunk.id = format!("{}:1:abc", kept_path.display());
    keep_chunk.file = kept_path.clone();
    let mut gone_chunk = test_chunk("gone", "fn gone() {}");
    gone_chunk.id = format!("{}:1:def", gone_path.display());
    gone_chunk.file = gone_path.clone();
    store
        .upsert_chunk(&keep_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&gone_chunk, &mock_embedding(2.0), Some(12345))
        .unwrap();

    // Insert function_calls for both files. Distinct callee per file so we
    // can verify each independently.
    store
        .upsert_function_calls(
            &kept_path,
            &[FunctionCalls {
                name: "keep".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "prune_keeper_target_unique".to_string(),
                    line_number: 2,
                }],
            }],
        )
        .unwrap();
    store
        .upsert_function_calls(
            &gone_path,
            &[FunctionCalls {
                name: "gone".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "prune_victim_target_unique".to_string(),
                    line_number: 2,
                }],
            }],
        )
        .unwrap();

    let kept_origin_str = kept_path.to_string_lossy().to_string();
    let gone_origin_str = gone_path.to_string_lossy().to_string();
    assert_eq!(
        count_function_calls_for_file(&store, &kept_origin_str, "prune_keeper_target_unique"),
        1
    );
    assert_eq!(
        count_function_calls_for_file(&store, &gone_origin_str, "prune_victim_target_unique"),
        1
    );

    // Delete the gone file from disk; existing_files contains only kept.
    std::fs::remove_file(&gone_path).unwrap();
    let mut existing = HashSet::new();
    existing.insert(kept_path.clone());

    store.prune_missing(&existing, dir.path()).unwrap();

    // Victim's function_calls rows must be purged.
    assert_eq!(
        count_function_calls_for_file(&store, &gone_origin_str, "prune_victim_target_unique"),
        0,
        "function_calls rows for the removed file must be purged"
    );
    // Keeper's function_calls rows must survive.
    assert_eq!(
        count_function_calls_for_file(&store, &kept_origin_str, "prune_keeper_target_unique"),
        1,
        "function_calls rows for the kept file must NOT be touched"
    );
}

#[test]
fn delete_phantom_chunks_does_not_touch_function_calls() {
    // `delete_phantom_chunks` is invoked from `cli/watch.rs:2456` AFTER
    // `upsert_function_calls` has already DELETE-then-INSERTed the file's
    // function_calls rows. Adding a `function_calls` DELETE here would wipe
    // those just-written rows. The two terminal-cleanup paths
    // (`delete_by_origin`, `prune_missing`) carry their own `function_calls`
    // DELETE because no upsert follows them. This test pins the per-method
    // contract: phantom-chunk cleanup leaves `function_calls` intact.
    use cqs::parser::{CallSite, FunctionCalls};

    let store = TestStore::new();

    let mut c1 = test_chunk("a", "fn a() {}");
    c1.id = "phantom_test.rs:1:aaa".to_string();
    c1.file = std::path::PathBuf::from("phantom_test.rs");
    let mut c2 = test_chunk("b", "fn b() {}");
    c2.id = "phantom_test.rs:5:bbb".to_string();
    c2.file = std::path::PathBuf::from("phantom_test.rs");
    store
        .upsert_chunk(&c1, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&c2, &mock_embedding(2.0), Some(12345))
        .unwrap();

    store
        .upsert_function_calls(
            std::path::Path::new("phantom_test.rs"),
            &[FunctionCalls {
                name: "a".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "phantom_target_unique".to_string(),
                    line_number: 2,
                }],
            }],
        )
        .unwrap();

    assert_eq!(
        count_function_calls_for_file(&store, "phantom_test.rs", "phantom_target_unique"),
        1,
        "precondition: function_calls row should be present"
    );

    let live = vec![c1.id.as_str()];
    store
        .delete_phantom_chunks(std::path::Path::new("phantom_test.rs"), &live)
        .unwrap();

    assert_eq!(
        count_function_calls_for_file(&store, "phantom_test.rs", "phantom_target_unique"),
        1,
        "delete_phantom_chunks must NOT touch function_calls — that's the watch \
         flow's upsert_function_calls's job, see crud.rs comment in delete_phantom_chunks"
    );
}
