//! Audit P3 #115 — `cmd_neighbors` integration tests.
//!
//! `cqs neighbors <name>` does a brute-force cosine scan over all chunk
//! embeddings, returning top-K neighbors with similarity scores. The
//! inline tests in `src/cli/commands/search/neighbors.rs` cover only the
//! pure helpers (`dot()` and `build_neighbors_output()` on hand-built
//! data) — nothing pins the CLI argv → `resolve_target` → batched
//! embedding scan → JSON envelope path.
//!
//! Subprocess pattern: `cmd_neighbors` takes `&CommandContext<'_, ReadOnly>`
//! which is `pub(crate)`. Gated `slow-tests` for the embedder.

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

/// Build a project with a handful of distinct functions so the brute-force
/// neighbor scan has something to rank. We need >= 2 chunks for a non-empty
/// neighbor list (the target is excluded from its own results).
fn setup_neighbors_project() -> TempDir {
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

/// JSON mode: pin envelope shape + neighbors are sorted by score descending.
/// Sort order is a load-bearing contract: agents truncating to top-K
/// downstream rely on the first entry being the closest match. The order
/// comes from `find_neighbors:126` — `sort_by(|a, b| b.1.total_cmp(&a.1))`.
#[test]
#[serial]
fn test_neighbors_json_returns_results_sorted_by_score_desc() {
    let dir = setup_neighbors_project();

    let output = cqs()
        .args(["neighbors", "add", "--json", "-n", "3"])
        .current_dir(dir.path())
        .output()
        .expect("cqs neighbors failed to spawn");

    assert!(
        output.status.success(),
        "neighbors should succeed. stdout={} stderr={}",
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

    // NeighborsOutput inner shape
    assert_eq!(
        parsed["data"]["target"], "add",
        "target must echo the requested name"
    );
    let neighbors = parsed["data"]["neighbors"]
        .as_array()
        .expect("neighbors must be an array");

    // Self is excluded — `find_neighbors:111-112: if id == target.id continue`.
    assert!(
        !neighbors.is_empty(),
        "expected at least 1 neighbor (sub/mul/divide), got 0: {stdout}"
    );
    assert!(
        neighbors.len() <= 3,
        "limit=3 must cap the result list, got {}",
        neighbors.len()
    );
    let count = parsed["data"]["count"]
        .as_u64()
        .expect("count must be numeric");
    assert_eq!(
        count as usize,
        neighbors.len(),
        "count must equal neighbors.len()"
    );

    // None of the neighbors should be the target itself.
    for entry in neighbors {
        assert_ne!(
            entry["name"], "add",
            "target must be excluded from its own neighbor list"
        );
        // Per-entry shape check.
        assert!(entry["name"].is_string(), "entry.name must be string");
        assert!(entry["file"].is_string(), "entry.file must be string");
        assert!(
            entry["line_start"].is_number(),
            "entry.line_start must be number"
        );
        assert!(
            entry["chunk_type"].is_string(),
            "entry.chunk_type must be string"
        );
        assert!(entry["score"].is_number(), "entry.score must be number");
    }

    // Scores must be in descending order — pins the sort contract.
    let scores: Vec<f64> = neighbors
        .iter()
        .map(|e| e["score"].as_f64().expect("score must be f64"))
        .collect();
    for window in scores.windows(2) {
        assert!(
            window[0] >= window[1],
            "scores must be sorted descending: {:?}",
            scores
        );
    }
}

/// Unknown target: `resolve_target` fails with a context-decorated error.
/// Pins `cmd_neighbors:181: resolve_target.context("Failed to resolve target")`.
/// Process must exit non-zero with an actionable stderr message.
#[test]
#[serial]
fn test_neighbors_unknown_target_errors_gracefully() {
    let dir = setup_neighbors_project();

    let output = cqs()
        .args(["neighbors", "this_function_definitely_does_not_exist_xyz"])
        .current_dir(dir.path())
        .output()
        .expect("cqs neighbors failed to spawn");

    assert!(
        !output.status.success(),
        "neighbors on unknown target must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "should print an error message on unknown target"
    );
    let stderr_lc = stderr.to_lowercase();
    // resolve_target failure flows through a "Failed to resolve target"
    // anyhow context. Don't bind the exact phrasing — just confirm the
    // error references the target or the resolution failure.
    assert!(
        stderr_lc.contains("resolve")
            || stderr_lc.contains("not found")
            || stderr_lc.contains("no function")
            || stderr.contains("this_function_definitely_does_not_exist_xyz"),
        "stderr should explain the failure. got: {stderr}"
    );
}
