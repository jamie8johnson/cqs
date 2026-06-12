//! Call graph tests (T3, T17)
//!
//! Tests for upsert_calls, get_callers_full, get_callees, and call_stats.

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::{CallEdgeKind, CallSite};

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
            kind: CallEdgeKind::Call,
        },
        CallSite {
            callee_name: "bar".to_string(),
            line_number: 1,
            kind: CallEdgeKind::Call,
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
        kind: CallEdgeKind::Call,
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
            kind: CallEdgeKind::Call,
        },
        CallSite {
            callee_name: "baz".to_string(),
            line_number: 2,
            kind: CallEdgeKind::Call,
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
        kind: CallEdgeKind::Call,
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
                kind: CallEdgeKind::Call,
            }],
        },
        FunctionCalls {
            name: "fn2".to_string(),
            line_start: 10,
            calls: vec![CallSite {
                callee_name: "target".to_string(),
                line_number: 15,
                kind: CallEdgeKind::Call,
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

/// End-to-end: a serde string-callback (`#[serde(default = "fn")]`) must
/// surface through `cqs callers` of the callback function. Parses a real Rust
/// file with the extractor, upserts the resulting function_calls, then queries
/// callers — without the extractor-side edge this returns empty.
#[test]
fn test_get_callers_full_resolves_serde_callback() {
    use cqs::parser::Parser;
    use std::io::Write;

    let mut file = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("temp file");
    write!(
        file,
        r#"
#[derive(serde::Deserialize)]
struct RefWeightCfg {{
    #[serde(default = "default_ref_weight")]
    weight: f32,
}}

fn default_ref_weight() -> f32 {{ 1.0 }}
"#
    )
    .expect("write temp file");
    file.flush().expect("flush");

    let parser = Parser::new().expect("parser");
    let (calls, _types) = parser
        .parse_file_relationships(file.path())
        .expect("parse relationships");

    let store = TestStore::new();
    store
        .upsert_function_calls(std::path::Path::new("cfg.rs"), &calls)
        .unwrap();

    let callers = store.get_callers_full("default_ref_weight").unwrap();
    let names: Vec<_> = callers.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"RefWeightCfg"),
        "serde default callback must list the carrying struct as a caller, got: {names:?}"
    );
}

/// End-to-end (#1818 second half): a function passed as a bare fn-pointer
/// VALUE in argument position — the `touch_idle_clock` shape from
/// `src/serve/mod.rs` — resolves through the store-level `get_callers_full`.
/// Pre-fix this was a live `cqs dead` false positive: the only reference to
/// `touch_idle_clock` was as an argument to `from_fn_with_state`, invisible to
/// the call query.
#[test]
fn test_get_callers_full_resolves_fn_pointer_arg() {
    use cqs::parser::Parser;
    use std::io::Write;

    let mut file = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("temp file");
    write!(
        file,
        r#"
fn touch_state() {{}}
fn touch_idle_clock() {{}}

fn build_middleware() {{
    let state = make_state();
    from_fn_with_state(state, touch_state, touch_idle_clock);
}}
"#
    )
    .expect("write temp file");
    file.flush().expect("flush");

    let parser = Parser::new().expect("parser");
    let (calls, _types) = parser
        .parse_file_relationships(file.path())
        .expect("parse relationships");

    let store = TestStore::new();
    store
        .upsert_function_calls(std::path::Path::new("serve.rs"), &calls)
        .unwrap();

    let callers = store.get_callers_full("touch_idle_clock").unwrap();
    let names: Vec<_> = callers.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"build_middleware"),
        "fn-pointer arg must list the enclosing function as a caller, got: {names:?}"
    );
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
            kind: CallEdgeKind::Call,
        },
        CallSite {
            callee_name: "b".to_string(),
            line_number: 2,
            kind: CallEdgeKind::Call,
        },
        CallSite {
            callee_name: "c".to_string(),
            line_number: 3,
            kind: CallEdgeKind::Call,
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
                    kind: CallEdgeKind::Call,
                },
                CallSite {
                    callee_name: "bar".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::Call,
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
                    kind: CallEdgeKind::Call,
                },
                CallSite {
                    callee_name: "baz".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::Call,
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
    use cqs::parser::{CallEdgeKind, CallSite, FunctionCalls};

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
                    kind: CallEdgeKind::Call,
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
                    kind: CallEdgeKind::Call,
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
                    kind: CallEdgeKind::Call,
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
                    kind: CallEdgeKind::Call,
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

/// TC-HAP-V1.36-2 / P3: get_callers_with_context positive test. The
/// richer variant of get_callers_full carries call_line — used by
/// `cqs impact`. Coverage was indirect via tests/impact_test.rs only.
#[test]
fn test_get_callers_with_context_returns_call_line() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    let calls = vec![FunctionCalls {
        name: "caller_fn".to_string(),
        line_start: 10,
        calls: vec![CallSite {
            callee_name: "target".to_string(),
            line_number: 25,
            kind: CallEdgeKind::Call,
        }],
    }];
    store
        .upsert_function_calls(std::path::Path::new("a.rs"), &calls)
        .unwrap();

    let callers = store.get_callers_with_context("target").unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].name, "caller_fn");
    assert_eq!(callers[0].line, 10);
    assert_eq!(callers[0].call_line, 25);
}

/// TC-HAP-V1.36-3 / P3: pin _full_batch variants — both must return
/// empty Vec for unknown names (not missing keys), and matching entries
/// for known names.
#[test]
fn test_get_callers_full_batch_returns_per_name_results() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    let calls = vec![FunctionCalls {
        name: "caller".to_string(),
        line_start: 1,
        calls: vec![CallSite {
            callee_name: "real_target".to_string(),
            line_number: 2,
            kind: CallEdgeKind::Call,
        }],
    }];
    store
        .upsert_function_calls(std::path::Path::new("a.rs"), &calls)
        .unwrap();

    let result = store
        .get_callers_full_batch(&["real_target", "missing"])
        .unwrap();
    assert_eq!(result.get("real_target").map(|v| v.len()), Some(1));
    // Names with no callers don't appear in the map at all (current
    // contract). Pin it so a future change to "absent key → empty Vec"
    // is intentional, not silent. The audit suggested the "empty Vec"
    // shape but that's a contract change for callers.
    assert!(!result.contains_key("missing"));
}

// ===== edge provenance (§1) =====

/// Edge kind written at the source survives the function_calls round-trip and
/// surfaces on the CallerInfo / CalleeInfo results.
#[test]
fn edge_kind_round_trips_through_function_calls() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    let chunk = test_chunk("caller_fn", "fn caller_fn() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(1))
        .unwrap();

    store
        .upsert_function_calls(
            std::path::Path::new("src/x.rs"),
            &[FunctionCalls {
                name: "caller_fn".to_string(),
                line_start: 1,
                calls: vec![
                    CallSite {
                        callee_name: "syntactic_callee".to_string(),
                        line_number: 2,
                        kind: CallEdgeKind::Call,
                    },
                    CallSite {
                        callee_name: "macro_callee".to_string(),
                        line_number: 3,
                        kind: CallEdgeKind::MacroHeuristic,
                    },
                ],
            }],
        )
        .unwrap();

    // Caller side: the macro callee's caller carries the heuristic kind.
    let callers = store.get_callers_full("macro_callee").unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].edge_kind, CallEdgeKind::MacroHeuristic);

    let syn_callers = store.get_callers_full("syntactic_callee").unwrap();
    assert_eq!(syn_callers[0].edge_kind, CallEdgeKind::Call);

    // Callee side: get_callees_full carries the kind too.
    let callees = store.get_callees_full("caller_fn", None).unwrap();
    let macro_callee = callees.iter().find(|c| c.name == "macro_callee").unwrap();
    assert_eq!(macro_callee.edge_kind, CallEdgeKind::MacroHeuristic);
}

/// A callee reached only by heuristic edges is reported by
/// `find_low_confidence_live_names`; one with a syntactic (or serde) edge is
/// not. A doc-reference edge is inert: it neither qualifies nor disqualifies a
/// callee.
/// A doc-reference edge is inert: it neither qualifies nor disqualifies.
#[test]
fn low_confidence_live_names_finds_heuristic_only_callees() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    let chunk = test_chunk("caller_fn", "fn caller_fn() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(1))
        .unwrap();

    store
        .upsert_function_calls(
            std::path::Path::new("src/x.rs"),
            &[FunctionCalls {
                name: "caller_fn".to_string(),
                line_start: 1,
                calls: vec![
                    // heuristic-only callee
                    CallSite {
                        callee_name: "heuristic_only".to_string(),
                        line_number: 2,
                        kind: CallEdgeKind::FnPointer,
                    },
                    // has a syntactic edge → NOT low-confidence
                    CallSite {
                        callee_name: "has_syntactic".to_string(),
                        line_number: 3,
                        kind: CallEdgeKind::Call,
                    },
                    CallSite {
                        callee_name: "has_syntactic".to_string(),
                        line_number: 4,
                        kind: CallEdgeKind::MacroHeuristic,
                    },
                    // A doc reference + a macro edge → still low-confidence
                    // (the doc edge is inert, does not disqualify).
                    CallSite {
                        callee_name: "doc_plus_macro".to_string(),
                        line_number: 5,
                        kind: CallEdgeKind::DocReference,
                    },
                    CallSite {
                        callee_name: "doc_plus_macro".to_string(),
                        line_number: 6,
                        kind: CallEdgeKind::MacroHeuristic,
                    },
                    // A serde callback is trusted → NOT low-confidence even with
                    // a macro edge present.
                    CallSite {
                        callee_name: "serde_cb".to_string(),
                        line_number: 7,
                        kind: CallEdgeKind::SerdeCallback,
                    },
                    CallSite {
                        callee_name: "serde_cb".to_string(),
                        line_number: 8,
                        kind: CallEdgeKind::MacroHeuristic,
                    },
                ],
            }],
        )
        .unwrap();

    let names = store.find_low_confidence_live_names().unwrap();
    assert!(
        names.contains_key("heuristic_only"),
        "heuristic-only callee must be low-confidence-live: {names:?}"
    );
    assert!(
        !names.contains_key("has_syntactic"),
        "a callee with even one syntactic edge is not low-confidence-live: {names:?}"
    );
    assert!(
        names.contains_key("doc_plus_macro"),
        "a doc reference must not disqualify a heuristic-only callee (F5): {names:?}"
    );
    assert!(
        !names.contains_key("serde_cb"),
        "a serde callback is trusted, so it is not low-confidence-live: {names:?}"
    );
    // The reason carries per-kind counts.
    let info = &names["heuristic_only"];
    assert_eq!(info.total, 1);
    assert_eq!(info.kind_counts, vec![("fn_pointer".to_string(), 1)]);
}

/// Seam-audit Finding 1 regression. A function `cb` whose ONLY caller edge is a
/// macro-shape heuristic must NOT be reported by `find_dead_code` (the strict
/// zero-edge contract that `health`/`ci`/`suggest` consume) — it has an edge, so
/// it is not dead. It MUST instead surface in
/// `find_low_confidence_live_functions`, the additive `cqs dead`-only overlay
/// that `dead_core` relabels `low-confidence-live`. The two populations are
/// disjoint. Adding a real syntactic caller removes `cb` from both sets.
///
/// Fails before the fix: the dead-candidate query gated on absence of a TRUSTED
/// edge (not absence of ALL edges), so `cb` leaked into `find_dead_code` and the
/// three non-relabelling consumers reported live code as dead.
#[test]
fn heuristic_only_callee_is_low_conf_live_not_dead() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    // Define `cb` as a function chunk. Unique id/origin so it isn't a
    // windowed/parented chunk.
    let mut cb = test_chunk("cb", "fn cb() {}");
    cb.id = "src/app.rs:10:cb".to_string();
    cb.file = std::path::PathBuf::from("src/app.rs");
    store
        .upsert_chunk(&cb, &mock_embedding(1.0), Some(1))
        .unwrap();

    // A macro-shape-only caller edge to `cb` (no trusted edge).
    store
        .upsert_function_calls(
            std::path::Path::new("src/caller.rs"),
            &[FunctionCalls {
                name: "some_macro_site".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "cb".to_string(),
                    line_number: 2,
                    kind: CallEdgeKind::MacroHeuristic,
                }],
            }],
        )
        .unwrap();

    // find_dead_code (strict zero-edge): cb has an edge → NOT reported dead.
    // This is the failing-before assertion: health/ci/suggest call this method
    // directly and must never see cb.
    let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
    let in_dead = confident
        .iter()
        .chain(possibly_pub.iter())
        .any(|d| d.chunk.name == "cb");
    assert!(
        !in_dead,
        "cb (macro-only caller) must NOT be in find_dead_code — it has an edge, \
         so health/ci/suggest must not report it dead (Finding 1)"
    );

    // find_low_confidence_live_functions: cb surfaces here, the additive overlay.
    let (lc_conf, lc_pub) = store.find_low_confidence_live_functions(true).unwrap();
    let in_low_conf = lc_conf
        .iter()
        .chain(lc_pub.iter())
        .any(|d| d.chunk.name == "cb");
    assert!(
        in_low_conf,
        "cb (macro-only caller) must surface in find_low_confidence_live_functions \
         so dead_core can relabel it low-confidence-live (Finding 1)"
    );

    // Add a real syntactic caller → cb now has a trusted edge: out of BOTH sets.
    store
        .upsert_function_calls(
            std::path::Path::new("src/caller2.rs"),
            &[FunctionCalls {
                name: "real_caller".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "cb".to_string(),
                    line_number: 2,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();

    let (confident, possibly_pub) = store.find_dead_code(true).unwrap();
    let still_dead = confident
        .iter()
        .chain(possibly_pub.iter())
        .any(|d| d.chunk.name == "cb");
    assert!(
        !still_dead,
        "cb with a syntactic caller is genuinely live, not dead"
    );
    let (lc_conf, lc_pub) = store.find_low_confidence_live_functions(true).unwrap();
    let still_low_conf = lc_conf
        .iter()
        .chain(lc_pub.iter())
        .any(|d| d.chunk.name == "cb");
    assert!(
        !still_low_conf,
        "cb with a trusted edge has graduated out of the low-confidence-live overlay"
    );
}

/// A doc-mention-only caller renders with edge_kind "doc_reference".
#[test]
fn doc_reference_edge_kind_round_trips() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    let chunk = test_chunk("doc_caller", "fn doc_caller() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(1))
        .unwrap();

    store
        .upsert_function_calls(
            std::path::Path::new("README.md"),
            &[FunctionCalls {
                name: "doc_caller".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "mentioned_symbol".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::DocReference,
                }],
            }],
        )
        .unwrap();

    let callers = store.get_callers_full("mentioned_symbol").unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].edge_kind, CallEdgeKind::DocReference);
}

/// The MIN-collapse keeps the MOST-TRUSTED kind by explicit trust rank, not
/// lexical order. With all five kinds reaching the same callee from the same
/// (file, caller, line), the collapsed caller reports `call` (rank 0). A
/// separate group with only doc + serde reports `serde_callback` — proving the
/// rank (serde=1 < doc=4) beats the lexical order (`doc_reference` < `serde`).
#[test]
fn min_collapse_uses_trust_rank_not_lexical() {
    use cqs::parser::FunctionCalls;

    let store = TestStore::new();
    let chunk = test_chunk("multi", "fn multi() {}");
    store
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(1))
        .unwrap();

    let all_kinds = [
        CallEdgeKind::DocReference,
        CallEdgeKind::FnPointer,
        CallEdgeKind::MacroHeuristic,
        CallEdgeKind::SerdeCallback,
        CallEdgeKind::Call,
    ];
    // Group A: all five kinds at the SAME (file, caller, line) → collapse picks
    // `call`.
    let group_a: Vec<CallSite> = all_kinds
        .iter()
        .map(|&k| CallSite {
            callee_name: "callee_a".to_string(),
            line_number: 2,
            kind: k,
        })
        .collect();
    // Group B: doc + serde only, same (caller, line) → collapse picks serde
    // (rank 1) even though "doc_reference" sorts before "serde_callback".
    let group_b = vec![
        CallSite {
            callee_name: "callee_b".to_string(),
            line_number: 2,
            kind: CallEdgeKind::DocReference,
        },
        CallSite {
            callee_name: "callee_b".to_string(),
            line_number: 2,
            kind: CallEdgeKind::SerdeCallback,
        },
    ];

    store
        .upsert_function_calls(
            std::path::Path::new("src/x.rs"),
            &[FunctionCalls {
                name: "caller".to_string(),
                line_start: 1,
                calls: group_a.into_iter().chain(group_b).collect(),
            }],
        )
        .unwrap();

    let a = store.get_callers_full("callee_a").unwrap();
    assert_eq!(a.len(), 1, "five edges at one site collapse to one row");
    assert_eq!(
        a[0].edge_kind,
        CallEdgeKind::Call,
        "collapse must keep the most-trusted kind (call)"
    );

    let b = store.get_callers_full("callee_b").unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(
        b[0].edge_kind,
        CallEdgeKind::SerdeCallback,
        "serde (rank 1) beats doc_reference (rank 4) despite lexical order"
    );
}

// ===== trust-rank ordering + Type::method attribution =====

use cqs::parser::FunctionCalls;
use cqs::store::CallerAttribution;

/// Build a method-def chunk under an enclosing type, located at a given
/// origin/line so `get_callers_attributed`'s `(origin, name, line_start)` join
/// resolves it. `chunk_type` is Function (callable) so the candidate/owner
/// queries count it.
fn method_chunk(
    file: &str,
    name: &str,
    line: u32,
    parent_type: Option<&str>,
) -> cqs::parser::Chunk {
    use cqs::parser::{ChunkType, Language};
    let content = format!("fn {name}() {{}}");
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    cqs::parser::Chunk {
        id: format!("{file}:{line}:{}", &hash[..8]),
        file: std::path::PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {name}()"),
        content,
        doc: None,
        line_start: line,
        line_end: line + 1,
        content_hash: hash,
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: parent_type.map(String::from),
        parser_version: 0,
    }
}

/// A single doc_reference edge must never displace a direct call edge within a
/// limited window: trust rank leads the ORDER BY. The doc edge sits in
/// a file (`docs.md`) that sorts before the call edge's file (`src.rs`), so a
/// lexical `ORDER BY file` would surface it first — the regression this guards.
#[test]
fn doc_reference_never_displaces_call_in_window() {
    let store = TestStore::new();
    // doc_reference edge from a file that sorts FIRST lexically.
    store
        .upsert_function_calls(
            std::path::Path::new("docs.md"),
            &[FunctionCalls {
                name: "doc_mention".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "target".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::DocReference,
                }],
            }],
        )
        .unwrap();
    // Direct call edge from a file that sorts AFTER.
    store
        .upsert_function_calls(
            std::path::Path::new("src.rs"),
            &[FunctionCalls {
                name: "real_caller".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "target".to_string(),
                    line_number: 2,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();

    let callers = store.get_callers_full("target").unwrap();
    assert_eq!(callers.len(), 2);
    // Trust rank leads: the direct call edge is first despite its file sorting
    // last. A `--limit 1` window would therefore show the call edge, not the
    // doc reference.
    assert_eq!(callers[0].name, "real_caller");
    assert_eq!(callers[0].edge_kind, CallEdgeKind::Call);
    assert_eq!(callers[1].edge_kind, CallEdgeKind::DocReference);
}

/// Same property on the impact path (`get_callers_with_context`, no GROUP BY):
/// the call edge leads the doc edge regardless of file order.
#[test]
fn context_callers_trust_rank_leads() {
    let store = TestStore::new();
    store
        .upsert_function_calls(
            std::path::Path::new("aaa_docs.md"),
            &[FunctionCalls {
                name: "doc_mention".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "target".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::DocReference,
                }],
            }],
        )
        .unwrap();
    store
        .upsert_function_calls(
            std::path::Path::new("zzz_src.rs"),
            &[FunctionCalls {
                name: "real_caller".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "target".to_string(),
                    line_number: 2,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();

    let callers = store.get_callers_with_context("target").unwrap();
    assert_eq!(callers[0].edge_kind, CallEdgeKind::Call);
    assert_eq!(callers[0].name, "real_caller");
}

/// `Type::method` attribution: a caller whose own enclosing type IS the queried
/// type is a self-call (`SelfType`); a caller parented to a DIFFERENT type that
/// has its own same-named method is excluded; a free-function caller (no
/// enclosing type) is included but flagged `Ambiguous`.
#[test]
fn attributed_callers_pick_right_type() {
    let store = TestStore::new();

    // Two types each define `search`. Store::search lives in store.rs:10,
    // Index::search in index.rs:10.
    store
        .upsert_chunk(
            &method_chunk("store.rs", "search", 10, Some("Store")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();
    store
        .upsert_chunk(
            &method_chunk("index.rs", "search", 10, Some("Index")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();

    // Caller chunks: a Store method that self-calls search; an Index method that
    // calls its own search; a free function that calls search bare.
    store
        .upsert_chunk(
            &method_chunk("store.rs", "store_self", 20, Some("Store")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();
    store
        .upsert_chunk(
            &method_chunk("index.rs", "index_self", 20, Some("Index")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();
    store
        .upsert_chunk(
            &method_chunk("free.rs", "free_fn", 20, None),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();

    // Edges: each caller calls `search`.
    for (file, caller) in [
        ("store.rs", "store_self"),
        ("index.rs", "index_self"),
        ("free.rs", "free_fn"),
    ] {
        store
            .upsert_function_calls(
                std::path::Path::new(file),
                &[FunctionCalls {
                    name: caller.to_string(),
                    line_start: 20,
                    calls: vec![CallSite {
                        callee_name: "search".to_string(),
                        line_number: 21,
                        kind: CallEdgeKind::Call,
                    }],
                }],
            )
            .unwrap();
    }

    // other_owner_types for Store::search = {Index} (Index also owns `search`).
    let mut others = std::collections::HashSet::new();
    others.insert("Index".to_string());
    let attributed = store
        .get_callers_attributed("search", "Store", &others)
        .unwrap();
    let names: Vec<_> = attributed
        .iter()
        .map(|a| (a.caller.name.as_str(), a.attribution))
        .collect();
    // index_self is parented to Index (owns its own search) → excluded.
    assert!(
        !names.iter().any(|(n, _)| *n == "index_self"),
        "caller in a different type that owns the method must be excluded, got {names:?}"
    );
    // store_self → self-call.
    assert!(names.contains(&("store_self", CallerAttribution::SelfType)));
    // free_fn → no enclosing type → ambiguous, included.
    assert!(names.contains(&("free_fn", CallerAttribution::Ambiguous)));
}

/// `count_method_defs_by_type` groups a name's callable definitions by
/// enclosing type, powering the bare-name candidate list and the
/// other-owner-types exclusion set.
#[test]
fn count_method_defs_groups_by_type() {
    let store = TestStore::new();
    store
        .upsert_chunk(
            &method_chunk("store.rs", "search", 10, Some("Store")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();
    store
        .upsert_chunk(
            &method_chunk("index.rs", "search", 10, Some("Index")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();

    let defs = store.count_method_defs_by_type("search").unwrap();
    assert_eq!(defs.len(), 2, "two enclosing types define `search`");
    let types: std::collections::HashSet<_> = defs.iter().filter_map(|(t, _)| t.clone()).collect();
    assert!(types.contains("Store"));
    assert!(types.contains("Index"));
}

/// `get_type_method_origins` resolves the file(s) defining `Type::method`,
/// scoping the callees `Type::method` path.
#[test]
fn type_method_origins_resolve_def_file() {
    let store = TestStore::new();
    store
        .upsert_chunk(
            &method_chunk("store.rs", "search", 10, Some("Store")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();
    store
        .upsert_chunk(
            &method_chunk("index.rs", "search", 10, Some("Index")),
            &mock_embedding(1.0),
            Some(1),
        )
        .unwrap();

    let origins = store.get_type_method_origins("Store", "search").unwrap();
    assert_eq!(origins, vec!["store.rs".to_string()]);
}
