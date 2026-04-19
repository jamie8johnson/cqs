//! Audit P2 #44 — `cmd_blame` integration tests.
//!
//! `cqs blame <target>` resolves a function via `resolve_target`, builds a
//! `BlameData` (which spawns `git log` for each chunk's line range), and
//! emits either pretty terminal output or a JSON envelope. The inline tests
//! in `src/cli/commands/io/blame.rs` only cover `parse_git_log_output` and
//! the JSON-shape stub on a hand-built `BlameData` — nothing pins the path
//! from CLI argv → store query → git log subprocess → printed envelope.
//!
//! These tests use the subprocess pattern (`assert_cmd::Command::cargo_bin`)
//! because blame requires a live store + a real `git` invocation, neither of
//! which can be in-process. They are gated behind `slow-tests` because they
//! cold-load the embedder during `cqs index`.

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Set up a tiny git repo with a single Rust source file containing a
/// known function, run `git init` + commit + `cqs init` + `cqs index`.
/// Blame needs `git log` to find at least one commit touching the
/// function's line range; without a commit the blame entry's `commits`
/// array is empty (still a valid envelope).
fn setup_blame_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .expect("Failed to write lib.rs");

    // git init + commit so `git log -L` has something to report.
    StdCommand::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init failed");
    StdCommand::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .status()
        .expect("git config user.email failed");
    StdCommand::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir.path())
        .status()
        .expect("git config user.name failed");
    StdCommand::new("git")
        .args(["add", "src/lib.rs"])
        .current_dir(dir.path())
        .status()
        .expect("git add failed");
    StdCommand::new("git")
        .args(["commit", "-q", "-m", "initial: add `add`"])
        .current_dir(dir.path())
        .status()
        .expect("git commit failed");

    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();
    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Index complete"));

    dir
}

#[test]
#[serial]
fn test_blame_json_emits_envelope_for_known_function() {
    let dir = setup_blame_project();

    let output = cqs()
        .args(["blame", "add", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs blame");

    assert!(
        output.status.success(),
        "blame should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    // Envelope shape
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert!(parsed["data"].is_object(), "data must be object: {stdout}");

    // BlameOutput inner shape
    assert_eq!(parsed["data"]["name"], "add");
    assert!(parsed["data"]["file"].is_string(), "file must be a string");
    assert!(
        parsed["data"]["line_start"].is_number(),
        "line_start must be numeric"
    );
    assert!(
        parsed["data"]["line_end"].is_number(),
        "line_end must be numeric"
    );
    assert!(
        parsed["data"]["signature"].is_string(),
        "signature must be a string"
    );
    let commits = parsed["data"]["commits"]
        .as_array()
        .expect("commits must be an array");
    // commits may be empty if `git log -L` doesn't find any history for the
    // exact line range — accept either populated or empty, but the shape is
    // pinned. The total_commits field must equal the array length.
    assert_eq!(
        parsed["data"]["total_commits"].as_u64().unwrap_or(99),
        commits.len() as u64,
        "total_commits must equal commits.len()"
    );
}

#[test]
#[serial]
fn test_blame_text_output_includes_target_name() {
    let dir = setup_blame_project();

    let output = cqs()
        .args(["blame", "add"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs blame");

    assert!(
        output.status.success(),
        "blame text mode should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("add"),
        "text output should mention the target function name 'add'. got: {stdout}"
    );
}

#[test]
#[serial]
fn test_blame_unknown_target_fails_gracefully() {
    let dir = setup_blame_project();

    let output = cqs()
        .args(["blame", "this_function_definitely_does_not_exist_xyz"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs blame");

    assert!(
        !output.status.success(),
        "blame on unknown target should exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Error should be informative — not a panic and not silent.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "should print an error message on unknown target"
    );
    // Resolution failure flows through `Failed to resolve blame target`
    // (anyhow context in `build_blame_data`). Don't bind the exact phrasing
    // — just confirm the message references the target or the failure mode.
    assert!(
        stderr.to_lowercase().contains("blame")
            || stderr.to_lowercase().contains("resolve")
            || stderr.to_lowercase().contains("not found")
            || stderr.to_lowercase().contains("no function")
            || stderr.contains("this_function_definitely_does_not_exist_xyz"),
        "stderr should explain the failure. got: {stderr}"
    );
}

/// `--show-callers` produces an envelope with the `callers` field populated
/// when the target has callers (or omitted via `serde(skip_serializing_if)`
/// when empty). For a single-function fixture with no callers this exercises
/// the empty-callers branch — pins that the field is not emitted as `null`.
#[test]
#[serial]
fn test_blame_show_callers_for_no_caller_target_omits_callers_field() {
    let dir = setup_blame_project();

    let output = cqs()
        .args(["blame", "add", "--callers", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs blame --callers");

    assert!(
        output.status.success(),
        "blame --callers should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // No callers in the fixture (only `add` exists) → the BlameOutput field is
    // `Vec::is_empty` skipped. Confirm the field is either absent or empty —
    // both are correct contracts; what matters is it's not `null` (which
    // would break agent parsing).
    let callers = parsed["data"].get("callers");
    if let Some(c) = callers {
        assert!(
            c.is_array(),
            "callers must be an array if present, got: {c}"
        );
        assert!(
            c.as_array().unwrap().is_empty(),
            "callers must be empty for the no-caller fixture, got: {c}"
        );
    }
    // Else: field was correctly skipped via `serde(skip_serializing_if = "Vec::is_empty")`.
}
