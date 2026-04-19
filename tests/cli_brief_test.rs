//! Audit P3 #114 — `cmd_brief` integration tests.
//!
//! `cqs brief <path>` returns a one-line summary per function in `path`,
//! including caller and test counts. The inline tests in
//! `src/cli/commands/io/brief.rs` cover only `BriefEntry` / `BriefOutput`
//! serialization on hand-built structs — nothing pins the path from CLI
//! argv → `build_brief_data` → caller/test BFS → JSON envelope or text.
//!
//! Subprocess pattern: `cmd_brief` takes `&CommandContext<'_, ReadOnly>`
//! which is `pub(crate)`; integration tests cannot construct one. Gated
//! behind `slow-tests` because we run `cqs index`.

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Build a project with three functions and a test exercising one of them.
/// `process` is called by `main` (1 caller) and by `test_process` (1 test);
/// `validate` is called only by `process` (1 caller, 0 direct tests, but
/// `test_process` reaches it via depth-2 BFS so test_count >= 1).
/// Pins the caller/test count BFS path through `build_brief_data`.
fn setup_brief_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Entry point.
pub fn main() {
    let _ = process(42);
}

/// Process input through validation.
pub fn process(input: i32) -> i32 {
    validate(input)
}

/// Check input.
fn validate(input: i32) -> i32 {
    input + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        assert_eq!(process(1), 2);
    }
}
"#,
    )
    .expect("Failed to write lib.rs");

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

/// JSON mode: pin envelope shape + `data.functions` length and per-entry
/// fields. Functions discovered via `get_chunks_by_origin("src/lib.rs")`
/// must include `main`, `process`, `validate`, and the test. Each entry
/// carries `name`, `chunk_type`, `line_start`, `callers`, `tests`. The
/// `total` field must equal `functions.len()`.
#[test]
#[serial]
fn test_brief_json_emits_envelope_with_function_list() {
    let dir = setup_brief_project();

    let output = cqs()
        .args(["brief", "src/lib.rs", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("cqs brief failed to spawn");

    assert!(
        output.status.success(),
        "brief should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    // Envelope shape
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert!(parsed["data"].is_object(), "data must be object: {stdout}");

    // BriefOutput inner shape
    assert!(
        parsed["data"]["file"].is_string(),
        "file must be a string. got: {}",
        parsed["data"]["file"]
    );

    let functions = parsed["data"]["functions"]
        .as_array()
        .expect("functions must be an array");
    assert!(
        functions.len() >= 3,
        "expected at least main + process + validate, got {} functions: {functions:?}",
        functions.len()
    );

    // total must equal the array length (BriefOutput.total)
    let total = parsed["data"]["total"]
        .as_u64()
        .expect("total must be numeric");
    assert_eq!(
        total as usize,
        functions.len(),
        "total must equal functions.len()"
    );

    // Per-entry shape: name + chunk_type + line_start + callers + tests
    let names: Vec<&str> = functions
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    for required in ["main", "process", "validate"] {
        assert!(
            names.contains(&required),
            "expected function '{required}' in brief output, got: {names:?}"
        );
    }
    for entry in functions {
        assert!(entry["name"].is_string(), "entry.name must be a string");
        assert!(
            entry["chunk_type"].is_string(),
            "entry.chunk_type must be a string"
        );
        assert!(
            entry["line_start"].is_number(),
            "entry.line_start must be numeric"
        );
        assert!(
            entry["callers"].is_number(),
            "entry.callers must be numeric (u64)"
        );
        assert!(
            entry["tests"].is_number(),
            "entry.tests must be numeric (u64)"
        );
    }

    // `process` is called by `main` AND `test_process` (caller_counts is the
    // direct count from `get_caller_counts_batch`, so it should be >= 1).
    let process_entry = functions
        .iter()
        .find(|f| f["name"] == "process")
        .expect("process function must be present");
    assert!(
        process_entry["callers"].as_u64().unwrap_or(0) >= 1,
        "process should have >= 1 callers (main calls it). got: {}",
        process_entry["callers"]
    );
}

/// Text mode: confirms the human-readable rendering lists every function
/// name in `path`. The text path goes through `BriefEntry`'s display
/// formatter (the `{:<30} {:<12} {:>7} {:>7}` row) — pin that the names
/// flow through unchanged.
#[test]
#[serial]
fn test_brief_text_output_contains_all_function_names() {
    let dir = setup_brief_project();

    let output = cqs()
        .args(["brief", "src/lib.rs"])
        .current_dir(dir.path())
        .output()
        .expect("cqs brief (text) failed to spawn");

    assert!(
        output.status.success(),
        "brief text mode should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for required in ["main", "process", "validate"] {
        assert!(
            stdout.contains(required),
            "text output must mention '{required}'. got: {stdout}"
        );
    }

    // Header columns come from the format string in `cmd_brief`'s text branch.
    assert!(
        stdout.contains("Name") && stdout.contains("Type") && stdout.contains("Callers"),
        "text output must include the column headers. got: {stdout}"
    );
}
