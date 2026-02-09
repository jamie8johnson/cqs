//! CLI integration tests
//!
//! End-to-end tests for the cqs command-line interface.
//!
//! Tests that access the ML model are serialized to prevent HuggingFace Hub
//! lock contention in CI environments.

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

/// Get a Command for the cqs binary
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Create a temporary directory with a sample Rust file
fn setup_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src_dir = dir.path().join("src");
    fs::create_dir(&src_dir).expect("Failed to create src dir");
    fs::write(
        src_dir.join("lib.rs"),
        r#"
/// Adds two numbers
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Subtracts b from a
pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}
"#,
    )
    .expect("Failed to write test file");
    dir
}

#[test]
fn test_help_output() {
    cqs()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Semantic code search"));
}

#[test]
fn test_version_output() {
    cqs()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("cqs"));
}

#[test]
#[serial]
fn test_init_creates_cqs_directory() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Created .cqs/"));

    assert!(
        dir.path().join(".cqs").exists(),
        ".cqs directory should exist"
    );
}

#[test]
#[serial]
fn test_init_idempotent() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    // First init
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Second init should also succeed
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn test_stats_requires_init() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["stats"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
#[serial]
fn test_stats_shows_counts() {
    let dir = setup_project();

    // Initialize
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Index
    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Stats should show chunk count
    cqs()
        .args(["stats"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Total chunks:"));
}

#[test]
#[serial]
fn test_index_auto_initializes() {
    // Index command auto-creates .cqs if it doesn't exist
    let dir = setup_project();

    assert!(
        !dir.path().join(".cqs").exists(),
        ".cqs should not exist before index"
    );

    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Index complete"));

    assert!(
        dir.path().join(".cqs").exists(),
        ".cqs should exist after index"
    );
}

#[test]
#[serial]
fn test_index_parses_files() {
    let dir = setup_project();

    // Initialize
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Index should succeed
    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Index complete"));
}

#[test]
#[serial]
fn test_search_returns_results() {
    let dir = setup_project();

    // Initialize and index
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Search for "add" - should find the add function
    cqs()
        .args(["add numbers"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("add"));
}

#[test]
#[serial]
fn test_search_json_output() {
    let dir = setup_project();

    // Initialize and index
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Search with JSON output
    cqs()
        .args(["--json", "add numbers"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\""));
}

#[test]
fn test_completions_generates_script() {
    cqs()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"));
}

#[test]
fn test_invalid_option_fails() {
    cqs()
        .args(["--invalid-option-xyz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

// =============================================================================
// Doctor command tests
// =============================================================================

#[test]
#[serial]
fn test_doctor_runs() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
#[serial]
fn test_doctor_shows_runtime() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Runtime"));
}

#[test]
#[serial]
fn test_doctor_shows_parser() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Parser"));
}

// =============================================================================
// Call graph command tests (callers/callees)
// =============================================================================

#[test]
fn test_callers_no_index() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["callers", "some_function"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

#[test]
fn test_callees_no_index() {
    let dir = TempDir::new().expect("Failed to create temp dir");

    cqs()
        .args(["callees", "some_function"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Index")));
}

#[test]
#[serial]
fn test_callers_json_output() {
    let dir = setup_project();

    // Initialize and index first
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();

    // callers with --json should return valid JSON (even if empty)
    let output = cqs()
        .args(["callers", "add", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Parse stdout to verify it's valid JSON
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON output: {} — raw: {}", e, stdout));
    assert!(parsed.is_array(), "callers --json should return array");
}

#[test]
#[serial]
fn test_callees_json_output() {
    let dir = setup_project();

    // Initialize and index first
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();

    // callees with --json should return valid JSON
    let output = cqs()
        .args(["callees", "add", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON output: {} — raw: {}", e, stdout));
    assert!(parsed.is_object(), "callees --json should return object");
    assert!(parsed["function"].is_string(), "Should have function field");
}
