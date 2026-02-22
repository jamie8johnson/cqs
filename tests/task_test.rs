//! Task integration tests (TC-1/TC-9)
//!
//! Tests the task() pipeline end-to-end: scout → gather → impact → placement.
//! Requires embedder model download — run with: cargo test task_test -- --ignored --nocapture

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::{CallSite, ChunkType, FunctionCalls, Language};
use std::path::PathBuf;

/// Set up a test store with chunks and call graph for task tests.
fn setup_task_store() -> TestStore {
    let store = TestStore::new();

    // Insert chunks: search_filtered calls validate_query and normalize_for_fts
    let chunks = vec![
        test_chunk(
            "search_filtered",
            "fn search_filtered(query: &str) { validate_query(query); normalize_for_fts(query); }",
        ),
        test_chunk(
            "validate_query",
            "fn validate_query(q: &str) -> bool { !q.is_empty() }",
        ),
        test_chunk(
            "normalize_for_fts",
            "fn normalize_for_fts(q: &str) -> String { q.to_lowercase() }",
        ),
        test_chunk(
            "test_search_basic",
            "#[test] fn test_search_basic() { search_filtered(\"hello\"); }",
        ),
    ];

    // Use distinct embeddings so search can rank them
    let embeddings = [
        mock_embedding(1.0),
        mock_embedding(2.0),
        mock_embedding(3.0),
        mock_embedding(4.0),
    ];

    for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
        store.upsert_chunk(chunk, emb, Some(12345)).unwrap();
    }

    // Insert call edges
    let function_calls = vec![
        FunctionCalls {
            name: "search_filtered".to_string(),
            line_start: 1,
            calls: vec![
                CallSite {
                    callee_name: "validate_query".to_string(),
                    line_number: 1,
                },
                CallSite {
                    callee_name: "normalize_for_fts".to_string(),
                    line_number: 1,
                },
            ],
        },
        FunctionCalls {
            name: "test_search_basic".to_string(),
            line_start: 10,
            calls: vec![CallSite {
                callee_name: "search_filtered".to_string(),
                line_number: 10,
            }],
        },
    ];
    store
        .upsert_function_calls(&PathBuf::from("test.rs"), &function_calls)
        .unwrap();

    store
}

#[test]
fn test_task_to_json_integration() {
    // Tests task_to_json with a realistic TaskResult structure
    use cqs::{ChunkRole, FileGroup, ScoutChunk, ScoutResult, ScoutSummary};
    use cqs::{RiskLevel, RiskScore, TestInfo};

    let scout = ScoutResult {
        file_groups: vec![FileGroup {
            file: PathBuf::from("src/search.rs"),
            relevance_score: 0.9,
            chunks: vec![ScoutChunk {
                name: "search_filtered".to_string(),
                chunk_type: ChunkType::Function,
                signature: "fn search_filtered()".to_string(),
                line_start: 1,
                role: ChunkRole::ModifyTarget,
                caller_count: 5,
                test_count: 2,
                search_score: 0.95,
            }],
            is_stale: false,
        }],
        relevant_notes: Vec::new(),
        summary: ScoutSummary {
            total_files: 1,
            total_functions: 1,
            untested_count: 0,
            stale_count: 0,
        },
    };

    let result = cqs::TaskResult {
        description: "add fuzzy matching".to_string(),
        scout,
        code: Vec::new(),
        risk: vec![(
            "search_filtered".to_string(),
            RiskScore {
                caller_count: 5,
                test_count: 2,
                coverage: 0.4,
                risk_level: RiskLevel::Medium,
                blast_radius: RiskLevel::Medium,
                score: 3.0,
            },
        )],
        tests: vec![TestInfo {
            name: "test_search_basic".to_string(),
            file: PathBuf::from("tests/search.rs"),
            line: 10,
            call_depth: 1,
        }],
        placement: Vec::new(),
        summary: cqs::TaskSummary {
            total_files: 1,
            total_functions: 1,
            modify_targets: 1,
            high_risk_count: 0,
            test_count: 1,
            stale_count: 0,
        },
    };

    let json = cqs::task_to_json(&result, std::path::Path::new("/project"));

    // Verify structure
    assert_eq!(json["description"], "add fuzzy matching");
    assert!(json["scout"]["file_groups"].is_array());
    assert_eq!(json["risk"].as_array().unwrap().len(), 1);
    assert_eq!(json["risk"][0]["name"], "search_filtered");
    assert_eq!(json["risk"][0]["risk_level"], "medium");
    assert_eq!(json["tests"].as_array().unwrap().len(), 1);
    assert_eq!(json["tests"][0]["name"], "test_search_basic");
    assert_eq!(json["tests"][0]["call_depth"], 1);
    assert_eq!(json["summary"]["modify_targets"], 1);
    assert_eq!(json["summary"]["test_count"], 1);
}

#[test]
fn test_extract_modify_targets_integration() {
    use cqs::extract_modify_targets;
    use cqs::{ChunkRole, FileGroup, ScoutChunk, ScoutResult, ScoutSummary};

    let scout = ScoutResult {
        file_groups: vec![
            FileGroup {
                file: PathBuf::from("src/a.rs"),
                relevance_score: 0.9,
                chunks: vec![ScoutChunk {
                    name: "func_a".to_string(),
                    chunk_type: ChunkType::Function,
                    signature: "fn func_a()".to_string(),
                    line_start: 1,
                    role: ChunkRole::ModifyTarget,
                    caller_count: 3,
                    test_count: 1,
                    search_score: 0.8,
                }],
                is_stale: false,
            },
            FileGroup {
                file: PathBuf::from("src/b.rs"),
                relevance_score: 0.7,
                chunks: vec![ScoutChunk {
                    name: "func_b".to_string(),
                    chunk_type: ChunkType::Function,
                    signature: "fn func_b()".to_string(),
                    line_start: 1,
                    role: ChunkRole::Dependency,
                    caller_count: 1,
                    test_count: 0,
                    search_score: 0.6,
                }],
                is_stale: true,
            },
        ],
        relevant_notes: Vec::new(),
        summary: ScoutSummary {
            total_files: 2,
            total_functions: 2,
            untested_count: 1,
            stale_count: 1,
        },
    };

    let targets = extract_modify_targets(&scout);
    assert_eq!(targets, vec!["func_a"]);
}

#[test]
fn test_compute_risk_and_tests_integration() {
    // Tests the combined risk + test computation (PF-2 fix)
    use cqs::compute_risk_and_tests;
    use cqs::store::{CallGraph, ChunkSummary};
    use std::collections::HashMap;

    let mut forward = HashMap::new();
    forward.insert(
        "test_search".to_string(),
        vec!["search_filtered".to_string()],
    );
    forward.insert(
        "search_filtered".to_string(),
        vec!["validate_query".to_string()],
    );

    let mut reverse = HashMap::new();
    reverse.insert(
        "search_filtered".to_string(),
        vec!["test_search".to_string(), "main".to_string()],
    );
    reverse.insert(
        "validate_query".to_string(),
        vec!["search_filtered".to_string()],
    );

    let graph = CallGraph { forward, reverse };

    let test_chunks = vec![ChunkSummary {
        id: "test_id".to_string(),
        file: PathBuf::from("tests/search.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: "test_search".to_string(),
        signature: "fn test_search()".to_string(),
        content: "#[test] fn test_search() {}".to_string(),
        doc: None,
        line_start: 1,
        line_end: 5,
        parent_id: None,
    }];

    let (scores, tests) =
        compute_risk_and_tests(&["search_filtered", "validate_query"], &graph, &test_chunks);

    assert_eq!(scores.len(), 2);
    // search_filtered: 2 callers, test_search reachable → has test coverage
    assert_eq!(scores[0].caller_count, 2);
    assert!(scores[0].test_count > 0);
    // validate_query: 1 caller (search_filtered), test_search reachable via search_filtered
    assert_eq!(scores[1].caller_count, 1);

    // Tests should be deduplicated — test_search appears for both targets
    assert!(!tests.is_empty(), "Should find affected tests");
    let test_names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
    assert!(test_names.contains(&"test_search"));
    // Ensure no duplicates
    let unique: std::collections::HashSet<&str> = test_names.iter().copied().collect();
    assert_eq!(
        unique.len(),
        test_names.len(),
        "Tests should be deduplicated"
    );
}

#[test]
#[ignore] // Requires embedder model download
fn test_task_end_to_end() {
    let store = setup_task_store();
    let embedder = cqs::Embedder::new().expect("Failed to create embedder");
    let root = PathBuf::from("/tmp/test_project");

    let result = cqs::task(&store.store, &embedder, "search for code", &root, 3);
    assert!(result.is_ok(), "task() should succeed: {:?}", result.err());

    let task_result = result.unwrap();
    assert_eq!(task_result.description, "search for code");
    assert!(
        task_result.summary.total_files > 0 || task_result.code.is_empty(),
        "Should find files or gracefully return empty results"
    );
}

#[test]
#[ignore] // Requires embedder model download
fn test_task_with_resources_end_to_end() {
    let store = setup_task_store();
    let embedder = cqs::Embedder::new().expect("Failed to create embedder");
    let root = PathBuf::from("/tmp/test_project");

    let graph = store.store.get_call_graph().unwrap();
    let test_chunks = store.store.find_test_chunks().unwrap_or_default();

    let result = cqs::task_with_resources(
        &store.store,
        &embedder,
        "validate user input",
        &root,
        3,
        &graph,
        &test_chunks,
    );
    assert!(
        result.is_ok(),
        "task_with_resources() should succeed: {:?}",
        result.err()
    );

    let task_result = result.unwrap();
    // Verify the pipeline ran — even if search returns no results, structure should be valid
    assert_eq!(task_result.description, "validate user input");
    assert!(task_result.summary.total_files >= 0);
}
