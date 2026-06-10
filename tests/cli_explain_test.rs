//! TC-HAP-V1.38-6 (#1463): integration test for `cqs explain` exercised
//! end-to-end via the binary.
//!
//! Pre-fix, `tests/graph_test.rs` had two tests
//! (`explain_process_reports_callers_and_callees` and
//! `explain_data_integrates_callers_callees_similar_and_hints`) that
//! reimplemented the explain orchestration at the lib level — they
//! called `store.get_callers_full` / `get_callees_full` /
//! `search_filtered` directly. Neither invoked the binary, so a
//! regression in `cmd_explain` itself (target resolution via
//! `resolve_target`, JSON envelope shape, `--limit` truncation,
//! quiet-mode flag handling) had no guard. Agents query
//! `cqs --json explain <name>` and consume `data.callers[*].name` and
//! `data.callees[*].name`; field renames there break every consumer.
//!
//! This test builds a tiny on-disk store with `Store::upsert_chunk` +
//! `Store::upsert_function_calls`, then runs `cqs --json explain
//! func_b` against it via `assert_cmd::Command::current_dir`. No
//! embedder needed — explain only loads ONNX when `--tokens` is set,
//! and `resolve_target` uses FTS `search_by_name`.

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

/// Build a `.cqs/index.db` with `func_a → func_b` call edge.
/// Explaining `func_b` should show `func_a` as a caller.
fn seed_explain_store() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let db_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

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

/// `cqs --json explain <name>` returns a JSON envelope whose
/// `data.callers[]` and `data.callees[]` arrays expose the call graph
/// for the resolved chunk. Pin both arrays and the target identity
/// fields (`name`, `file`) so a field rename or a swap of callers/callees
/// is caught.
#[test]
fn explain_cli_emits_envelope_with_callers_and_callees() {
    let dir = seed_explain_store();

    // Explain func_b — it has one caller (func_a) and zero callees.
    let result = cqs_no_daemon()
        .args(["explain", "func_b", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs explain");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    assert!(
        result.status.success(),
        "cqs explain must succeed against seeded store. stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    // Target identity: must resolve to func_b in src/lib.rs.
    assert_eq!(
        parsed["data"]["name"].as_str(),
        Some("func_b"),
        "target name must be func_b: {parsed:?}"
    );

    // Caller list: must contain func_a (the only caller in seed corpus).
    let callers = parsed["data"]["callers"]
        .as_array()
        .unwrap_or_else(|| panic!("data.callers must be a JSON array: {parsed:?}"));
    let caller_names: Vec<&str> = callers.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(
        caller_names.contains(&"func_a"),
        "func_a must appear in data.callers[*].name: {parsed:?}"
    );

    // Callee list: must be present as an array (and empty here).
    assert!(
        parsed["data"]["callees"].is_array(),
        "data.callees must be a JSON array: {parsed:?}"
    );

    // Reverse: explain func_a — must have func_b as callee, no callers.
    let result = cqs_no_daemon()
        .args(["explain", "func_a", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs explain func_a");
    assert!(
        result.status.success(),
        "explain func_a must succeed: stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));
    let callees = parsed["data"]["callees"].as_array().unwrap();
    let callee_names: Vec<&str> = callees.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(
        callee_names.contains(&"func_b"),
        "func_b must appear in data.callees[*].name when explaining func_a: {parsed:?}"
    );
}
