//! Gather integration tests (#3)
//!
//! Note: gather requires embeddings and call graph, so these are basic
//! integration tests that verify the function executes without crashing.

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::{CallEdgeKind, CallSite, ChunkType, FunctionCalls, Language};
use cqs::reference::ReferenceIndex;
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
                kind: CallEdgeKind::Call,
            }],
        },
        FunctionCalls {
            name: "func_b".to_string(),
            line_start: 5,
            calls: vec![CallSite {
                callee_name: "func_c".to_string(),
                line_number: 5,
                kind: CallEdgeKind::Call,
            }],
        },
    ];
    store
        .upsert_function_calls(&PathBuf::from("test.rs"), &function_calls)
        .unwrap();

    // Run gather with basic options
    let query = mock_embedding(1.0);
    let opts = GatherOptions {
        expand_depth: 1,
        direction: GatherDirection::Both,
        limit: 10,
        ..GatherOptions::default()
    };
    let graph = store.store.get_call_graph().unwrap();
    let result = cqs::gather_with_graph(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        &graph,
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
        assert_eq!(
            chunk.language,
            Language::Rust,
            "Gathered chunk should have language"
        );
        assert_eq!(
            chunk.chunk_type,
            ChunkType::Function,
            "Gathered chunk should have chunk_type"
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
    let graph = store.store.get_call_graph().unwrap();
    let result = cqs::gather_with_graph(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        &graph,
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

    // Only `target` shares the query direction (seed 1.0). `caller` and
    // `callee` sit on the opposite direction (seed -1.0) so they fall below
    // the seed threshold — they can only enter the result via graph expansion,
    // which makes the direction assertion meaningful (otherwise all three
    // would be seeds at depth 0 and direction would be untestable).
    store
        .upsert_chunk(&chunk_a, &mock_embedding(-1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk_target, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk_callee, &mock_embedding(-1.0), Some(12345))
        .unwrap();

    // caller → target → callee
    let function_calls = vec![
        FunctionCalls {
            name: "caller".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "target".to_string(),
                line_number: 1,
                kind: CallEdgeKind::Call,
            }],
        },
        FunctionCalls {
            name: "target".to_string(),
            line_start: 5,
            calls: vec![CallSite {
                callee_name: "callee".to_string(),
                line_number: 5,
                kind: CallEdgeKind::Call,
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
    let graph = store.store.get_call_graph().unwrap();
    let result = cqs::gather_with_graph(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        &graph,
    )
    .expect("Gather with callers direction should work");

    // Names brought in by graph expansion (depth ≥ 1).
    let expanded: Vec<&str> = result
        .chunks
        .iter()
        .filter(|c| c.depth >= 1)
        .map(|c| c.name.as_str())
        .collect();

    assert!(
        expanded.contains(&"caller"),
        "Callers direction should expand up to `caller`, got expanded {expanded:?}"
    );
    assert!(
        !expanded.contains(&"callee"),
        "Callers direction must not expand down to `callee`, got expanded {expanded:?}"
    );
}

#[test]
fn test_gather_callees_only() {
    let store = TestStore::new();

    let chunk_a = test_chunk("caller", "fn caller() { target(); }");
    let chunk_target = test_chunk("target", "fn target() { callee(); }");
    let chunk_callee = test_chunk("callee", "fn callee() {}");

    // Mirror of the callers test: only `target` is a seed (direction 1.0);
    // `caller` (upstream) and `callee` (downstream) sit on the opposite
    // direction so they only enter via graph expansion.
    store
        .upsert_chunk(&chunk_a, &mock_embedding(-1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk_target, &mock_embedding(1.0), Some(12345))
        .unwrap();
    store
        .upsert_chunk(&chunk_callee, &mock_embedding(-1.0), Some(12345))
        .unwrap();

    // caller → target → callee
    let function_calls = vec![
        FunctionCalls {
            name: "caller".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "target".to_string(),
                line_number: 1,
                kind: CallEdgeKind::Call,
            }],
        },
        FunctionCalls {
            name: "target".to_string(),
            line_start: 5,
            calls: vec![CallSite {
                callee_name: "callee".to_string(),
                line_number: 5,
                kind: CallEdgeKind::Call,
            }],
        },
    ];
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
    let graph = store.store.get_call_graph().unwrap();
    let result = cqs::gather_with_graph(
        &store.store,
        &query,
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        &graph,
    )
    .expect("Gather with callees direction should work");

    // Names brought in by graph expansion (depth ≥ 1).
    let expanded: Vec<&str> = result
        .chunks
        .iter()
        .filter(|c| c.depth >= 1)
        .map(|c| c.name.as_str())
        .collect();

    assert!(
        expanded.contains(&"callee"),
        "Callees direction should expand down to `callee`, got expanded {expanded:?}"
    );
    assert!(
        !expanded.contains(&"caller"),
        "Callees direction must not expand up to `caller`, got expanded {expanded:?}"
    );
}

// ============ Cross-index gather tests (#414) ============

/// Helper: build a ReferenceIndex from a TestStore
fn make_ref_index(store: &TestStore, name: &str) -> ReferenceIndex {
    let ref_store = cqs::Store::open_readonly(&store.db_path()).unwrap();
    ReferenceIndex::new_loaded(
        name.to_string(),
        ref_store,
        None,
        1.0,
        std::path::PathBuf::new(),
    )
}

#[test]
fn test_gather_cross_index_basic() {
    // Reference store: has "ref_func" with embedding seed 1.0
    let ref_ts = TestStore::new();
    let ref_chunk = test_chunk("ref_func", "fn ref_func() { does_stuff(); }");
    ref_ts
        .upsert_chunk(&ref_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Project store: has "proj_func" with same embedding seed (so bridge search finds it)
    // and "proj_callee" connected via call graph
    let proj_ts = TestStore::new();
    let proj_chunk = test_chunk("proj_func", "fn proj_func() { proj_callee(); }");
    let proj_callee = test_chunk("proj_callee", "fn proj_callee() {}");
    proj_ts
        .upsert_chunk(&proj_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();
    proj_ts
        .upsert_chunk(&proj_callee, &mock_embedding(2.0), Some(12345))
        .unwrap();

    // Call edge: proj_func → proj_callee
    proj_ts
        .upsert_function_calls(
            &PathBuf::from("test.rs"),
            &[FunctionCalls {
                name: "proj_func".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "proj_callee".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();

    let ref_idx = make_ref_index(&ref_ts, "test-ref");
    let opts = GatherOptions {
        expand_depth: 1,
        direction: GatherDirection::Both,
        limit: 20,
        ..GatherOptions::default()
    };

    let result = cqs::gather_cross_index_with_index(
        &proj_ts.store,
        &ref_idx,
        &mock_embedding(1.0),
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        None,
    )
    .unwrap();

    // Should have at least the ref seed chunk
    assert!(!result.chunks.is_empty(), "Should return chunks");

    // Verify source tags
    let ref_chunks: Vec<_> = result
        .chunks
        .iter()
        .filter(|c| c.source.is_some())
        .collect();
    let proj_chunks: Vec<_> = result
        .chunks
        .iter()
        .filter(|c| c.source.is_none())
        .collect();

    assert!(
        !ref_chunks.is_empty(),
        "Should have reference-sourced chunks"
    );
    // Ref chunks should be tagged with our ref name
    for c in &ref_chunks {
        assert_eq!(c.source.as_deref(), Some("test-ref"));
    }
    // Project chunks (if any) should have source: None
    for c in &proj_chunks {
        assert!(c.source.is_none());
    }
}

#[test]
fn test_gather_cross_index_no_ref_seeds() {
    // Reference store: empty
    let ref_ts = TestStore::new();

    // Project store: has data
    let proj_ts = TestStore::new();
    let chunk = test_chunk("proj_func", "fn proj_func() {}");
    proj_ts
        .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    let ref_idx = make_ref_index(&ref_ts, "empty-ref");
    let opts = GatherOptions::default();

    let result = cqs::gather_cross_index_with_index(
        &proj_ts.store,
        &ref_idx,
        &mock_embedding(1.0),
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        None,
    )
    .unwrap();

    assert!(
        result.chunks.is_empty(),
        "Empty reference should yield empty result"
    );
}

#[test]
fn test_gather_cross_index_ref_only() {
    // Reference store: has chunks with seed 1.0
    let ref_ts = TestStore::new();
    let ref_chunk = test_chunk("ref_func", "fn ref_func() {}");
    ref_ts
        .upsert_chunk(&ref_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Project store: has chunks with orthogonal embedding so bridge won't match.
    // mock_embedding normalizes to unit vector, so all positive seeds produce
    // nearly identical direction. Use a negative seed to get an opposing direction.
    let proj_ts = TestStore::new();
    let proj_chunk = test_chunk("unrelated_func", "fn unrelated_func() {}");
    proj_ts
        .upsert_chunk(&proj_chunk, &mock_embedding(-1.0), Some(12345))
        .unwrap();

    let ref_idx = make_ref_index(&ref_ts, "isolated-ref");
    let opts = GatherOptions {
        expand_depth: 1,
        limit: 20,
        ..GatherOptions::default()
    };

    let result = cqs::gather_cross_index_with_index(
        &proj_ts.store,
        &ref_idx,
        &mock_embedding(1.0),
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        None,
    )
    .unwrap();

    // Should have ref seeds but no project chunks (bridge found nothing)
    assert!(!result.chunks.is_empty(), "Should have ref seed chunks");
    for c in &result.chunks {
        assert!(
            c.source.is_some(),
            "All chunks should be from reference (no bridge matches)"
        );
    }
}

#[test]
fn test_gather_cross_index_respects_limit() {
    // Reference store: multiple chunks
    let ref_ts = TestStore::new();
    for i in 0..5 {
        let chunk = test_chunk(&format!("ref_fn_{}", i), &format!("fn ref_fn_{}() {{}}", i));
        ref_ts
            .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
            .unwrap();
    }

    // Project store: multiple chunks with same embedding so bridge finds them
    let proj_ts = TestStore::new();
    for i in 0..5 {
        let chunk = test_chunk(
            &format!("proj_fn_{}", i),
            &format!("fn proj_fn_{}() {{}}", i),
        );
        proj_ts
            .upsert_chunk(&chunk, &mock_embedding(1.0), Some(12345))
            .unwrap();
    }

    let ref_idx = make_ref_index(&ref_ts, "big-ref");
    let opts = GatherOptions {
        expand_depth: 0,
        limit: 3, // tight limit
        ..GatherOptions::default()
    };

    let result = cqs::gather_cross_index_with_index(
        &proj_ts.store,
        &ref_idx,
        &mock_embedding(1.0),
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        None,
    )
    .unwrap();

    assert!(
        result.chunks.len() <= 3,
        "Should respect limit=3, got {}",
        result.chunks.len()
    );
}

#[test]
fn test_gather_cross_index_with_real_index() {
    use cqs::hnsw::HnswIndex;
    use cqs::index::VectorIndex;

    // Reference store: has "ref_func" with embedding seed 1.0
    let ref_ts = TestStore::new();
    let ref_chunk = test_chunk("ref_func", "fn ref_func() { does_stuff(); }");
    ref_ts
        .upsert_chunk(&ref_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();

    // Project store: "proj_func" matches the ref seed direction (bridge hit),
    // "proj_callee" reachable via the call graph.
    let proj_ts = TestStore::new();
    let proj_chunk = test_chunk("proj_func", "fn proj_func() { proj_callee(); }");
    let proj_callee = test_chunk("proj_callee", "fn proj_callee() {}");
    proj_ts
        .upsert_chunk(&proj_chunk, &mock_embedding(1.0), Some(12345))
        .unwrap();
    proj_ts
        .upsert_chunk(&proj_callee, &mock_embedding(2.0), Some(12345))
        .unwrap();

    proj_ts
        .upsert_function_calls(
            &PathBuf::from("test.rs"),
            &[FunctionCalls {
                name: "proj_func".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "proj_callee".to_string(),
                    line_number: 1,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();

    // Build a real HNSW index over the project chunks the way production does:
    // (chunk_id, embedding) pairs keyed identically to the project store rows.
    let embeddings: Vec<(String, _)> = vec![
        (proj_chunk.id.clone(), mock_embedding(1.0)),
        (proj_callee.id.clone(), mock_embedding(2.0)),
    ];
    let index = HnswIndex::build_with_dim(embeddings, cqs::EMBEDDING_DIM).unwrap();

    let ref_idx = make_ref_index(&ref_ts, "test-ref");
    let opts = GatherOptions {
        expand_depth: 1,
        direction: GatherDirection::Both,
        limit: 20,
        ..GatherOptions::default()
    };

    // Exercise the indexed (non-None) bridge-search path.
    let result = cqs::gather_cross_index_with_index(
        &proj_ts.store,
        &ref_idx,
        &mock_embedding(1.0),
        "test query",
        &opts,
        &PathBuf::from("/tmp"),
        Some(&index as &dyn VectorIndex),
    )
    .unwrap();

    assert!(!result.chunks.is_empty(), "Should return chunks");
    let names: Vec<&str> = result.chunks.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"ref_func"),
        "Should include the reference seed, got {names:?}"
    );
    assert!(
        names.contains(&"proj_func"),
        "Indexed bridge search should find proj_func, got {names:?}"
    );
}
