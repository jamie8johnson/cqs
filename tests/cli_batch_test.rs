//! Gated behind the `slow-tests` feature: these shell out to the `cqs`
//! binary and cold-load the full model stack per invocation, adding ~2h
//! to CI. PR CI skips; nightly runs `cargo test --features "gpu-index slow-tests"`.
//! See issue #980.
#![cfg(feature = "slow-tests")]

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
    assert!(
        parsed["data"].is_array(),
        "callers should return a JSON array under data"
    );
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

    // Both lines should be valid JSON envelopes
    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("Each line should be valid JSON");
        assert!(
            parsed["data"].is_array() || parsed["data"].is_object(),
            "data payload should be array or object: {}",
            line
        );
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
    // On batch error: data is null, error is {"code": ..., "message": ...}
    assert!(
        !parsed["error"].is_null(),
        "Should have populated error field: {}",
        stdout.trim()
    );
    assert!(
        parsed["error"]["message"].is_string(),
        "Error should have message field: {}",
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
        parsed["data"].get("total_chunks").is_some(),
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
        parsed["data"].get("callers").is_some(),
        "Explain should have callers: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("callees").is_some(),
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
        parsed["data"].get("dead").is_some() || parsed["data"].get("total_dead").is_some(),
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
        parsed["data"].get("calls").is_some(),
        "Callees should have calls field: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("count").is_some(),
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

    // Pipeline envelope (under data)
    assert_eq!(
        parsed["data"].get("stages").and_then(|v| v.as_u64()),
        Some(2)
    );
    assert!(
        parsed["data"].get("results").is_some(),
        "Should have results array"
    );
    assert!(
        parsed["data"].get("pipeline").is_some(),
        "Should have pipeline field"
    );
    assert!(
        parsed["data"].get("total_inputs").is_some(),
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
        parsed["data"].get("stages").and_then(|v| v.as_u64()),
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

    // Should get empty results, not an error (under data)
    let results = parsed["data"].get("results").and_then(|v| v.as_array());
    assert!(results.is_some(), "Should have results: {}", stdout.trim());
    assert_eq!(
        results.unwrap().len(),
        0,
        "Should have 0 results for nonexistent function"
    );
    assert_eq!(
        parsed["data"].get("total_inputs").and_then(|v| v.as_u64()),
        Some(0)
    );
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

    // Pipeline error: error is now structured {code, message}
    let error_msg = parsed["error"]["message"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("Cannot pipe into 'stats'"),
        "Should reject non-pipeable downstream: {}",
        error_msg
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

    // Should be a bare array under data (callers output), NOT a pipeline envelope
    assert!(
        parsed["data"].is_array(),
        "Single command should not produce pipeline envelope: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("pipeline").is_none(),
        "Should not have pipeline field under data"
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

    // Should be a normal search result (with results key under data), not a pipeline.
    // On error, error is non-null with structured {code, message}.
    assert!(
        parsed["data"].get("results").is_some() || !parsed["error"].is_null(),
        "Should be normal search output: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("pipeline").is_none(),
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

    // First line: pipeline result (under data)
    let line1: serde_json::Value =
        serde_json::from_str(lines[0]).expect("First line should be valid JSON");
    assert!(
        line1["data"].get("pipeline").is_some(),
        "First line should be pipeline envelope"
    );

    // Second line: stats (single command, under data)
    let line2: serde_json::Value =
        serde_json::from_str(lines[1]).expect("Second line should be valid JSON");
    assert!(
        line2["data"].get("total_chunks").is_some(),
        "Second line should be stats output"
    );
}

// =============================================================================
// HP-4: High-value batch command integration tests
// =============================================================================

#[test]
#[serial]
fn test_batch_impact() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("impact process\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert_eq!(
        parsed["data"].get("name").and_then(|v| v.as_str()),
        Some("process"),
        "Impact should report the target function: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("callers").is_some(),
        "Impact should have callers field: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("tests").is_some(),
        "Impact should have tests field: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("caller_count").is_some(),
        "Impact should have caller_count: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("test_count").is_some(),
        "Impact should have test_count: {}",
        stdout.trim()
    );

    // `process` is called by `main` -> at least 1 caller
    let caller_count = parsed["data"]
        .get("caller_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        caller_count >= 1,
        "process should have at least 1 caller (main), got {}",
        caller_count
    );
}

#[test]
#[serial]
fn test_batch_impact_with_suggest_tests() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("impact process --suggest-tests\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert!(
        parsed["data"].get("test_suggestions").is_some(),
        "Impact with --suggest-tests should have test_suggestions: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_trace_connected() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // main calls process, process calls validate -- trace should find a path
    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("trace main validate\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert_eq!(
        parsed["data"].get("source").and_then(|v| v.as_str()),
        Some("main"),
        "Trace should report source: {}",
        stdout.trim()
    );
    assert_eq!(
        parsed["data"].get("target").and_then(|v| v.as_str()),
        Some("validate"),
        "Trace should report target: {}",
        stdout.trim()
    );
    assert_eq!(
        parsed["data"].get("found").and_then(|v| v.as_bool()),
        Some(true),
        "Trace should find a path from main to validate: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("path").is_some(),
        "Trace should have path field when found: {}",
        stdout.trim()
    );

    // Path should have at least 2 hops (main -> process -> validate)
    let path = parsed["data"].get("path").and_then(|v| v.as_array());
    assert!(
        path.is_some_and(|p| p.len() >= 2),
        "Trace path should have >= 2 hops: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_trace_disconnected() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    // validate does not call main -- trace should fail to find a path
    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("trace validate main\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert_eq!(
        parsed["data"].get("found").and_then(|v| v.as_bool()),
        Some(false),
        "Trace should not find a path from validate to main: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_similar() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("similar process\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert_eq!(
        parsed["data"].get("target").and_then(|v| v.as_str()),
        Some("process"),
        "Similar should report the target function: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("results").is_some(),
        "Similar should have results array: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("total").is_some(),
        "Similar should have total field: {}",
        stdout.trim()
    );

    // With 4 functions in the fixture, there should be at least 1 similar result
    let results = parsed["data"].get("results").and_then(|v| v.as_array());
    assert!(
        results.is_some_and(|r| !r.is_empty()),
        "Similar should find at least one result for process: {}",
        stdout.trim()
    );

    // Each result should have name, file, score
    let first = results.unwrap().first().unwrap();
    assert!(first.get("name").is_some(), "Result should have name field");
    assert!(first.get("file").is_some(), "Result should have file field");
    assert!(
        first.get("score").is_some(),
        "Result should have score field"
    );
}

#[test]
#[serial]
fn test_batch_stale() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("stale\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert!(
        parsed["data"].get("stale").is_some(),
        "Stale should have stale array: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("missing").is_some(),
        "Stale should have missing array: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("stale_count").is_some(),
        "Stale should have stale_count: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("missing_count").is_some(),
        "Stale should have missing_count: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("total_indexed").is_some(),
        "Stale should have total_indexed: {}",
        stdout.trim()
    );

    // Freshly indexed project should have 0 stale and 0 missing
    assert_eq!(
        parsed["data"].get("stale_count").and_then(|v| v.as_u64()),
        Some(0),
        "Fresh index should have 0 stale files"
    );
    assert_eq!(
        parsed["data"].get("missing_count").and_then(|v| v.as_u64()),
        Some(0),
        "Fresh index should have 0 missing files"
    );
    // Should have indexed at least our 2 files
    let total = parsed["data"]
        .get("total_indexed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        total >= 2,
        "Should have indexed at least 2 files, got {}",
        total
    );
}

#[test]
#[serial]
fn test_batch_health() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("health\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert!(
        parsed["data"].get("stats").is_some(),
        "Health should have stats object: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("stale_count").is_some(),
        "Health should have stale_count: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("missing_count").is_some(),
        "Health should have missing_count: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("dead_confident").is_some(),
        "Health should have dead_confident: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("dead_possible").is_some(),
        "Health should have dead_possible: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("hotspots").is_some(),
        "Health should have hotspots: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("note_count").is_some(),
        "Health should have note_count: {}",
        stdout.trim()
    );

    // Stats sub-object should have chunk/file counts
    let stats = parsed["data"].get("stats").unwrap();
    assert!(
        stats.get("total_chunks").is_some(),
        "Stats should have total_chunks: {}",
        stdout.trim()
    );
    assert!(
        stats.get("total_files").is_some(),
        "Stats should have total_files: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_gather() {
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("gather \"process input\"\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Should be valid JSON");

    assert!(
        parsed["data"].get("query").is_some(),
        "Gather should have query field: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("chunks").is_some(),
        "Gather should have chunks array: {}",
        stdout.trim()
    );

    // Gather should find at least one chunk for "process input"
    let chunks = parsed["data"].get("chunks").and_then(|v| v.as_array());
    assert!(
        chunks.is_some_and(|c| !c.is_empty()),
        "Gather should find at least one chunk for 'process input': {}",
        stdout.trim()
    );

    // The expansion_capped and search_degraded fields should be present
    assert!(
        parsed["data"].get("expansion_capped").is_some(),
        "Gather should have expansion_capped: {}",
        stdout.trim()
    );
    assert!(
        parsed["data"].get("search_degraded").is_some(),
        "Gather should have search_degraded: {}",
        stdout.trim()
    );
}

#[test]
#[serial]
fn test_batch_multiple_hp4_commands() {
    // Verify multiple HP-4 commands work in a single batch session
    let dir = setup_graph_project();
    init_and_index(&dir);

    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("impact process\nstale\nhealth\n")
        .output()
        .expect("Failed to run batch");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3, "Should have three JSONL lines");

    // Line 1: impact
    let l1: serde_json::Value =
        serde_json::from_str(lines[0]).expect("Impact line should be valid JSON");
    assert!(
        l1["data"].get("name").is_some() && l1["data"].get("callers").is_some(),
        "Line 1 should be impact output"
    );

    // Line 2: stale
    let l2: serde_json::Value =
        serde_json::from_str(lines[1]).expect("Stale line should be valid JSON");
    assert!(
        l2["data"].get("stale_count").is_some(),
        "Line 2 should be stale output"
    );

    // Line 3: health
    let l3: serde_json::Value =
        serde_json::from_str(lines[2]).expect("Health line should be valid JSON");
    assert!(
        l3["data"].get("stats").is_some(),
        "Line 3 should be health output"
    );
}
