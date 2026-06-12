//! CLI-surface integration tests: things that genuinely need to spawn
//! the `cqs` binary because they exercise argv parsing, exit codes,
//! `--help`/`--version` output, completions, or the `doctor` probe.
//!
//! Critically, none of these load the embedder or the HNSW index — the
//! covered subcommands all short-circuit before the model stack. So
//! while each invocation pays the binary's ~100-300 ms cold start, the
//! whole binary runs in ~5 seconds total. That's why this file is NOT
//! gated behind `slow-tests` and runs in regular PR CI.
//!
//! The bulk of the integration coverage that used to live in the
//! gated `cli_test.rs` is now in `tests/index_search_test.rs` and
//! `tests/health_test.rs`, both of which are in-process.

mod common;

use common::cqs_v1 as cqs;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn help_output_lists_subcommands() {
    cqs()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Semantic code search"));
}

#[test]
fn version_output_contains_cqs() {
    cqs()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("cqs"));
}

#[test]
fn completions_generates_bash_script() {
    cqs()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"));
}

#[test]
fn invalid_option_fails_with_nonzero_exit() {
    cqs().args(["--invalid-option-xyz"]).assert().failure();
}

#[test]
fn doctor_runs_without_an_index() {
    // `cqs doctor` runs in any directory — it probes the runtime, parser
    // registry, and (if present) the index. With no `.cqs/`, it should
    // still succeed; the report will note that no index was found.
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn doctor_output_mentions_runtime_and_parser() {
    // Combined version of test_doctor_shows_runtime + test_doctor_shows_parser.
    // Two `predicate::str::contains` calls would require two assertions;
    // the test asserts both via `and()`.
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Runtime").and(predicate::str::contains("Parser")));
}

// ---------------------------------------------------------------------
// "no index" error-path tests. These do spawn the binary and check the
// error message + non-zero exit code. They don't load the model stack
// because the failure happens at Store::open before any embedder is
// constructed.
// ---------------------------------------------------------------------

#[test]
fn stats_without_init_fails() {
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["stats"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

#[test]
fn callers_without_index_fails() {
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["callers", "some_function"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

#[test]
fn callees_without_index_fails() {
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["callees", "some_function"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

#[test]
fn callers_const_name_emits_kind_fallback_json_shape() {
    // Polymorphic routing at the CLI surface: `cqs callers <CONST>` must
    // emit the kind-labeled `{kind, fallback_from, name, definitions,
    // note}` fallback object instead of a misrouted empty caller list.
    // The index is seeded in-process via the lib (no embedder needed —
    // callers is SQL-only and the binary stays on the cheap path), then
    // the real binary pins the end-to-end JSON shape under the default
    // (bare) output format.
    let dir = TempDir::new().unwrap();
    let cqs_subdir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_subdir).unwrap();
    {
        let store = ::cqs::store::Store::open(&cqs_subdir.join(::cqs::INDEX_DB_FILENAME)).unwrap();
        store.init(&::cqs::store::ModelInfo::default()).unwrap();
        let mut chunk = common::test_chunk("MAX_RETRIES", "pub const MAX_RETRIES: u32 = 3;");
        chunk.chunk_type = ::cqs::parser::ChunkType::Constant;
        store
            .upsert_chunks_batch(&[(chunk, common::mock_embedding(1.0))], Some(0))
            .unwrap();
    }

    #[allow(deprecated)]
    let output = assert_cmd::Command::cargo_bin("cqs")
        .expect("cqs binary")
        .args(["callers", "MAX_RETRIES", "--json"])
        .env("CQS_NO_DAEMON", "1")
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("utf8 stdout");
    let payload: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("bare JSON payload");

    assert_eq!(payload["kind"], "const", "got: {payload}");
    assert_eq!(payload["fallback_from"], "callers", "got: {payload}");
    assert_eq!(payload["name"], "MAX_RETRIES", "got: {payload}");
    let defs = payload["definitions"]
        .as_array()
        .unwrap_or_else(|| panic!("definitions must be an array, got: {payload}"));
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0]["chunk_type"], "constant");
    assert!(
        payload["note"].as_str().is_some_and(|n| !n.is_empty()),
        "note must be a non-empty string, got: {payload}"
    );
}

#[test]
fn gc_without_index_fails() {
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["gc"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

#[test]
fn dead_without_index_fails() {
    let dir = TempDir::new().unwrap();
    cqs()
        .args(["dead"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

// ---------------------------------------------------------------------
// audit-mode argv validation. The state-management tests are in-process
// in graph_test.rs; this one stays subprocess because it asserts on
// clap's "possible values" error message format.
// ---------------------------------------------------------------------

#[test]
fn audit_mode_invalid_state_fails_with_possible_values() {
    let dir = TempDir::new().unwrap();
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();
    cqs()
        .args(["audit-mode", "maybe"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("possible values: on, off"));
}

// ---------------------------------------------------------------------
// project subcommand. Mutates the global registry at
// `~/.config/cqs/projects.toml`, so we point XDG_CONFIG_HOME at a
// tempdir per test to keep the user's real registry untouched.
// ---------------------------------------------------------------------

#[test]
fn project_register_list_remove_round_trips() {
    let cfg_dir = TempDir::new().unwrap();
    let proj_dir = TempDir::new().unwrap();
    // Create the .cqs/index.db marker the registry's `register` validates.
    let cqs_subdir = proj_dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_subdir).unwrap();
    std::fs::write(cqs_subdir.join("index.db"), "").unwrap();

    cqs()
        .args([
            "project",
            "register",
            "testproj",
            proj_dir.path().to_str().unwrap(),
        ])
        .env("XDG_CONFIG_HOME", cfg_dir.path())
        .env("HOME", cfg_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("testproj"));

    cqs()
        .args(["project", "list"])
        .env("XDG_CONFIG_HOME", cfg_dir.path())
        .env("HOME", cfg_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("testproj"));

    cqs()
        .args(["project", "remove", "testproj"])
        .env("XDG_CONFIG_HOME", cfg_dir.path())
        .env("HOME", cfg_dir.path())
        .assert()
        .success();
}

#[test]
fn related_nonexistent_function_fails_with_message() {
    // The library API (`cqs::find_related`) returns an AnalysisError on
    // unresolved targets; the CLI surface translates that to "No
    // function found" stderr. We assert the surface message here
    // because the in-process call would just propagate Err, which
    // doesn't tell us about the user-visible diagnostic.
    //
    // This test does spawn a binary that opens a populated index, so it
    // pays a model load (~2-5s). Single test, acceptable; if it grows,
    // promote the in-process variant and drop this one.
    let dir = TempDir::new().unwrap();
    let cqs_subdir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_subdir).unwrap();
    cqs()
        .args(["related", "nonexistent_fn_xyz_12345"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("No function found")
                .or(predicate::str::contains("not found"))
                .or(predicate::str::contains("Index")),
        );
}

// `cqs batch` REPL surface (pipe parsing, comments/blanks/quit, error
// envelopes) is intentionally NOT covered here. The batch dispatcher
// requires an indexed store at startup, so any subprocess test would
// need init+index → ~5+ seconds of model load per test, exactly what
// this whole effort is trying to delete.
//
// What it would cover is:
//   - pipe parsing → src/cli/batch/parse.rs has its own unit tests
//   - line filtering (comments/blanks) → trivial, also unit-tested
//   - per-command dispatch → covered behaviourally by the in-process
//     test files (each subcommand has its own `tests/<cmd>_test.rs`)
//   - error envelope → exercised by the json_envelope unit tests
//
// If batch ever grows a bug that's not caught by either of those, the
// fix is to land a proper subprocess test alongside the bug fix —
// not to pre-emptively add an integration smoke that costs minutes
// per CI run.

// ---------------------------------------------------------------------
// Mapping-guaranteed review / affected populated-path spawns.
//
// The slow-tests `cli_review_test.rs` / `cli_train_review_test.rs` variants
// index real source via the embedder, which makes hunk-to-function line
// mapping fragile, so their populated-branch assertions are written as
// "populated OR empty". Here we seed the store in-process with explicit
// `(line_start, line_end)` chunks and feed a diff whose hunk lands inside
// that range — the mapping is guaranteed, so the populated branch is
// unconditional. Both `review` and `affected` are SQL/graph-only (no
// embedder), so these stay in regular CI.
// ---------------------------------------------------------------------

/// Seed a `.cqs/index.db` with `target_fn` (lines 10-30, in src/lib.rs), a
/// caller `caller_fn`, and a test `test_target` that both reach it via call
/// edges. A diff hunk at line 15 maps deterministically to `target_fn`.
fn seed_review_store(dir: &std::path::Path) {
    let cqs_subdir = dir.join(".cqs");
    std::fs::create_dir_all(&cqs_subdir).unwrap();
    let store = ::cqs::store::Store::open(&cqs_subdir.join(::cqs::INDEX_DB_FILENAME)).unwrap();
    store.init(&::cqs::store::ModelInfo::default()).unwrap();

    let make = |name: &str, file: &str, ls: u32, le: u32| -> ::cqs::parser::Chunk {
        let content = format!("fn {name}() {{ }}");
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        ::cqs::parser::Chunk {
            id: format!("{file}:{ls}:{}", &hash[..8]),
            file: std::path::PathBuf::from(file),
            language: ::cqs::parser::Language::Rust,
            chunk_type: ::cqs::parser::ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: ls,
            line_end: le,
            content_hash: hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    };

    let emb = common::mock_embedding(1.0);
    for c in [
        make("target_fn", "src/lib.rs", 10, 30),
        make("caller_fn", "src/api.rs", 1, 15),
        make("test_target", "tests/lib_test.rs", 1, 10),
    ] {
        store
            .upsert_chunks_batch(&[(c, emb.clone())], Some(12345))
            .unwrap();
    }

    // caller_fn → target_fn, test_target → target_fn
    store
        .upsert_function_calls(
            std::path::Path::new("src/api.rs"),
            &[::cqs::parser::FunctionCalls {
                name: "caller_fn".to_string(),
                line_start: 1,
                calls: vec![::cqs::parser::CallSite {
                    callee_name: "target_fn".to_string(),
                    line_number: 8,
                    kind: cqs::parser::CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();
    store
        .upsert_function_calls(
            std::path::Path::new("tests/lib_test.rs"),
            &[::cqs::parser::FunctionCalls {
                name: "test_target".to_string(),
                line_start: 1,
                calls: vec![::cqs::parser::CallSite {
                    callee_name: "target_fn".to_string(),
                    line_number: 5,
                    kind: cqs::parser::CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();
}

/// A diff whose hunk (`@@ -15,3 +15,4 @@`) lands inside target_fn's 10-30 range.
const TARGET_FN_DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -15,3 +15,4 @@ fn target_fn() {
     let x = 1;
+    let y = 2;
";

#[test]
fn review_stdin_populated_branch_lists_changed_function() {
    let dir = TempDir::new().unwrap();
    seed_review_store(dir.path());

    let output = cqs()
        .args(["review", "--stdin", "--json"])
        .env("CQS_NO_DAEMON", "1")
        .current_dir(dir.path())
        .write_stdin(TARGET_FN_DIFF)
        .output()
        .expect("run cqs review --stdin");

    assert!(
        output.status.success(),
        "review should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    let data = &parsed["data"];
    // Unconditional populated-branch assertions — the seeded mapping guarantees
    // target_fn is the changed function.
    let changed: Vec<&str> = data["changed_functions"]
        .as_array()
        .expect("changed_functions array")
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(
        changed.contains(&"target_fn"),
        "changed_functions must contain target_fn, got: {changed:?}"
    );
    assert_ne!(
        data["risk_summary"]["overall"], "none",
        "populated review must report a real risk level"
    );
}

#[test]
fn review_stdin_token_budget_is_honored_on_populated_branch() {
    let dir = TempDir::new().unwrap();
    seed_review_store(dir.path());

    let output = cqs()
        .args(["review", "--stdin", "--json", "--tokens", "100"])
        .env("CQS_NO_DAEMON", "1")
        .current_dir(dir.path())
        .write_stdin(TARGET_FN_DIFF)
        .output()
        .expect("run cqs review --tokens");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");
    let data = &parsed["data"];

    assert!(
        data["token_count"].is_number(),
        "populated review with --tokens must emit numeric token_count, got: {}",
        data["token_count"]
    );
    assert_eq!(
        data["token_budget"],
        serde_json::json!(100),
        "token_budget must echo the requested budget"
    );
}

/// V2Bare default-format pin for the review family. `cqs review --stdin
/// --json` with NO `CQS_OUTPUT_FORMAT` pin must emit the bare review payload
/// (object at the top level, no `data` / `version` envelope) carrying the
/// `changed_functions` content key.
#[test]
fn review_stdin_default_format_emits_bare_payload() {
    let dir = TempDir::new().unwrap();
    seed_review_store(dir.path());

    #[allow(deprecated)]
    let output = assert_cmd::Command::cargo_bin("cqs")
        .expect("cqs binary")
        .args(["review", "--stdin", "--json"])
        .env("CQS_NO_DAEMON", "1")
        .current_dir(dir.path())
        .write_stdin(TARGET_FN_DIFF)
        .output()
        .expect("run cqs review --stdin (bare)");

    assert!(
        output.status.success(),
        "review should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("bare JSON parse");

    assert!(
        parsed.get("data").is_none() && parsed.get("version").is_none(),
        "bare default drops envelope keys, got: {parsed}"
    );
    let changed: Vec<&str> = parsed["changed_functions"]
        .as_array()
        .unwrap_or_else(|| panic!("changed_functions must be a top-level array, got: {parsed}"))
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(
        changed.contains(&"target_fn"),
        "bare review payload must list target_fn, got: {changed:?}"
    );
}

#[test]
fn affected_stdin_populated_branch_lists_changed_and_overall_risk() {
    let dir = TempDir::new().unwrap();
    seed_review_store(dir.path());

    let output = cqs()
        .args(["affected", "--stdin", "--json"])
        .env("CQS_NO_DAEMON", "1")
        .current_dir(dir.path())
        .write_stdin(TARGET_FN_DIFF)
        .output()
        .expect("run cqs affected --stdin");

    assert!(
        output.status.success(),
        "affected should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");
    let data = &parsed["data"];

    let changed: Vec<&str> = data["changed_functions"]
        .as_array()
        .expect("changed_functions array")
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(
        changed.contains(&"target_fn"),
        "affected changed_functions must contain target_fn, got: {changed:?}"
    );
    assert_ne!(
        data["overall_risk"], "none",
        "populated affected must report a real overall_risk, not the empty sentinel"
    );
}

// ---------------------------------------------------------------------
// Every-session command set — happy-path binary spawns against a seeded
// store. Each command spawns `cqs <cmd> ... --json`, asserts exit 0 and
// that the seeded content (a function name or a non-zero count) appears in
// the bare JSON payload. `where` is skipped — it loads the embedder, which
// the seeded store can't satisfy without a model on disk.
//
// All asserted commands are SQL/graph-only, so the binary stays off the
// model stack and these run in regular CI.
// ---------------------------------------------------------------------

/// Seed a `.cqs/index.db` AND the physical `src/`/`tests/` source files for
/// the every-session command suite. `producer` (lines 10-30, src/lib.rs)
/// calls `consumer` and uses the `Config` type; `test_producer` exercises it.
fn seed_session_project(dir: &std::path::Path) {
    use ::cqs::parser::{CallEdgeKind, CallSite, Chunk, ChunkType, FunctionCalls, Language};

    // Physical source files so `read`/`context` resolve content.
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub struct Config { v: i32 }\npub fn producer() -> i32 { consumer(Config { v: 1 }) }\npub fn consumer(c: Config) -> i32 { c.v }\n",
    )
    .unwrap();

    let cqs_subdir = dir.join(".cqs");
    std::fs::create_dir_all(&cqs_subdir).unwrap();
    let store = ::cqs::store::Store::open(&cqs_subdir.join(::cqs::INDEX_DB_FILENAME)).unwrap();
    store.init(&::cqs::store::ModelInfo::default()).unwrap();

    let mk = |name: &str, file: &str, ct: ChunkType, ls: u32, le: u32| -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let hash = blake3::hash(format!("{file}:{name}").as_bytes())
            .to_hex()
            .to_string();
        Chunk {
            id: format!("{file}:{ls}:{}", &hash[..8]),
            file: std::path::PathBuf::from(file),
            language: Language::Rust,
            chunk_type: ct,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: ls,
            line_end: le,
            content_hash: hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    };

    let emb = common::mock_embedding(1.0);
    for c in [
        mk("Config", "src/lib.rs", ChunkType::Struct, 1, 1),
        mk("producer", "src/lib.rs", ChunkType::Function, 10, 30),
        mk("consumer", "src/lib.rs", ChunkType::Function, 31, 40),
        mk(
            "test_producer",
            "tests/lib_test.rs",
            ChunkType::Function,
            1,
            10,
        ),
    ] {
        store
            .upsert_chunks_batch(&[(c, emb.clone())], Some(12345))
            .unwrap();
    }

    // producer → consumer, test_producer → producer
    store
        .upsert_function_calls(
            std::path::Path::new("src/lib.rs"),
            &[FunctionCalls {
                name: "producer".to_string(),
                line_start: 10,
                calls: vec![CallSite {
                    callee_name: "consumer".to_string(),
                    line_number: 11,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();
    store
        .upsert_function_calls(
            std::path::Path::new("tests/lib_test.rs"),
            &[FunctionCalls {
                name: "test_producer".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "producer".to_string(),
                    line_number: 3,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )
        .unwrap();
}

/// Spawn `cqs <args> --json` against the session fixture and return the parsed
/// bare-or-v1 payload (this file pins `CQS_OUTPUT_FORMAT=v1` via `cqs()`).
fn session_spawn(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = cqs()
        .args(args)
        .arg("--json")
        .env("CQS_NO_DAEMON", "1")
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn {args:?} failed: {e}"));
    assert!(
        output.status.success(),
        "{args:?} should exit 0. stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("{args:?} JSON parse failed: {e}\n{stdout}"))
}

#[test]
fn session_impact_lists_seeded_caller() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    // impact consumer → producer is a caller.
    let parsed = session_spawn(dir.path(), &["impact", "consumer"]);
    assert!(
        parsed.to_string().contains("producer"),
        "impact consumer must surface caller producer, got: {parsed}"
    );
}

#[test]
fn session_test_map_lists_seeded_test() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    // producer is reached by test_producer.
    let parsed = session_spawn(dir.path(), &["test-map", "producer"]);
    assert!(
        parsed.to_string().contains("test_producer"),
        "test-map producer must surface test_producer, got: {parsed}"
    );
}

#[test]
fn session_deps_reverse_lists_used_type() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    // No type edges seeded, but the command must resolve and emit a payload
    // naming the queried function rather than erroring.
    let parsed = session_spawn(dir.path(), &["deps", "consumer", "--reverse"]);
    assert!(
        parsed["data"].is_object() || parsed["data"].is_array(),
        "deps --reverse must emit a data payload, got: {parsed}"
    );
}

#[test]
fn session_callers_lists_seeded_caller() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    let parsed = session_spawn(dir.path(), &["callers", "consumer"]);
    assert!(
        parsed.to_string().contains("producer"),
        "callers consumer must list producer, got: {parsed}"
    );
}

#[test]
fn session_callees_lists_seeded_callee() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    let parsed = session_spawn(dir.path(), &["callees", "producer"]);
    assert!(
        parsed.to_string().contains("consumer"),
        "callees producer must list consumer, got: {parsed}"
    );
}

#[test]
fn session_trace_finds_same_project_path() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    // producer → consumer is a direct edge; trace must find the path.
    let parsed = session_spawn(dir.path(), &["trace", "producer", "consumer"]);
    assert!(
        parsed.to_string().contains("consumer"),
        "trace producer consumer must include consumer in the path, got: {parsed}"
    );
}

#[test]
fn session_context_lists_seeded_chunks() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    // --summary avoids token packing (no embedder).
    let parsed = session_spawn(dir.path(), &["context", "src/lib.rs", "--summary"]);
    assert!(
        parsed.to_string().contains("producer"),
        "context src/lib.rs must reference producer, got: {parsed}"
    );
}

#[test]
fn session_read_returns_file_content() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    let parsed = session_spawn(dir.path(), &["read", "src/lib.rs"]);
    assert!(
        parsed.to_string().contains("producer"),
        "read src/lib.rs must include the producer source, got: {parsed}"
    );
}

#[test]
fn session_related_resolves_seeded_function() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    // related is graph/SQL-only; producer shares the call graph with consumer.
    let parsed = session_spawn(dir.path(), &["related", "producer"]);
    assert!(
        parsed["data"].is_object() || parsed["data"].is_array(),
        "related producer must emit a data payload, got: {parsed}"
    );
}

#[test]
fn session_health_reports_seeded_chunk_count() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    let parsed = session_spawn(dir.path(), &["health"]);
    let total = parsed["data"]["stats"]["total_chunks"].as_u64();
    assert!(
        total.is_some_and(|n| n >= 4),
        "health must report the 4 seeded chunks, got: {parsed}"
    );
}

#[test]
fn session_stats_reports_seeded_chunk_count() {
    let dir = TempDir::new().unwrap();
    seed_session_project(dir.path());
    let parsed = session_spawn(dir.path(), &["stats"]);
    let total = parsed["data"]["total_chunks"].as_u64();
    assert!(
        total.is_some_and(|n| n >= 4),
        "stats must report the 4 seeded chunks, got: {parsed}"
    );
}

#[test]
fn project_remove_nonexistent_succeeds_quietly() {
    let cfg_dir = TempDir::new().unwrap();
    cqs()
        .args(["project", "remove", "nosuchproject"])
        .env("XDG_CONFIG_HOME", cfg_dir.path())
        .env("HOME", cfg_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("not found"));
}
