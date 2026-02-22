//! Integration tests for the `cqs onboard` command (TC-11)
//!
//! Uses a graph fixture with call relationships:
//!   src/lib.rs:  process_data() -> validate(), process_data() -> format_output()
//!   src/lib.rs:  test_process() -> process_data()

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

/// Get a Command for the cqs binary.
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Create a project with call relationships suitable for onboard testing.
fn setup_graph_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Process incoming data through validation and formatting
pub fn process_data(input: &str) -> String {
    let result = validate(input);
    format_output(&result)
}

/// Validate and trim the input string
fn validate(s: &str) -> String {
    s.trim().to_string()
}

/// Format the output with brackets
fn format_output(s: &str) -> String {
    format!("[{}]", s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        let result = process_data("hello");
        assert_eq!(result, "[hello]");
    }
}
"#,
    )
    .expect("Failed to write lib.rs");

    dir
}

/// Initialize and index a project.
fn init_and_index(dir: &TempDir) {
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
}

#[test]
#[serial]
fn test_onboard_cli_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["onboard", "process data", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    // Verify top-level fields exist
    assert!(
        parsed["entry_point"].is_object(),
        "onboard --json should have entry_point object"
    );
    assert!(
        parsed["call_chain"].is_array(),
        "onboard --json should have call_chain array"
    );
    assert!(
        parsed["callers"].is_array(),
        "onboard --json should have callers array"
    );
    assert!(
        parsed["key_types"].is_array(),
        "onboard --json should have key_types array"
    );
    assert!(
        parsed["tests"].is_array(),
        "onboard --json should have tests array"
    );
    assert!(
        parsed["summary"].is_object(),
        "onboard --json should have summary object"
    );

    // Verify summary has callee_depth as a number
    assert!(
        parsed["summary"]["callee_depth"].is_number(),
        "summary.callee_depth should be a number, got: {}",
        parsed["summary"]["callee_depth"]
    );

    // Verify entry_point has expected fields
    let entry = &parsed["entry_point"];
    assert!(entry["name"].is_string(), "entry_point should have name");
    assert!(entry["file"].is_string(), "entry_point should have file");
    assert!(
        entry["line_start"].is_number(),
        "entry_point should have line_start"
    );

    // Verify concept is stored
    assert!(
        parsed["concept"].is_string(),
        "onboard --json should have concept field"
    );
}

#[test]
#[serial]
fn test_onboard_not_found() {
    // Create a project with no source files — only init, no indexable content.
    // onboard should fail because the index has no chunks to search.
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    // Write an empty file — parser won't extract any chunks from it
    fs::write(src.join("lib.rs"), "// empty\n").expect("Failed to write lib.rs");

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

    // With no chunks in the index, onboard should fail
    cqs()
        .args(["onboard", "anything", "--json"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("No relevant")));
}
