//! TC-HAP-V1.38-5 (#1463): integration tests for `cqs dead` exercised
//! end-to-end via the binary.
//!
//! Pre-fix, `tests/dead_code_test.rs` exercised the library
//! `find_dead_code` against a `TestStore` but never invoked `cmd_dead`.
//! The CLI wrapper translates between the library output, two flags
//! (`--include-pub`, `--limit`), and the JSON envelope. A bug like
//! swapping `dead` and `possibly_dead_pub` in the envelope, or
//! `--include-pub` being silently ignored, would leave no regression
//! guard. Agents query `cqs dead --json` and consume `data.dead[]`;
//! a wrong field name there breaks every consumer.
//!
//! These tests build a tiny on-disk store with `Store::upsert_chunk` +
//! `Store::upsert_function_calls`, then run `cqs --json dead` against
//! it via `assert_cmd::Command::current_dir`.

use assert_cmd::Command;
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
use cqs::store::ModelInfo;
use cqs::Store;
use serde_json::Value;
use std::path::PathBuf;
use tempfile::TempDir;

/// Default helper — no env pins. The CLI direct success path emits the
/// bare V2Bare payload (shipped default), so the `dead` object is the
/// top-level JSON value and tests read `parsed["dead"]` directly.
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// v1 compatibility helper — `CQS_OUTPUT_FORMAT=v1` restores the legacy
/// `{data, error, version, _meta}` envelope. One kept-v1 test below asserts
/// the contract still resolves.
fn cqs_no_daemon_v1() -> Command {
    let mut c = cqs_no_daemon();
    c.env("CQS_OUTPUT_FORMAT", "v1");
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
    // Build a finite vector matching the default-model dimension.
    let dim = ModelInfo::default().dimensions;
    let v: Vec<f32> = (0..dim).map(|i| ((i % 7) as f32) * 0.01).collect();
    cqs::embedder::Embedding::new(v)
}

/// Build a `.cqs/index.db` with three chunks (one definitely dead) +
/// a single `func_a → func_b` call edge. Returns the tempdir.
fn seed_dead_code_store() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let db_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    let store = Store::open(&db_path).expect("open store");
    store.init(&ModelInfo::default()).expect("init");

    let chunk_a = make_chunk("a", "func_a", "fn func_a() { func_b(); }");
    let chunk_b = make_chunk("b", "func_b", "fn func_b() {}");
    let chunk_c = make_chunk("c", "func_dead", "fn func_dead() {}");
    let emb = dummy_embedding();
    store
        .upsert_chunk(&chunk_a, &emb, Some(1))
        .expect("upsert a");
    store
        .upsert_chunk(&chunk_b, &emb, Some(1))
        .expect("upsert b");
    store
        .upsert_chunk(&chunk_c, &emb, Some(1))
        .expect("upsert c");

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

/// `cqs --json dead` (bare default) emits the payload directly: `dead[]`
/// at the top level. Pin the shape so agents that consume `dead[*].name`
/// aren't broken by an accidental field rename.
#[test]
fn dead_cli_emits_bare_payload_with_dead_field() {
    let dir = seed_dead_code_store();

    let result = cqs_no_daemon()
        .args(["dead", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs dead");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    assert!(
        result.status.success(),
        "cqs dead must succeed against seeded store. stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    // Bare default: no `data` wrapper. `dead` MUST be an array at top level.
    assert!(
        parsed.get("data").is_none(),
        "bare default must not wrap in data envelope: {parsed:?}"
    );
    let dead = parsed["dead"]
        .as_array()
        .unwrap_or_else(|| panic!("dead must be a JSON array: {parsed:?}"));

    let dead_names: Vec<&str> = dead.iter().filter_map(|d| d["name"].as_str()).collect();
    assert!(
        dead_names.contains(&"func_dead"),
        "func_dead must appear in dead[*].name: {parsed:?}"
    );
    assert!(
        !dead_names.contains(&"func_b"),
        "func_b is called by func_a so must NOT be flagged dead: {parsed:?}"
    );
}

/// Sanity: `count` matches the array length so consumers can trust the
/// count without re-iterating.
#[test]
fn dead_cli_count_matches_array_length() {
    let dir = seed_dead_code_store();

    let result = cqs_no_daemon()
        .args(["dead", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs dead");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    let dead_len = parsed["dead"].as_array().unwrap().len();
    let count = parsed["count"].as_u64().unwrap_or(u64::MAX) as usize;
    assert_eq!(count, dead_len, "count must match dead.len(): {parsed:?}");
}

/// Kept v1-compat: `CQS_OUTPUT_FORMAT=v1` restores the `{data, error,
/// version, _meta}` envelope, so `data.dead[]` still resolves for
/// consumers pinned to the legacy shape.
#[test]
fn dead_cli_v1_compat_restores_data_envelope() {
    let dir = seed_dead_code_store();

    let result = cqs_no_daemon_v1()
        .args(["dead", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs dead");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    assert!(result.status.success(), "cqs dead must succeed. {stdout}");

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    assert_eq!(parsed["version"], 1, "v1 envelope carries version: 1");
    assert!(parsed["error"].is_null(), "v1 success → error: null");
    let dead = parsed["data"]["dead"]
        .as_array()
        .unwrap_or_else(|| panic!("v1: data.dead must be a JSON array: {parsed:?}"));
    let dead_names: Vec<&str> = dead.iter().filter_map(|d| d["name"].as_str()).collect();
    assert!(dead_names.contains(&"func_dead"));
}
