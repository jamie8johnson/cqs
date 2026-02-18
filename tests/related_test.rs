//! Tests for related.rs (P3-9: find_related, resolve_to_related, find_type_overlap)

mod common;

use common::{mock_embedding, TestStore};
use cqs::find_related;
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language, TypeEdgeKind, TypeRef};
use std::path::{Path, PathBuf};

/// Create a chunk at a specific file and line
fn chunk_at(name: &str, file: &str, line_start: u32, line_end: u32, sig: &str) -> Chunk {
    let content = format!("fn {}() {{ }}", name);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: sig.to_string(),
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

// ===== find_related with shared callers =====

#[test]
fn test_find_related_shared_callers() {
    let store = TestStore::new();

    // Setup: A calls both target and sibling
    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10, "fn target_fn()"),
        chunk_at("sibling_fn", "src/lib.rs", 20, 30, "fn sibling_fn()"),
        chunk_at("caller_a", "src/app.rs", 1, 15, "fn caller_a()"),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("caller_a", 1, &[("target_fn", 5), ("sibling_fn", 10)])],
    );

    let result = find_related(&store, "target_fn", 10).unwrap();
    assert_eq!(result.target, "target_fn");
    // sibling_fn shares a caller (caller_a) with target_fn
    assert!(
        result.shared_callers.iter().any(|r| r.name == "sibling_fn"),
        "sibling_fn should share a caller with target_fn, got: {:?}",
        result
            .shared_callers
            .iter()
            .map(|r| &r.name)
            .collect::<Vec<_>>()
    );
}

// ===== find_related with shared callees =====

#[test]
fn test_find_related_shared_callees() {
    let store = TestStore::new();

    // Setup: target and sibling both call helper
    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10, "fn target_fn()"),
        chunk_at("sibling_fn", "src/lib.rs", 20, 30, "fn sibling_fn()"),
        chunk_at("helper", "src/utils.rs", 1, 5, "fn helper()"),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/lib.rs",
        &[
            ("target_fn", 1, &[("helper", 5)]),
            ("sibling_fn", 20, &[("helper", 25)]),
        ],
    );

    let result = find_related(&store, "target_fn", 10).unwrap();
    assert!(
        result.shared_callees.iter().any(|r| r.name == "sibling_fn"),
        "sibling_fn should share a callee (helper) with target_fn, got: {:?}",
        result
            .shared_callees
            .iter()
            .map(|r| &r.name)
            .collect::<Vec<_>>()
    );
}

// ===== find_related with no relations =====

#[test]
fn test_find_related_empty_result() {
    let store = TestStore::new();

    // Single isolated function — no callers, no callees, no types
    let chunks = vec![chunk_at("lonely_fn", "src/lib.rs", 1, 5, "fn lonely_fn()")];
    insert_chunks(&store, &chunks);

    let result = find_related(&store, "lonely_fn", 10).unwrap();
    assert_eq!(result.target, "lonely_fn");
    assert!(result.shared_callers.is_empty());
    assert!(result.shared_callees.is_empty());
    assert!(result.shared_types.is_empty());
}

// ===== find_related with limit =====

#[test]
fn test_find_related_limit_works() {
    let store = TestStore::new();

    // Setup: 3 siblings all called by the same caller
    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 1, 10, "fn target_fn()"),
        chunk_at("sib_a", "src/lib.rs", 20, 25, "fn sib_a()"),
        chunk_at("sib_b", "src/lib.rs", 30, 35, "fn sib_b()"),
        chunk_at("sib_c", "src/lib.rs", 40, 45, "fn sib_c()"),
        chunk_at("common_caller", "src/app.rs", 1, 20, "fn common_caller()"),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[(
            "common_caller",
            1,
            &[("target_fn", 3), ("sib_a", 6), ("sib_b", 9), ("sib_c", 12)],
        )],
    );

    // Limit to 2 shared callers
    let result = find_related(&store, "target_fn", 2).unwrap();
    assert!(
        result.shared_callers.len() <= 2,
        "Should respect limit=2, got {}",
        result.shared_callers.len()
    );
}

// ===== find_related with shared types =====

#[test]
fn test_find_related_shared_types() {
    let store = TestStore::new();

    // Both functions use Config in their signature
    let chunks = vec![
        chunk_at(
            "parse_config",
            "src/config.rs",
            1,
            10,
            "fn parse_config(cfg: Config) -> Result",
        ),
        chunk_at(
            "validate_config",
            "src/config.rs",
            20,
            30,
            "fn validate_config(cfg: Config) -> bool",
        ),
        chunk_at(
            "render_ui",
            "src/ui.rs",
            1,
            10,
            "fn render_ui(state: AppState)",
        ),
    ];
    insert_chunks(&store, &chunks);

    // Insert type edges: both parse_config and validate_config reference Config
    let config_ref = TypeRef {
        type_name: "Config".to_string(),
        line_number: 1,
        kind: Some(TypeEdgeKind::Param),
    };
    store
        .upsert_type_edges(&chunks[0].id, std::slice::from_ref(&config_ref))
        .unwrap();
    store
        .upsert_type_edges(&chunks[1].id, std::slice::from_ref(&config_ref))
        .unwrap();
    // render_ui references AppState (different type)
    store
        .upsert_type_edges(
            &chunks[2].id,
            &[TypeRef {
                type_name: "AppState".to_string(),
                line_number: 1,
                kind: Some(TypeEdgeKind::Param),
            }],
        )
        .unwrap();

    let result = find_related(&store, "parse_config", 10).unwrap();
    // validate_config shares the "Config" type
    let type_names: Vec<&str> = result
        .shared_types
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert!(
        type_names.contains(&"validate_config"),
        "validate_config should share type Config, got: {:?}",
        type_names
    );
    assert!(
        !type_names.contains(&"render_ui"),
        "render_ui does not share types"
    );
}

// ===== find_related nonexistent function =====

#[test]
fn test_find_related_nonexistent_target() {
    let store = TestStore::new();

    let result = find_related(&store, "ghost_fn", 10);
    assert!(result.is_err(), "Should fail for nonexistent function");
}
