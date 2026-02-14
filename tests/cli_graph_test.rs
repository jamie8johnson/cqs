//! CLI integration tests for call-graph and utility commands
//!
//! Tests commands that need inter-function call relationships (trace, impact,
//! test-map, context, gather, explain, similar) and standalone commands
//! (audit-mode, notes, project, read).
//!
//! Graph tests use a richer fixture with call chains:
//!   src/lib.rs:  main() → process(), process() → validate(), process() → transform()
//!   src/tests.rs: test_process() → process()

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

/// Create a project with call relationships for graph command testing.
fn setup_graph_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Entry point
pub fn main() {
    let data = process(42);
    println!("{}", data);
}

/// Process input through validation and transformation
pub fn process(input: i32) -> String {
    let valid = validate(input);
    if valid {
        transform(input)
    } else {
        String::from("invalid")
    }
}

/// Check if input is positive
fn validate(input: i32) -> bool {
    input > 0
}

/// Double and format the input
fn transform(input: i32) -> String {
    format!("result: {}", input * 2)
}
"#,
    )
    .expect("Failed to write lib.rs");

    fs::write(
        src.join("tests.rs"),
        r#"
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        let result = process(5);
        assert_eq!(result, "result: 10");
    }
}
"#,
    )
    .expect("Failed to write tests.rs");

    dir
}

/// Initialize and index a project, returning the TempDir.
/// Must be called inside a #[serial] test.
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

// =============================================================================
// Tier 1: No model needed — standalone commands
// =============================================================================

#[test]
fn test_audit_mode_on() {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("Failed to create .cqs dir");

    cqs()
        .args(["audit-mode", "on", "--expires", "30m"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Audit mode enabled"));
}

#[test]
fn test_audit_mode_off() {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("Failed to create .cqs dir");

    // Turn on first
    cqs()
        .args(["audit-mode", "on", "--expires", "30m"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Turn off
    cqs()
        .args(["audit-mode", "off"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Audit mode disabled"));
}

#[test]
fn test_audit_mode_query_status() {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("Failed to create .cqs dir");

    // Query when off
    cqs()
        .args(["audit-mode"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Audit mode: OFF"));
}

#[test]
fn test_audit_mode_json() {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("Failed to create .cqs dir");

    let output = cqs()
        .args(["audit-mode", "on", "--expires", "1h", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));
    assert_eq!(parsed["audit_mode"], true);
    assert!(parsed["expires_at"].is_string());
}

#[test]
fn test_audit_mode_invalid_state() {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("Failed to create .cqs dir");

    cqs()
        .args(["audit-mode", "maybe"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid state"));
}

#[test]
#[serial]
fn test_project_register_list_remove() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Register
    cqs()
        .args([
            "project",
            "register",
            "testproj",
            dir.path().to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Registered 'testproj'"));

    // List
    cqs()
        .args(["project", "list"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("testproj"));

    // Remove
    cqs()
        .args(["project", "remove", "testproj"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed 'testproj'"));
}

#[test]
#[serial]
fn test_project_remove_nonexistent() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["project", "remove", "nosuchproject"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("not found"));
}

// =============================================================================
// Tier 2: Real init+index — graph and search commands
// =============================================================================

#[test]
#[serial]
fn test_trace_finds_path() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["trace", "main", "validate", "--format", "json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    assert_eq!(parsed["source"], "main");
    assert_eq!(parsed["target"], "validate");
    let path = parsed["path"].as_array().expect("path should be array");
    assert!(path.len() >= 2, "Path should have at least 2 hops");

    // Verify path starts with main and ends with validate
    assert_eq!(path[0]["name"], "main");
    assert_eq!(path[path.len() - 1]["name"], "validate");
}

#[test]
#[serial]
fn test_trace_trivial_self() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["trace", "main", "main"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("same function"));
}

#[test]
#[serial]
fn test_trace_no_path() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // validate doesn't call main — no reverse path
    cqs()
        .args(["trace", "validate", "main"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No call path found"));
}

#[test]
#[serial]
fn test_impact_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["impact", "validate", "--format", "json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    // validate is called by process, which is called by main
    assert!(parsed["function"].is_string(), "Should have function field");
}

#[test]
#[serial]
fn test_impact_text_output() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["impact", "validate"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("validate"));
}

#[test]
#[serial]
fn test_test_map_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["test-map", "process", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    assert!(parsed["function"].is_string(), "Should have function field");
    assert!(parsed["tests"].is_array(), "Should have tests array");
}

#[test]
#[serial]
fn test_test_map_transitive() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // validate is called by process, which is called by test_process
    let output = cqs()
        .args(["test-map", "validate", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    assert!(parsed["function"].is_string(), "Should have function field");
}

#[test]
#[serial]
fn test_context_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["context", "src/lib.rs", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    assert_eq!(parsed["file"], "src/lib.rs");
    let chunks = parsed["chunks"]
        .as_array()
        .expect("Should have chunks array");
    assert!(
        chunks.len() >= 4,
        "Should have at least 4 chunks (main, process, validate, transform)"
    );
}

#[test]
#[serial]
fn test_context_summary() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["context", "src/lib.rs", "--summary"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Chunks:"));
}

#[test]
#[serial]
fn test_context_nonexistent_file() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["context", "src/nonexistent.rs"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No indexed chunks"));
}

#[test]
#[serial]
fn test_explain_text() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["explain", "process"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("process")
                .and(predicate::str::contains("Callers:").or(predicate::str::contains("Callees:"))),
        );
}

#[test]
#[serial]
fn test_explain_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["explain", "process", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    assert!(parsed["name"].is_string(), "Should have name field");
    assert!(parsed["callers"].is_array(), "Should have callers array");
    assert!(parsed["callees"].is_array(), "Should have callees array");
    assert!(parsed["signature"].is_string(), "Should have signature");
}

#[test]
#[serial]
fn test_explain_nonexistent() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["explain", "nonexistent_function_xyz"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No function found"));
}

#[test]
#[serial]
fn test_similar_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // similar to process — should find other functions
    let output = cqs()
        .args(["similar", "process", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Should be valid JSON (either results or empty)
    let _parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));
}

#[test]
#[serial]
fn test_gather_json() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["gather", "process data", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    assert_eq!(parsed["query"], "process data");
    assert!(parsed["chunks"].is_array(), "Should have chunks array");

    // Verify language/chunk_type in JSON output
    if let Some(chunks) = parsed["chunks"].as_array() {
        for chunk_json in chunks {
            assert!(
                chunk_json.get("language").is_some(),
                "JSON should include language"
            );
            assert!(
                chunk_json.get("chunk_type").is_some(),
                "JSON should include chunk_type"
            );
        }
    }
}

#[test]
#[serial]
fn test_read_file() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["read", "src/lib.rs"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("pub fn main()"));
}

#[test]
#[serial]
fn test_read_nonexistent() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["read", "src/nope.rs"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("File not found"));
}

#[test]
#[serial]
fn test_read_focused() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    cqs()
        .args(["read", "src/lib.rs", "--focus", "process"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Focused read:"));
}

#[test]
#[serial]
fn test_notes_add_list_remove() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Create docs directory for notes.toml
    let docs_dir = dir.path().join("docs");
    fs::create_dir_all(&docs_dir).expect("Failed to create docs dir");

    // Add
    cqs()
        .args([
            "notes",
            "add",
            "test note for CLI",
            "--sentiment",
            "0.5",
            "--mentions",
            "lib.rs",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    // List
    cqs()
        .args(["notes", "list"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("test note for CLI"));

    // Remove
    cqs()
        .args(["notes", "remove", "test note for CLI", "--no-reindex"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
#[serial]
fn test_notes_warnings_filter() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let docs_dir = dir.path().join("docs");
    fs::create_dir_all(&docs_dir).expect("Failed to create docs dir");

    // Add a warning note
    cqs()
        .args([
            "notes",
            "add",
            "this is a warning",
            "--sentiment",
            "-0.5",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    // List with --warnings filter
    cqs()
        .args(["notes", "list", "--warnings"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("this is a warning"));
}
