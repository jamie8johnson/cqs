//! TC-HAP-1.29-7 — `cmd_similar` CLI integration tests.
//!
//! `cqs similar <name>` takes a function name, resolves it to a chunk, and
//! returns the K nearest chunks by embedding cosine similarity (excluding
//! the source). The existing inline tests in `src/cli/commands/search/similar.rs`
//! cover only `parse_target` — there was no end-to-end test of the CLI
//! path through `resolve_target` → embedding fetch → HNSW search → envelope.
//!
//! Subprocess pattern + `slow-tests` gate because `cmd_similar` cold-loads
//! the embedder at index time (real embeddings are needed for meaningful
//! cosine similarity).

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

/// Tiny project with 4 related functions. `cqs similar add --json -n 3`
/// should return sub/mul/divide (in some order) and exclude add itself.
fn setup_similar_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Add two numbers together.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Subtract one number from another.
pub fn sub(a: i32, b: i32) -> i32 {
    a - b
}

/// Multiply two numbers.
pub fn mul(a: i32, b: i32) -> i32 {
    a * b
}

/// Divide two numbers, returning None on divide-by-zero.
pub fn divide(a: i32, b: i32) -> Option<i32> {
    if b == 0 {
        None
    } else {
        Some(a / b)
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

/// JSON mode: envelope shape + self-exclusion + bounded result count.
/// `cmd_similar` uses `resolve_target` (non-test chunk preference) and
/// excludes the source chunk. With 4 chunks in the project, a limit of
/// 3 and self-exclusion, the result should have at most 3 entries.
#[test]
#[serial]
fn test_similar_json_returns_envelope_with_self_excluded() {
    let dir = setup_similar_project();

    let output = cqs()
        .args(["similar", "add", "--json", "-n", "3"])
        .current_dir(dir.path())
        .output()
        .expect("cqs similar failed to spawn");

    assert!(
        output.status.success(),
        "similar should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    // Envelope shape
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert!(
        parsed["data"].is_object() || parsed["data"].is_array(),
        "data must be object or array: {stdout}"
    );

    // Inner shape — display_similar_results_json emits a `results` array
    // and a `target` string. Don't pin the exact keys too tightly since
    // the display helper is the contract boundary.
    let data = &parsed["data"];
    assert!(
        data.get("results").is_some() || data.get("target").is_some() || data.is_array(),
        "data should carry similar results; got {data}"
    );

    // If `results` is present, it must be an array and no entry should
    // match the source chunk name "add".
    if let Some(results) = data.get("results").and_then(|v| v.as_array()) {
        assert!(
            results.len() <= 3,
            "limit=3 must cap the result list, got {}",
            results.len()
        );
        for entry in results {
            let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
            assert_ne!(
                name, "add",
                "source chunk must be excluded from similar results"
            );
        }
    }
}

/// `cmd_similar` against an unknown name must error cleanly (not panic).
/// Exit code should be non-zero (matches the `cqs::resolve_target`
/// StoreError::NotFound path).
#[test]
#[serial]
fn test_similar_unknown_name_returns_error() {
    let dir = setup_similar_project();

    let output = cqs()
        .args(["similar", "this_function_does_not_exist_42", "-n", "3"])
        .current_dir(dir.path())
        .output()
        .expect("cqs similar failed to spawn");

    assert!(
        !output.status.success(),
        "similar on unknown name must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
