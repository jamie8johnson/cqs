#![cfg(feature = "slow-tests")]
//! Reason this file is gated: subprocess-spawning CLI tests for
//! `cqs eval --reranker`. Each test runs the full cqs binary cold start
//! which is too expensive for PR-time CI. Run via
//! `cargo test --features slow-tests` or nightly ci-slow.yml.
//!
//! TC-HAP-V1.33-2: `cqs eval --reranker` flag (#1303 / v1.33.0) had zero
//! CLI integration test. `RerankerMode::{None, Onnx, Llm}` was added with
//! three branches in `cmd_eval`: `None` short-circuits, `Onnx` builds via
//! `ctx.reranker()`, `Llm` eagerly constructs `LlmReranker::new()` so the
//! "not yet implemented" error fires before the search loop. These tests
//! pin the `none` and `llm` branches at the binary level — `onnx` requires
//! the model fixture, deferred to ci-slow's full-suite.

use assert_cmd::Command;
use serde_json::json;
use serial_test::serial;
use std::fs;
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
    // Tests are independent of freshness gating — turn it off so the
    // binary doesn't spin trying to connect to a daemon.
    c.env("CQS_EVAL_REQUIRE_FRESH", "0");
    c
}

fn write_minimal_queries(dir: &TempDir) -> std::path::PathBuf {
    let queries = json!({
        "queries": [
            {
                "query": "any query",
                "category": "structural_search",
                "gold_chunk": {
                    "name": "missing_function",
                    "origin": "src/missing.rs",
                    "line_start": 1,
                }
            }
        ]
    });
    let q_path = dir.path().join("queries.json");
    fs::write(&q_path, queries.to_string()).expect("write queries");
    q_path
}

fn seed_minimal_store(dir: &TempDir) {
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir_all(&cqs_dir).expect("create .cqs dir");
    let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let store = cqs::Store::open(&store_path).expect("open seed store");
    store
        .init(&cqs::store::ModelInfo::default())
        .expect("init seed store");
}

/// TC-HAP-V1.33-2: `--reranker none` is the historical retrieval-only
/// pipeline. The flag must parse and the binary must reach the eval
/// handler without complaining about an unrecognized option.
#[test]
#[serial]
fn eval_with_reranker_none_parses_and_reaches_handler() {
    let dir = TempDir::new().expect("tempdir");
    seed_minimal_store(&dir);
    let q_path = write_minimal_queries(&dir);

    let result = cqs_no_daemon()
        .args([
            "eval",
            q_path.to_str().unwrap(),
            "--reranker",
            "none",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run cqs eval");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    // Pin: clap accepts `--reranker none` (no "unrecognized" error).
    assert!(
        !stderr.contains("error: unrecognized")
            && !stderr.contains("error: invalid value")
            && !stderr.contains("error: a value is required"),
        "args must parse — got CLI parse error. stderr={stderr}"
    );
    // The binary may fail at embedder init if the model isn't on disk —
    // that's acceptable. What matters is it reached the eval path.
    assert!(
        !stderr.contains("Index not found"),
        "should find seeded store. stdout={stdout} stderr={stderr}"
    );
}

/// API-V1.36-2 (v1.37.0): `--reranker llm` was a placeholder enum
/// variant that errored at runtime. Per `cqs --help` truth-in-advertising,
/// v1.37.0 dropped `Llm` from `RerankerMode` so clap now rejects the
/// spelling at parse time instead of running a search that fails late.
///
/// The previous incarnation of this test (TC-HAP-V1.33-2) pinned the
/// early-construction skeleton-error contract while the variant was still
/// listed; the contract is now tighter — the spelling never reaches the
/// dispatch path at all. Pin that here so a future re-introduction of
/// the variant without an actual implementation gets noticed.
#[test]
#[serial]
fn eval_with_reranker_llm_rejected_at_parse_time() {
    let dir = TempDir::new().expect("tempdir");
    seed_minimal_store(&dir);
    let q_path = write_minimal_queries(&dir);

    let result = cqs_no_daemon()
        .args([
            "eval",
            q_path.to_str().unwrap(),
            "--reranker",
            "llm",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run cqs eval");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        !result.status.success(),
        "post-API-V1.36-2 `--reranker llm` must fail at parse time, \
         not silently succeed.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains("invalid value 'llm'") && stderr.contains("--reranker"),
        "clap must reject the spelling with the `invalid value` error \
         (signals the variant was actually removed, not just deprecated). \
         stderr={stderr}"
    );
    assert!(
        stderr.contains("[possible values: none, onnx]"),
        "clap should enumerate the surviving variants so a user typing \
         `llm` immediately sees what's actually supported. stderr={stderr}"
    );
}
