//! CLI integration tests for batch mode
//!
//! Tests `cqs batch` — reads commands from stdin, outputs JSONL.
//! Reuses the graph project fixture from cli_graph_test.rs.

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

/// Create a project with call relationships for batch testing.
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

// =============================================================================
// Batch mode integration tests
// =============================================================================

#[test]
#[serial]
fn test_batch_single_command() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers process\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");
    assert!(parsed.is_array(), "callers should return a JSON array");
}

#[test]
#[serial]
fn test_batch_multiple_commands() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers process\ncallees main\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2, "Should have two JSONL lines");

    // Both lines should be valid JSON
    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("Each line should be valid JSON");
        assert!(parsed.is_array() || parsed.is_object());
    }
}

#[test]
#[serial]
fn test_batch_error_handling() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("unknown_cmd foo\n")
        .output()
        .expect("Failed to run batch");

    assert!(
        output.status.success(),
        "Batch should not crash on bad input"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");
    assert!(
        parsed.get("error").is_some(),
        "Should have error field: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_comments_and_blanks() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("# comment\n\ncallers process\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "Only the callers command should produce output"
    );
}

#[test]
#[serial]
fn test_batch_quit() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers process\nquit\ncallers main\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "Should only output the first command (before quit)"
    );
}

#[test]
#[serial]
fn test_batch_stats() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("stats\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");
    assert!(
        parsed.get("total_chunks").is_some(),
        "Stats should have total_chunks: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_explain() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("explain process\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");
    assert!(
        parsed.get("callers").is_some(),
        "Explain should have callers: {}",
        stdout.trim()
    );
    assert!(
        parsed.get("callees").is_some(),
        "Explain should have callees: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_dead() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("dead\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");
    assert!(
        parsed.get("dead").is_some() || parsed.get("total_dead").is_some(),
        "Dead should have dead code fields: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_callees() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callees process\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");
    assert!(
        parsed.get("calls").is_some(),
        "Callees should have calls field: {}",
        stdout.trim()
    );
    assert!(
        parsed.get("count").is_some(),
        "Callees should have count field: {}",
        stdout.trim()
    );
}
