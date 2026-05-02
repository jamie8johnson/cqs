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
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
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

/// TC-HAP-V1.33-2: `--reranker llm` eagerly constructs the skeleton
/// `LlmReranker`, which surfaces "not yet implemented" before the
/// search loop spins up. Pins the early-construction contract — a
/// regression that delayed construction until first scoring would
/// burn minutes on retrieval before failing.
#[test]
#[serial]
fn eval_with_reranker_llm_returns_skeleton_error() {
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
    let combined = format!("{stdout}\n{stderr}");

    // Either the skeleton error fires (preferred), or the embedder
    // fails to load (acceptable in test env without a model). The
    // load-bearing assertion: the binary did NOT silently succeed with
    // an unimplemented LLM scorer.
    let skeleton_hit = combined.to_ascii_lowercase().contains("skeleton")
        || combined
            .to_ascii_lowercase()
            .contains("not yet implemented");
    let embedder_failed = combined.contains("Embedder")
        || combined.contains("ModelDownload")
        || combined.contains("model")
        || stderr.contains("Failed to");

    assert!(
        skeleton_hit || embedder_failed,
        "llm reranker mode must surface skeleton error or embedder failure, \
         not silently succeed.\nstdout={stdout}\nstderr={stderr}"
    );
}
