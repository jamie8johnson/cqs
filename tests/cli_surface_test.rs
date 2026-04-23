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

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

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
