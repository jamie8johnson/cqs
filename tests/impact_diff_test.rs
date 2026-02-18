//! Diff-aware impact tests (map_hunks_to_functions, analyze_diff_impact)

mod common;

use common::{mock_embedding, TestStore};
use cqs::diff_parse::{parse_unified_diff, DiffHunk};
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
use cqs::{analyze_diff_impact, map_hunks_to_functions, ChangedFunction};
use std::path::{Path, PathBuf};

/// Create a test chunk with custom file and line range
fn chunk_at(name: &str, file: &str, line_start: u32, line_end: u32) -> Chunk {
    let content = format!("fn {}() {{ }}", name);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content,
        doc: None,
        line_start,
        line_end,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
    }
}

/// Insert chunks into the store
fn insert_chunks(store: &TestStore, chunks: &[Chunk]) {
    let emb = mock_embedding(1.0);
    let pairs: Vec<_> = chunks.iter().map(|c| (c.clone(), emb.clone())).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();
}

/// Insert function call graph entries
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

// ===== map_hunks_to_functions tests =====

#[test]
fn test_map_hunks_to_functions() {
    let store = TestStore::new();
    let chunks = vec![
        chunk_at("foo", "src/lib.rs", 10, 20),
        chunk_at("bar", "src/lib.rs", 30, 40),
    ];
    insert_chunks(&store, &chunks);

    // Hunk at lines 15-17 overlaps foo (10-20)
    let hunks = vec![DiffHunk {
        file: "src/lib.rs".to_string(),
        start: 15,
        count: 3,
    }];

    let result = map_hunks_to_functions(&store, &hunks);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "foo");
}

#[test]
fn test_map_hunks_no_overlap() {
    let store = TestStore::new();
    let chunks = vec![
        chunk_at("foo", "src/lib.rs", 10, 20),
        chunk_at("bar", "src/lib.rs", 30, 40),
    ];
    insert_chunks(&store, &chunks);

    // Hunk at lines 22-28 — between foo and bar
    let hunks = vec![DiffHunk {
        file: "src/lib.rs".to_string(),
        start: 22,
        count: 7,
    }];

    let result = map_hunks_to_functions(&store, &hunks);
    assert!(result.is_empty(), "Should find no functions in the gap");
}

#[test]
fn test_map_hunks_partial_overlap() {
    let store = TestStore::new();
    let chunks = vec![chunk_at("foo", "src/lib.rs", 10, 20)];
    insert_chunks(&store, &chunks);

    // Hunk starts at line 10 (first line of foo)
    let hunks = vec![DiffHunk {
        file: "src/lib.rs".to_string(),
        start: 10,
        count: 1,
    }];

    let result = map_hunks_to_functions(&store, &hunks);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "foo");
}

#[test]
fn test_map_hunks_boundary_off_by_one() {
    let store = TestStore::new();
    // Function at lines 10-20
    let chunks = vec![chunk_at("foo", "src/lib.rs", 10, 20)];
    insert_chunks(&store, &chunks);

    // Hunk covers lines [8, 10) — exclusive end at 10 means it doesn't touch foo
    let hunks = vec![DiffHunk {
        file: "src/lib.rs".to_string(),
        start: 8,
        count: 2, // lines 8, 9 — exclusive end at 10
    }];

    let result = map_hunks_to_functions(&store, &hunks);
    assert!(
        result.is_empty(),
        "Hunk ending at line 10 (exclusive) should NOT match chunk starting at line 10"
    );
}

#[test]
fn test_map_hunks_non_indexed_file() {
    let store = TestStore::new();
    // No chunks in the store for this file

    let hunks = vec![DiffHunk {
        file: "src/unknown.rs".to_string(),
        start: 1,
        count: 10,
    }];

    let result = map_hunks_to_functions(&store, &hunks);
    assert!(result.is_empty(), "Non-indexed file should return empty");
}

// ===== analyze_diff_impact tests =====

#[test]
fn test_diff_impact_aggregation() {
    let store = TestStore::new();

    // Two changed functions
    let chunks = vec![
        chunk_at("fn_a", "src/lib.rs", 10, 20),
        chunk_at("fn_b", "src/lib.rs", 30, 40),
        // shared_caller calls both fn_a and fn_b
        chunk_at("shared_caller", "src/app.rs", 1, 10),
        // test calls shared_caller
        chunk_at("test_shared", "tests/test.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("shared_caller", 1, &[("fn_a", 5), ("fn_b", 8)])],
    );
    insert_calls(
        &store,
        "tests/test.rs",
        &[("test_shared", 1, &[("shared_caller", 3)])],
    );

    let changed = map_hunks_to_functions(
        &store,
        &[
            DiffHunk {
                file: "src/lib.rs".to_string(),
                start: 15,
                count: 1,
            },
            DiffHunk {
                file: "src/lib.rs".to_string(),
                start: 35,
                count: 1,
            },
        ],
    );

    assert_eq!(changed.len(), 2, "Should find fn_a and fn_b");

    let result = analyze_diff_impact(&store, changed).unwrap();
    // shared_caller appears once even though it calls both changed functions
    assert!(
        result.all_callers.len() <= 1,
        "Shared caller should be deduped: got {}",
        result.all_callers.len()
    );
}

#[test]
fn test_diff_impact_empty_functions() {
    let store = TestStore::new();

    let result = analyze_diff_impact(&store, vec![]).unwrap();
    assert!(result.changed_functions.is_empty());
    assert!(result.all_callers.is_empty());
    assert!(result.all_tests.is_empty());
    assert_eq!(result.summary.changed_count, 0);
    assert_eq!(result.summary.caller_count, 0);
    assert_eq!(result.summary.test_count, 0);
}

// ===== End-to-end: parse diff → map → analyze =====

#[test]
fn test_diff_to_impact_end_to_end() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("search_fn", "src/search.rs", 10, 30),
        chunk_at("cmd_query", "src/cli.rs", 1, 20),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/cli.rs",
        &[("cmd_query", 1, &[("search_fn", 10)])],
    );

    let diff = "\
diff --git a/src/search.rs b/src/search.rs
--- a/src/search.rs
+++ b/src/search.rs
@@ -15,3 +15,4 @@ fn search_fn() {
     let x = 1;
+    let y = 2;
";

    let hunks = parse_unified_diff(diff);
    assert_eq!(hunks.len(), 1);

    let changed = map_hunks_to_functions(&store, &hunks);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].name, "search_fn");

    let result = analyze_diff_impact(&store, changed).unwrap();

    assert_eq!(result.summary.changed_count, 1);
    // cmd_query should be in callers
    assert!(
        result.all_callers.iter().any(|c| c.name == "cmd_query"),
        "cmd_query should be an affected caller"
    );
}

// ===== Test discovery verification (TC-9) =====

/// Create a test chunk (name starts with "test_") recognized as a test by the store
fn test_chunk_at(name: &str, file: &str, line_start: u32, line_end: u32) -> Chunk {
    let content = format!("#[test] fn {}() {{ }}", name);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content,
        doc: None,
        line_start,
        line_end,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
    }
}

#[test]
fn test_diff_impact_all_tests_populated() {
    let store = TestStore::new();

    // Setup: changed_fn -> caller_fn, test_fn -> caller_fn
    let chunks = vec![
        chunk_at("changed_fn", "src/lib.rs", 10, 20),
        chunk_at("caller_fn", "src/app.rs", 1, 15),
        test_chunk_at("test_caller", "tests/app_test.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("caller_fn", 1, &[("changed_fn", 5)])],
    );
    insert_calls(
        &store,
        "tests/app_test.rs",
        &[("test_caller", 1, &[("caller_fn", 3)])],
    );

    let changed = vec![ChangedFunction {
        name: "changed_fn".to_string(),
        file: "src/lib.rs".to_string(),
        line_start: 10,
    }];

    let result = analyze_diff_impact(&store, changed).unwrap();

    // all_tests should contain test_caller
    assert_eq!(result.summary.test_count, 1, "Should find 1 affected test");
    assert!(
        result.all_tests.iter().any(|t| t.name == "test_caller"),
        "test_caller should be in all_tests, got: {:?}",
        result.all_tests.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // Verify the test's via field points to the changed function
    let test_entry = result
        .all_tests
        .iter()
        .find(|t| t.name == "test_caller")
        .expect("test_caller should be present");
    assert_eq!(
        test_entry.via, "changed_fn",
        "via should point to the changed function that leads to this test"
    );
    assert!(
        test_entry.call_depth > 0,
        "call_depth should be > 0 (test is not the changed function itself)"
    );
}

#[test]
fn test_diff_impact_via_attribution_multiple_functions() {
    let store = TestStore::new();

    // Two changed functions, each with a dedicated test path:
    //   test_alpha -> mid_alpha -> fn_alpha (changed)
    //   test_beta -> fn_beta (changed)
    let chunks = vec![
        chunk_at("fn_alpha", "src/lib.rs", 10, 20),
        chunk_at("fn_beta", "src/lib.rs", 30, 40),
        chunk_at("mid_alpha", "src/app.rs", 1, 10),
        test_chunk_at("test_alpha", "tests/test.rs", 1, 10),
        test_chunk_at("test_beta", "tests/test.rs", 20, 30),
    ];
    insert_chunks(&store, &chunks);

    // mid_alpha calls fn_alpha
    insert_calls(
        &store,
        "src/app.rs",
        &[("mid_alpha", 1, &[("fn_alpha", 5)])],
    );
    // test_alpha calls mid_alpha, test_beta calls fn_beta
    insert_calls(
        &store,
        "tests/test.rs",
        &[
            ("test_alpha", 1, &[("mid_alpha", 3)]),
            ("test_beta", 20, &[("fn_beta", 25)]),
        ],
    );

    let changed = vec![
        ChangedFunction {
            name: "fn_alpha".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 10,
        },
        ChangedFunction {
            name: "fn_beta".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 30,
        },
    ];

    let result = analyze_diff_impact(&store, changed).unwrap();

    assert_eq!(result.summary.test_count, 2, "Should find 2 affected tests");

    // Verify via attribution: test_alpha should point to fn_alpha
    let alpha_test = result
        .all_tests
        .iter()
        .find(|t| t.name == "test_alpha")
        .expect("test_alpha should be found");
    assert_eq!(
        alpha_test.via, "fn_alpha",
        "test_alpha's via should be fn_alpha (shortest path: test_alpha -> mid_alpha -> fn_alpha)"
    );

    // test_beta should point to fn_beta (direct call, depth 1)
    let beta_test = result
        .all_tests
        .iter()
        .find(|t| t.name == "test_beta")
        .expect("test_beta should be found");
    assert_eq!(
        beta_test.via, "fn_beta",
        "test_beta's via should be fn_beta (direct call)"
    );
    assert_eq!(
        beta_test.call_depth, 1,
        "test_beta is a direct caller of fn_beta, so depth should be 1"
    );
}

#[test]
fn test_diff_impact_shared_test_via_minimum_depth() {
    let store = TestStore::new();

    // A single test reaches two changed functions at different depths:
    //   test_shared -> fn_close (changed, depth 1)
    //   test_shared -> mid -> fn_far (changed, depth 2)
    // The via field should attribute to fn_close (minimum depth)
    let chunks = vec![
        chunk_at("fn_close", "src/lib.rs", 10, 20),
        chunk_at("fn_far", "src/lib.rs", 30, 40),
        chunk_at("mid", "src/app.rs", 1, 10),
        test_chunk_at("test_shared", "tests/test.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(&store, "src/app.rs", &[("mid", 1, &[("fn_far", 5)])]);
    insert_calls(
        &store,
        "tests/test.rs",
        &[("test_shared", 1, &[("fn_close", 3), ("mid", 5)])],
    );

    let changed = vec![
        ChangedFunction {
            name: "fn_close".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 10,
        },
        ChangedFunction {
            name: "fn_far".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 30,
        },
    ];

    let result = analyze_diff_impact(&store, changed).unwrap();

    assert_eq!(
        result.summary.test_count, 1,
        "test_shared should appear once (deduplicated)"
    );
    let test = &result.all_tests[0];
    assert_eq!(test.name, "test_shared");
    assert_eq!(
        test.via, "fn_close",
        "via should attribute to fn_close (minimum depth path)"
    );
    assert_eq!(
        test.call_depth, 1,
        "call_depth should be 1 (direct call to fn_close)"
    );
}
