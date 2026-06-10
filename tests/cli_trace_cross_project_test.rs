//! TC-HAP-V1.38-8 (#1463): integration test for `cqs trace --cross-project`
//! exercised end-to-end via the binary.
//!
//! `cmd_trace` has two arms: the local-trace fast-path and the
//! `--cross-project` arm that opens `CrossProjectContext::from_config`
//! and walks the merged call graph across the local store + every
//! configured `[references]` entry. Pre-fix, the cross-project arm had
//! ZERO tests (`tests/graph_test.rs` only exercises the local arm via
//! `find_call_path` lib-level helpers; `cli_dispatch_test.rs` doesn't
//! touch it). A regression that:
//!   - swapped the result type from `CrossProjectTraceResult` to the
//!     local `TraceOutput` (callers consuming `data.path[*].project`
//!     would break — the local shape has no `project` field)
//!   - dropped the `--cross-project` flag binding so it silently fell
//!     through to the local arm
//!   - flipped the if/else gate at line 114
//! …would not break any existing test yet would silently corrupt every
//! agent's cross-project consumer.
//!
//! This test seeds a tempdir with a single-project store (no refs
//! configured) containing a `func_a → func_b` call edge, then runs
//! `cqs --json trace func_a func_b --cross-project` and asserts the
//! envelope shape — `data.path[*].project = "local"` is the
//! load-bearing field that distinguishes cross-project from local
//! envelope.

use assert_cmd::Command;
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
use cqs::store::ModelInfo;
use cqs::Store;
use serde_json::Value;
use std::path::PathBuf;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    let mut c = Command::cargo_bin("cqs").expect("Failed to find cqs binary");
    // Kept-v1 compat set: the default wire shape is V2Bare since
    // v1.40.0. These tests pin `CQS_OUTPUT_FORMAT=v1` to exercise the
    // surviving legacy-envelope contract, so `parsed["data"][...]`
    // assertions keep working. The bare default is asserted end-to-end in
    // tests/cli_envelope_test.rs, tests/cli_dead_test.rs, and
    // tests/cli_chat_format_test.rs.
    c.env("CQS_OUTPUT_FORMAT", "v1");
    c
}

fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

fn make_chunk(id: &str, name: &str, content: &str) -> Chunk {
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: id.to_string(),
        file: PathBuf::from("src/lib.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: content.to_string(),
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash,
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    }
}

fn dummy_embedding() -> cqs::embedder::Embedding {
    let dim = ModelInfo::default().dimensions;
    let v: Vec<f32> = (0..dim).map(|i| ((i % 7) as f32) * 0.01).collect();
    cqs::embedder::Embedding::new(v)
}

/// Build a `.cqs/slots/default/index.db` with `func_a → func_b` call
/// edge and no configured refs. `--cross-project` will open just the
/// local store as the only "project" — sufficient to pin the envelope
/// shape. Seed at the slots path directly so the binary's
/// legacy-migration step doesn't move the DB out from under
/// `CrossProjectContext::from_config`'s hardcoded `.cqs/index.db` lookup.
fn seed_trace_store() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let slot_dir = dir.path().join(".cqs").join("slots").join("default");
    std::fs::create_dir_all(&slot_dir).expect("mkdir slots/default");
    // Active-slot pointer + slot dir layout is what the binary
    // expects post-slot-migration. Pre-creating both keeps
    // `migrate_legacy_index_to_default_slot` a no-op.
    std::fs::write(dir.path().join(".cqs").join("active-slot"), "default")
        .expect("write active-slot");
    let db_path = slot_dir.join(cqs::INDEX_DB_FILENAME);

    let store = Store::open(&db_path).expect("open store");
    store.init(&ModelInfo::default()).expect("init");

    let chunk_a = make_chunk("a", "func_a", "fn func_a() { func_b(); }");
    let chunk_b = make_chunk("b", "func_b", "fn func_b() {}");
    let emb = dummy_embedding();
    store
        .upsert_chunk(&chunk_a, &emb, Some(1))
        .expect("upsert a");
    store
        .upsert_chunk(&chunk_b, &emb, Some(1))
        .expect("upsert b");

    let calls = vec![FunctionCalls {
        name: "func_a".to_string(),
        line_start: 1,
        calls: vec![CallSite {
            callee_name: "func_b".to_string(),
            line_number: 1,
        }],
    }];
    store
        .upsert_function_calls(&PathBuf::from("src/lib.rs"), &calls)
        .expect("upsert function_calls");

    drop(store);
    dir
}

/// `cqs --json trace <s> <t> --cross-project` returns the cross-project
/// envelope (`CrossProjectTraceResult`) — `path[*].project` is the
/// load-bearing field. With no configured refs the only project is
/// `"local"`. Pin the envelope shape AND the flag-routing — a regression
/// that flipped the if/else gate would yield the local-arm envelope
/// instead, which has no `project` field on hops.
#[test]
fn trace_cross_project_emits_envelope_with_project_tagged_hops() {
    let dir = seed_trace_store();

    let result = cqs_no_daemon()
        .args(["trace", "func_a", "func_b", "--cross-project", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs trace --cross-project");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    assert!(
        result.status.success(),
        "cqs trace --cross-project must succeed against seeded local store. \
         stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    // `found = true` plus a path of length 2 (func_a → func_b).
    assert_eq!(
        parsed["data"]["found"].as_bool(),
        Some(true),
        "cross-project trace must find the path: {parsed:?}"
    );
    let path = parsed["data"]["path"]
        .as_array()
        .unwrap_or_else(|| panic!("data.path must be a JSON array: {parsed:?}"));
    assert_eq!(
        path.len(),
        2,
        "path should be func_a → func_b (2 hops): {parsed:?}"
    );
    assert_eq!(path[0]["name"], "func_a");
    assert_eq!(path[1]["name"], "func_b");

    // The load-bearing schema difference vs the local-arm envelope:
    // every hop carries a `project` field (empty string OK on the
    // source hop today; populated on resolved hops). A regression that
    // fell through to the local arm would emit hops with no `project`
    // key at all — `as_str()` would return `None` instead of `Some(...)`.
    assert!(
        path[0]["project"].as_str().is_some(),
        "hops must carry a `project` field (cross-project envelope shape): {parsed:?}"
    );
    assert!(
        path[1]["project"].as_str().is_some(),
        "hops must carry a `project` field (cross-project envelope shape): {parsed:?}"
    );
    // At least one hop must be tagged "local" since no refs are
    // configured — pins that the resolver actually opened the local
    // store, not just constructed an empty CrossProjectContext.
    let projects: Vec<&str> = path.iter().filter_map(|h| h["project"].as_str()).collect();
    assert!(
        projects.contains(&"local"),
        "at least one hop must be tagged 'local' (proves the local store was \
         opened by from_config): {parsed:?}"
    );

    // `depth` is hops - 1. Pin so a future off-by-one (depth = path.len())
    // is caught by the integration boundary.
    assert_eq!(
        parsed["data"]["depth"].as_u64(),
        Some(1),
        "depth must be hops-1: {parsed:?}"
    );
}

/// Negative arm: `cqs --json trace <s> <t> --cross-project` against a
/// seeded store where the call edge does NOT exist must emit the
/// envelope with `found = false`, `path = null` (skip-serialized so
/// the key is absent), and `depth = null`. Pins the not-found shape so
/// agents that branch on `data.found` get a stable contract.
#[test]
fn trace_cross_project_not_found_envelope_shape() {
    let dir = seed_trace_store();

    let result = cqs_no_daemon()
        .args(["trace", "func_b", "func_a", "--cross-project", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs trace --cross-project (no path)");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    assert!(
        result.status.success(),
        "trace must succeed even when no path exists; stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    assert_eq!(
        parsed["data"]["found"].as_bool(),
        Some(false),
        "no-path case must report found=false: {parsed:?}"
    );
    // `path` and `depth` are `#[serde(skip_serializing_if = "Option::is_none")]`,
    // so the keys are absent from the envelope when None.
    assert!(
        parsed["data"].get("path").is_none() || parsed["data"]["path"].is_null(),
        "no-path case must omit data.path: {parsed:?}"
    );
}
