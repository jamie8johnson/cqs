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

    // Verify top-level fields exist (under data envelope)
    assert!(
        parsed["data"]["entry_point"].is_object(),
        "onboard --json should have data.entry_point object"
    );
    assert!(
        parsed["data"]["call_chain"].is_array(),
        "onboard --json should have data.call_chain array"
    );
    assert!(
        parsed["data"]["callers"].is_array(),
        "onboard --json should have data.callers array"
    );
    assert!(
        parsed["data"]["key_types"].is_array(),
        "onboard --json should have data.key_types array"
    );
    assert!(
        parsed["data"]["tests"].is_array(),
        "onboard --json should have data.tests array"
    );
    assert!(
        parsed["data"]["summary"].is_object(),
        "onboard --json should have data.summary object"
    );

    // Verify summary has callee_depth as a number
    assert!(
        parsed["data"]["summary"]["callee_depth"].is_number(),
        "data.summary.callee_depth should be a number, got: {}",
        parsed["data"]["summary"]["callee_depth"]
    );

    // Verify entry_point has expected fields
    let entry = &parsed["data"]["entry_point"];
    assert!(entry["name"].is_string(), "entry_point should have name");
    assert!(entry["file"].is_string(), "entry_point should have file");
    assert!(
        entry["line_start"].is_number(),
        "entry_point should have line_start"
    );

    // Verify concept is stored
    assert!(
        parsed["data"]["concept"].is_string(),
        "onboard --json should have data.concept field"
    );

    // --- Content assertions (issue #974) ---
    // The fixture has: test_process → process_data → {validate, format_output}.
    // Verify the retrieval actually names the right chunks, not just field shape.

    assert_eq!(
        parsed["data"]["entry_point"]["name"].as_str(),
        Some("process_data"),
        "entry_point.name should be 'process_data' for query 'process data', got: {}",
        parsed["data"]["entry_point"]["name"]
    );

    let call_chain = parsed["data"]["call_chain"]
        .as_array()
        .expect("data.call_chain should be an array");
    let chain_names: Vec<&str> = call_chain
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(
        chain_names.contains(&"validate") || chain_names.contains(&"format_output"),
        "call_chain should contain 'validate' or 'format_output', got: {:?}",
        chain_names
    );

    let callers = parsed["data"]["callers"]
        .as_array()
        .expect("data.callers should be an array");
    let caller_names: Vec<&str> = callers.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(
        caller_names.contains(&"test_process"),
        "callers should contain 'test_process' (the only caller of process_data in the fixture), got: {:?}",
        caller_names
    );

    let tests = parsed["data"]["tests"]
        .as_array()
        .expect("data.tests should be an array");
    let test_names: Vec<&str> = tests.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        test_names.contains(&"test_process"),
        "tests should contain 'test_process', got: {:?}",
        test_names
    );
}

/// Verify that `--depth 1` limits the BFS expansion: the call_chain should
/// contain no chunks deeper than depth 1 (at most a single hop from the entry).
#[test]
#[serial]
fn test_onboard_depth_limits_chain() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["onboard", "process_data", "--depth", "1", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, stdout));

    let call_chain = parsed["data"]["call_chain"]
        .as_array()
        .expect("data.call_chain should be an array");

    // With --depth 1, every returned callee must be at depth <= 1.
    // (The entry point is not in call_chain; it has its own field.)
    for entry in call_chain {
        let depth = entry["depth"]
            .as_u64()
            .unwrap_or_else(|| panic!("call_chain entry missing numeric depth: {}", entry));
        assert!(
            depth <= 1,
            "With --depth 1, all call_chain entries should have depth <= 1, got depth {} for entry {}",
            depth,
            entry["name"]
        );
    }
}

/// Verify text and JSON output identify the same entry point. If they diverge,
/// one of the rendering paths has drifted from the underlying retrieval.
#[test]
#[serial]
fn test_onboard_text_matches_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // JSON output — extract entry_point.name
    let json_output = cqs()
        .args(["onboard", "process_data", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();
    let json_stdout = String::from_utf8(json_output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(json_stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} -- raw: {}", e, json_stdout));
    let json_entry = parsed["data"]["entry_point"]["name"]
        .as_str()
        .expect("JSON output should have data.entry_point.name string")
        .to_string();
    assert_eq!(
        json_entry, "process_data",
        "JSON entry should be process_data, got: {}",
        json_entry
    );

    // Text output — should mention the same entry point name
    let text_output = cqs()
        .args(["onboard", "process_data"])
        .current_dir(dir.path())
        .assert()
        .success();
    let text_stdout = String::from_utf8(text_output.get_output().stdout.clone()).unwrap();
    assert!(
        text_stdout.contains(&json_entry),
        "Text output should contain entry_point name '{}' (from JSON), but text was:\n{}",
        json_entry,
        text_stdout
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
