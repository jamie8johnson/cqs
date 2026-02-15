//! Tests for impact.rs (P3-11: suggest_tests, P3-12: analyze_impact)

mod common;

use common::{mock_embedding, TestStore};
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
use cqs::{analyze_impact, suggest_tests, ImpactResult};
use std::path::{Path, PathBuf};

/// Create a chunk at a specific file and line
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
    }
}

/// Create a test chunk (name starts with "test_")
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

// ===== analyze_impact tests (P3-12) =====

#[test]
fn test_analyze_impact_with_callers() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("caller_a", "src/app.rs", 1, 15),
        chunk_at("caller_b", "src/cli.rs", 1, 20),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("caller_a", 1, &[("target_fn", 5)])],
    );
    insert_calls(
        &store,
        "src/cli.rs",
        &[("caller_b", 1, &[("target_fn", 10)])],
    );

    let result = analyze_impact(&store, "target_fn", 1, false).unwrap();
    assert_eq!(result.function_name, "target_fn");
    assert!(
        result.callers.len() >= 2,
        "Should have at least 2 callers, got {}",
        result.callers.len()
    );
    let caller_names: Vec<&str> = result.callers.iter().map(|c| c.name.as_str()).collect();
    assert!(caller_names.contains(&"caller_a"));
    assert!(caller_names.contains(&"caller_b"));
}

#[test]
fn test_analyze_impact_with_tests() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("caller_fn", "src/app.rs", 1, 15),
        test_chunk_at("test_caller", "tests/test.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    // caller_fn calls target_fn, test_caller calls caller_fn
    insert_calls(
        &store,
        "src/app.rs",
        &[("caller_fn", 1, &[("target_fn", 5)])],
    );
    insert_calls(
        &store,
        "tests/test.rs",
        &[("test_caller", 1, &[("caller_fn", 3)])],
    );

    let result = analyze_impact(&store, "target_fn", 1, false).unwrap();
    assert!(
        result.tests.iter().any(|t| t.name == "test_caller"),
        "test_caller should be found via BFS: test_caller -> caller_fn -> target_fn"
    );
}

#[test]
fn test_analyze_impact_no_callers() {
    let store = TestStore::new();

    let chunks = vec![chunk_at("isolated_fn", "src/lib.rs", 1, 10)];
    insert_chunks(&store, &chunks);

    let result = analyze_impact(&store, "isolated_fn", 1, false).unwrap();
    assert_eq!(result.function_name, "isolated_fn");
    assert!(result.callers.is_empty(), "Should have no callers");
    assert!(result.tests.is_empty(), "Should have no tests");
}

#[test]
fn test_analyze_impact_transitive_callers() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("direct", "src/lib.rs", 20, 30),
        chunk_at("indirect", "src/app.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    // indirect -> direct -> target_fn
    insert_calls(
        &store,
        "src/lib.rs",
        &[("direct", 20, &[("target_fn", 25)])],
    );
    insert_calls(&store, "src/app.rs", &[("indirect", 1, &[("direct", 5)])]);

    // depth=2 should find transitive callers
    let result = analyze_impact(&store, "target_fn", 2, false).unwrap();
    let trans_names: Vec<&str> = result
        .transitive_callers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        trans_names.contains(&"direct"),
        "direct should be a transitive caller"
    );
    assert!(
        trans_names.contains(&"indirect"),
        "indirect should be a transitive caller at depth 2"
    );
}

#[test]
fn test_analyze_impact_depth_1_no_transitive() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("direct", "src/lib.rs", 20, 30),
        chunk_at("indirect", "src/app.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/lib.rs",
        &[("direct", 20, &[("target_fn", 25)])],
    );
    insert_calls(&store, "src/app.rs", &[("indirect", 1, &[("direct", 5)])]);

    // depth=1 should NOT include transitive callers
    let result = analyze_impact(&store, "target_fn", 1, false).unwrap();
    assert!(
        result.transitive_callers.is_empty(),
        "depth=1 should not include transitive callers"
    );
}

// ===== suggest_tests tests (P3-11) =====

#[test]
fn test_suggest_tests_for_untested_caller() {
    let store = TestStore::new();

    // target_fn has caller_fn (untested) and test_caller (a test)
    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("untested_caller", "src/app.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("untested_caller", 1, &[("target_fn", 5)])],
    );

    let impact = analyze_impact(&store, "target_fn", 1, false).unwrap();
    let suggestions = suggest_tests(&store, &impact);

    // untested_caller has no tests reaching it, should get a suggestion
    assert!(
        suggestions
            .iter()
            .any(|s| s.for_function == "untested_caller"),
        "Should suggest test for untested_caller, got: {:?}",
        suggestions
            .iter()
            .map(|s| &s.for_function)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_suggest_tests_no_suggestions_when_tested() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("caller_fn", "src/app.rs", 1, 15),
        test_chunk_at("test_caller", "tests/test.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    // test_caller calls caller_fn, caller_fn calls target_fn
    insert_calls(
        &store,
        "src/app.rs",
        &[("caller_fn", 1, &[("target_fn", 5)])],
    );
    insert_calls(
        &store,
        "tests/test.rs",
        &[("test_caller", 1, &[("caller_fn", 3)])],
    );

    let impact = analyze_impact(&store, "target_fn", 1, false).unwrap();
    let suggestions = suggest_tests(&store, &impact);

    // caller_fn is tested via test_caller — no suggestion needed
    assert!(
        !suggestions.iter().any(|s| s.for_function == "caller_fn"),
        "Should not suggest test for already-tested caller_fn"
    );
}

#[test]
fn test_suggest_tests_empty_impact() {
    let store = TestStore::new();

    let chunks = vec![chunk_at("lonely_fn", "src/lib.rs", 1, 10)];
    insert_chunks(&store, &chunks);

    let impact = ImpactResult {
        function_name: "lonely_fn".to_string(),
        callers: Vec::new(),
        tests: Vec::new(),
        transitive_callers: Vec::new(),
        type_impacted: Vec::new(),
    };
    let suggestions = suggest_tests(&store, &impact);
    assert!(suggestions.is_empty(), "No callers means no suggestions");
}

#[test]
fn test_suggest_tests_generates_correct_name() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10),
        chunk_at("process_data", "src/app.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("process_data", 1, &[("target_fn", 5)])],
    );

    let impact = analyze_impact(&store, "target_fn", 1, false).unwrap();
    let suggestions = suggest_tests(&store, &impact);

    if let Some(suggestion) = suggestions
        .iter()
        .find(|s| s.for_function == "process_data")
    {
        assert_eq!(
            suggestion.test_name, "test_process_data",
            "Rust test name should be test_ prefixed"
        );
    }
}
