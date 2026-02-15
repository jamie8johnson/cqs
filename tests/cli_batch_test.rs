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

// =============================================================================
// Pipeline integration tests
// =============================================================================

#[test]
#[serial]
fn test_pipeline_callers_to_explain() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers process | explain\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    // Pipeline envelope
    assert_eq!(parsed.get("stages").and_then(|v| v.as_u64()), Some(2));
    assert!(parsed.get("results").is_some(), "Should have results array");
    assert!(
        parsed.get("pipeline").is_some(),
        "Should have pipeline field"
    );
    assert!(
        parsed.get("total_inputs").is_some(),
        "Should have total_inputs"
    );
}

#[test]
#[serial]
fn test_pipeline_three_stages() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callees main | callers | explain\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert_eq!(
        parsed.get("stages").and_then(|v| v.as_u64()),
        Some(3),
        "Should be 3-stage pipeline: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_pipeline_empty_upstream() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Search for something that doesn't exist → 0 names → empty pipeline result
    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers xyznonexistent99 | explain\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    // Should get empty results, not an error
    let results = parsed.get("results").and_then(|v| v.as_array());
    assert!(results.is_some(), "Should have results: {}", stdout.trim());
    assert_eq!(
        results.unwrap().len(),
        0,
        "Should have 0 results for nonexistent function"
    );
    assert_eq!(parsed.get("total_inputs").and_then(|v| v.as_u64()), Some(0));
}

#[test]
#[serial]
fn test_pipeline_ineligible_downstream() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers process | stats\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    let error = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        error.contains("Cannot pipe into 'stats'"),
        "Should reject non-pipeable downstream: {}",
        error
    );
}

#[test]
#[serial]
fn test_pipeline_single_stage_no_pipe() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // No pipe → should use normal dispatch, NOT pipeline envelope
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

    // Should be a bare array (callers output), NOT a pipeline envelope
    assert!(
        parsed.is_array(),
        "Single command should not produce pipeline envelope: {}",
        stdout.trim()
    );
    assert!(
        parsed.get("pipeline").is_none(),
        "Should not have pipeline field"
    );
}

#[test]
#[serial]
fn test_pipeline_quoted_pipe_in_query() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Pipe inside quotes should NOT be treated as a pipeline separator.
    // shell_words tokenizes "foo | bar" as a single token, so no standalone `|`.
    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("search \"foo | bar\"\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    // Should be a normal search result (with results key), not a pipeline
    assert!(
        parsed.get("results").is_some() || parsed.get("error").is_some(),
        "Should be normal search output: {}",
        stdout.trim()
    );
    assert!(
        parsed.get("pipeline").is_none(),
        "Quoted pipe should not trigger pipeline"
    );
}

#[test]
#[serial]
fn test_pipeline_mixed_with_single() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // Mix of pipeline and single commands in one batch session
    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("callers process | explain\nstats\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2, "Should have two JSONL lines");

    // First line: pipeline result
    let line1: serde_json::Value =
        serde_json::from_str(lines[0]).expect("First line should be valid JSON");
    assert!(
        line1.get("pipeline").is_some(),
        "First line should be pipeline envelope"
    );

    // Second line: stats (single command)
    let line2: serde_json::Value =
        serde_json::from_str(lines[1]).expect("Second line should be valid JSON");
    assert!(
        line2.get("total_chunks").is_some(),
        "Second line should be stats output"
    );
}
