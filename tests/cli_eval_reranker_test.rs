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
//! "not yet implemented" error fires before the search loop.
//!
//! Coverage by branch:
//! - `none` — `eval_with_reranker_none_parses_and_reaches_handler`.
//! - `llm` — `eval_with_reranker_llm_rejected_at_parse_time` (the variant was
//!   dropped, so clap now rejects the spelling at parse time).
//! - `onnx` — the only arm that constructs a reranker. `ctx.reranker()` is
//!   lazy, so the model resolves on the first `rerank`, which
//!   `runner::search_for_rank` makes only when stage-1 returns >1 hit. Two
//!   tests cover it: `eval_reranker_onnx_reaches_reranker_model_resolution`
//!   (non-ignored — a real index makes stage-1 return >1 hit, and an injected
//!   missing `CQS_RERANKER_MODEL` proves the arm reaches reranker model
//!   resolution rather than falling back to retrieval-only) and the
//!   `#[ignore]`d `eval_reranker_onnx_runs_real_cross_encoder` (downloads the
//!   ~91MB ms-marco cross-encoder and asserts a clean end-to-end rerank).

mod common;

use assert_cmd::Command;
use common::cqs_v1 as cqs;
use serde_json::json;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

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

/// Build a real, indexed project with ≥2 code chunks so eval's stage-1
/// retrieval returns >1 hit — the precondition for `runner::search_for_rank`
/// to actually invoke `reranker.rerank` (it no-ops on ≤1 result, so the
/// 0-chunk `seed_minimal_store` never reaches the reranker at all). Mirrors
/// the `init` + `index` subprocess convention the other slow-tests use (e.g.
/// tests/onboard_test.rs); the real embedder runs, hence the `slow-tests` gate
/// on this whole file.
fn setup_indexed_project() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(
        src.join("lib.rs"),
        r#"
/// Parse a raw config string into key/value pairs.
pub fn parse_config(raw: &str) -> Vec<(String, String)> {
    raw.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Validate that a parsed config has the required keys.
pub fn validate_config(pairs: &[(String, String)]) -> bool {
    pairs.iter().any(|(k, _)| k == "name")
}

/// Render config pairs back to a display string.
pub fn render_config(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}
"#,
    )
    .expect("write lib.rs");

    // init + index via subprocess (real embedder + HNSW build). CQS_NO_DAEMON
    // keeps the run self-contained — the tempdir is not a project any running
    // daemon watches, and forcing CLI mode removes socket dependence entirely.
    let mut init = cqs();
    init.env("CQS_NO_DAEMON", "1");
    init.args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    let mut index = cqs();
    index.env("CQS_NO_DAEMON", "1");
    let out = index
        .args(["index"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs index");
    assert!(
        out.status.success(),
        "cqs index must succeed to seed a >=2-chunk store. stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// Query file whose gold points at a real indexed function. The reranker fires
/// regardless of whether the gold is hit — it keys on stage-1 result count,
/// not correctness — so the gold is only here to make the report well-formed.
fn write_indexed_query(dir: &TempDir) -> std::path::PathBuf {
    let queries = json!({
        "queries": [
            {
                "query": "parse configuration key value pairs",
                "category": "structural_search",
                "gold_chunk": {
                    "name": "parse_config",
                    "origin": "src/lib.rs",
                    "line_start": 1,
                }
            }
        ]
    });
    let q_path = dir.path().join("queries.json");
    fs::write(&q_path, queries.to_string()).expect("write queries");
    q_path
}

/// The `onnx` arm (`RerankerMode::Onnx => Some(ctx.reranker()?)` in `cmd_eval`)
/// is the only `--reranker` value that constructs a reranker. `ctx.reranker()`
/// is lazy — the model resolves on the first `rerank` call, which
/// `runner::search_for_rank` only makes when stage-1 returns >1 result — so a
/// real >=2-chunk index is the minimum that reaches the reranker's model
/// resolution.
///
/// Inject a nonexistent `CQS_RERANKER_MODEL` path: the arm still constructs the
/// reranker, stage-1 returns >1 hit (retrieval threshold is 0.0), and `rerank`
/// resolves the missing model and errors. `run_eval` catches per-query search
/// errors and scores them as a miss — so the process still exits 0 — logging
/// the failure at WARN, which the binary's default `cqs=info,warn` env filter
/// prints to stderr. Asserting that warn names the reranker model-resolution
/// failure proves the onnx arm was genuinely taken: a `--reranker none` run
/// never touches the reranker, so the string cannot appear.
#[test]
#[serial]
fn eval_reranker_onnx_reaches_reranker_model_resolution() {
    let dir = setup_indexed_project();
    let q_path = write_indexed_query(&dir);

    // Absolute, path-like, guaranteed-missing → aux_model::resolve_raw returns
    // NotFound("reranker override <path> does not exist"), surfaced as
    // RerankerError::ModelDownload and wrapped by the runner as
    // "Reranking failed: ...".
    let missing_model = dir.path().join("no-such-reranker-model");

    let result = cqs_no_daemon()
        .env("CQS_RERANKER_MODEL", &missing_model)
        .args([
            "eval",
            q_path.to_str().unwrap(),
            "--reranker",
            "onnx",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run cqs eval");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    // Caught-error contract: a per-query rerank failure is scored as a miss,
    // the eval still completes and exits 0.
    assert!(
        result.status.success(),
        "eval must complete (a rerank failure is scored as a miss, not fatal). \
         stdout={stdout} stderr={stderr}"
    );
    // The onnx arm reached reranker model resolution: rerank was attempted
    // (proves stage-1 returned >1 result and the arm constructed a reranker)
    // and it failed resolving our injected missing model path.
    assert!(
        stderr.contains("Reranking failed"),
        "onnx arm must attempt rerank — never reached on `none`. stderr={stderr}"
    );
    assert!(
        stderr.contains("reranker override") && stderr.contains("does not exist"),
        "the failure must name the reranker model-resolution path, proving the \
         onnx arm was taken rather than silently falling back. stderr={stderr}"
    );
}

/// End-to-end `eval --reranker onnx` against the real ms-marco-MiniLM-L-6-v2
/// cross-encoder. `#[ignore]`d because the first run cold-downloads the ~91MB
/// ONNX model from the HuggingFace Hub (and loads it, on GPU when available).
///
/// Run manually with:
/// ```text
/// cargo test --features slow-tests,cuda-index \
///     --test cli_eval_reranker_test \
///     eval_reranker_onnx_runs_real_cross_encoder -- --ignored --nocapture
/// ```
///
/// Asserts shape, not scores: the reranker constructs and runs cleanly (no
/// "Reranking failed" on stderr) and eval emits a well-formed report. This is
/// the only test that exercises a genuine `Reranker::rerank` at the binary
/// level; the non-ignored sibling proves the arm is reached without the
/// download.
#[test]
#[serial]
#[ignore = "downloads ~91MB ms-marco cross-encoder; run with --ignored"]
fn eval_reranker_onnx_runs_real_cross_encoder() {
    let dir = setup_indexed_project();
    let q_path = write_indexed_query(&dir);

    let result = cqs_no_daemon()
        .args([
            "eval",
            q_path.to_str().unwrap(),
            "--reranker",
            "onnx",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run cqs eval");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        result.status.success(),
        "real onnx eval must complete. stdout={stdout} stderr={stderr}"
    );
    // A real reranker run must not surface the per-query rerank-failure warn —
    // that would mean the model failed to load and every query silently
    // degraded to retrieval-only.
    assert!(
        !stderr.contains("Reranking failed"),
        "real reranker must load and rerank without error. stderr={stderr}"
    );

    // Report shape (values are noise on a 3-chunk toy corpus): stdout parses as
    // JSON and carries the eval report's aggregate fields, whether wrapped in
    // the v1 envelope's `data` or emitted bare.
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be JSON: {e}\n{stdout}"));
    let report = parsed.get("data").unwrap_or(&parsed);
    assert!(
        report
            .get("overall")
            .and_then(|o| o.get("r_at_1"))
            .is_some(),
        "report must carry overall.r_at_1. parsed={parsed}"
    );
    assert!(
        report.get("query_count").is_some(),
        "report must carry query_count. parsed={parsed}"
    );
}
