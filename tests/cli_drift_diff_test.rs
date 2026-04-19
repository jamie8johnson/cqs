//! Audit P3 #113 — `cmd_drift` and `cmd_diff` integration tests.
//!
//! `cqs drift <ref>` and `cqs diff <source> [<target>]` open both a
//! reference store and the project store, run `cqs::drift::detect_drift`
//! / `cqs::semantic_diff`, and emit either text or a JSON envelope. The
//! inline tests in `src/cli/commands/io/{drift,diff}.rs` cover the typed
//! output structs (`DriftOutput` / `DiffOutput`) but nothing pins the
//! end-to-end CLI argv → reference resolution → drift/diff → envelope.
//!
//! Setup:
//! - Create a tempdir for the project, init + index it.
//! - Create a SECOND tempdir as the reference source.
//! - Run `cqs ref add baseline <ref_source>` to register the baseline.
//! - Then `cqs drift baseline --json` and `cqs diff baseline --json`.
//!
//! Subprocess + `slow-tests` for the same reason as cli_blame_test.rs:
//! the embedder cold-loads on every `cqs index` / `cqs ref add`.

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

/// Build the project to be compared against. Two functions; the body of
/// `process` is large enough that a real semantic embedding gets a
/// distinct vector from `validate`.
fn setup_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Process input through validation and transformation.
pub fn process(input: i32) -> String {
    let valid = validate(input);
    if valid {
        format!("processed: {}", input * 2)
    } else {
        String::from("invalid")
    }
}

/// Check whether the input is positive.
pub fn validate(input: i32) -> bool {
    input > 0
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

/// Build the reference source to register as `baseline`. Same `process`
/// shape as the project but a slightly different `validate` body — gives
/// us at least one entry in the diff `modified` list and exercises the
/// drift-by-similarity path.
fn setup_baseline_source() -> TempDir {
    let dir = TempDir::new().expect("Failed to create baseline tempdir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Process input through validation and transformation.
pub fn process(input: i32) -> String {
    let valid = validate(input);
    if valid {
        format!("processed: {}", input * 2)
    } else {
        String::from("invalid")
    }
}

/// Check whether the input falls within the supported range.
pub fn validate(input: i32) -> bool {
    input >= 0 && input < 1000
}

/// Helper that exists only in baseline — should appear in diff.removed.
pub fn legacy_helper() -> i32 {
    42
}
"#,
    )
    .expect("Failed to write baseline lib.rs");

    dir
}

/// Add `baseline` reference using `cqs ref add`. We isolate the reference
/// store directory via `XDG_DATA_HOME` so concurrent test runs don't
/// clobber each other's ref storage. Mirrors `cli_commands_test.rs::test_query_with_ref`.
fn add_baseline_ref(project: &TempDir, ref_source: &TempDir, xdg_home: &TempDir) {
    cqs()
        .args([
            "ref",
            "add",
            "baseline",
            ref_source.path().to_str().unwrap(),
        ])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .assert()
        .success();
}

/// `cqs drift baseline --json`: must emit the standard envelope and the
/// inner `DriftOutput` shape (reference / threshold / min_drift / drifted /
/// total_compared / unchanged). Pins the JSON path through `build_drift_output`
/// + `emit_json` at `drift.rs:111-112`.
#[test]
#[serial]
fn test_drift_json_emits_envelope_for_baseline_reference() {
    let project = setup_project();
    let ref_source = setup_baseline_source();
    let xdg_home = TempDir::new().expect("xdg tempdir");

    add_baseline_ref(&project, &ref_source, &xdg_home);

    let output = cqs()
        .args(["drift", "baseline", "--json"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .output()
        .expect("cqs drift failed to spawn");

    assert!(
        output.status.success(),
        "drift should succeed. stdout={} stderr={}",
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

    // DriftOutput inner shape
    assert_eq!(
        parsed["data"]["reference"], "baseline",
        "reference must echo the requested name"
    );
    assert!(
        parsed["data"]["threshold"].is_number(),
        "threshold must be numeric. got: {}",
        parsed["data"]["threshold"]
    );
    assert!(
        parsed["data"]["min_drift"].is_number(),
        "min_drift must be numeric. got: {}",
        parsed["data"]["min_drift"]
    );
    assert!(
        parsed["data"]["drifted"].is_array(),
        "drifted must be an array. got: {}",
        parsed["data"]["drifted"]
    );
    assert!(
        parsed["data"]["total_compared"].is_number(),
        "total_compared must be numeric. got: {}",
        parsed["data"]["total_compared"]
    );
    assert!(
        parsed["data"]["unchanged"].is_number(),
        "unchanged must be numeric. got: {}",
        parsed["data"]["unchanged"]
    );

    // total_compared >= unchanged + drifted (drifted are a subset of compared)
    let total = parsed["data"]["total_compared"].as_u64().unwrap();
    let unchanged = parsed["data"]["unchanged"].as_u64().unwrap();
    let drifted_len = parsed["data"]["drifted"].as_array().unwrap().len() as u64;
    assert!(
        total >= unchanged,
        "total_compared ({total}) must be >= unchanged ({unchanged})"
    );
    assert!(
        total >= drifted_len,
        "total_compared ({total}) must be >= drifted.len ({drifted_len})"
    );
}

/// `cqs diff baseline --json` (default target = project): pins the JSON
/// envelope shape for `DiffOutput` (source / target / added / removed /
/// modified / summary). The legacy_helper() function exists only in
/// baseline, so it should land in `removed`. Exercises `display_diff_json`
/// at `diff.rs:192-195`.
#[test]
#[serial]
fn test_diff_json_emits_envelope_baseline_to_project() {
    let project = setup_project();
    let ref_source = setup_baseline_source();
    let xdg_home = TempDir::new().expect("xdg tempdir");

    add_baseline_ref(&project, &ref_source, &xdg_home);

    let output = cqs()
        .args(["diff", "baseline", "--json"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .output()
        .expect("cqs diff failed to spawn");

    assert!(
        output.status.success(),
        "diff should succeed. stdout={} stderr={}",
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

    // DiffOutput inner shape
    assert_eq!(parsed["data"]["source"], "baseline");
    assert_eq!(
        parsed["data"]["target"], "project",
        "default target must be 'project' when --target is omitted"
    );
    assert!(parsed["data"]["added"].is_array(), "added must be array");
    assert!(
        parsed["data"]["removed"].is_array(),
        "removed must be array"
    );
    assert!(
        parsed["data"]["modified"].is_array(),
        "modified must be array"
    );

    // Summary shape — keys map to per-array lengths.
    let summary = &parsed["data"]["summary"];
    assert!(summary.is_object(), "summary must be object");
    for key in ["added", "removed", "modified", "unchanged"] {
        assert!(
            summary[key].is_number(),
            "summary.{key} must be numeric. got: {}",
            summary[key]
        );
    }
    let added_len = parsed["data"]["added"].as_array().unwrap().len() as u64;
    let removed_len = parsed["data"]["removed"].as_array().unwrap().len() as u64;
    let modified_len = parsed["data"]["modified"].as_array().unwrap().len() as u64;
    assert_eq!(
        summary["added"].as_u64().unwrap(),
        added_len,
        "summary.added must equal added.len()"
    );
    assert_eq!(
        summary["removed"].as_u64().unwrap(),
        removed_len,
        "summary.removed must equal removed.len()"
    );
    assert_eq!(
        summary["modified"].as_u64().unwrap(),
        modified_len,
        "summary.modified must equal modified.len()"
    );

    // legacy_helper exists only in baseline — should appear in removed.
    let removed: Vec<&str> = parsed["data"]["removed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        removed.iter().any(|n| *n == "legacy_helper"),
        "legacy_helper exists only in baseline → must appear in diff.removed. got: {removed:?}"
    );
}

/// Unknown reference: pins the failure path through
/// `resolve_reference_store` (drift/diff both use it). Exit non-zero,
/// stderr explains.
#[test]
#[serial]
fn test_drift_unknown_reference_errors() {
    let project = setup_project();
    let xdg_home = TempDir::new().expect("xdg tempdir");

    let output = cqs()
        .args(["drift", "definitely_no_such_ref_xyz", "--json"])
        .env("XDG_DATA_HOME", xdg_home.path())
        .current_dir(project.path())
        .output()
        .expect("cqs drift failed to spawn");

    assert!(
        !output.status.success(),
        "drift on unknown ref must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_lc = stderr.to_lowercase();
    assert!(
        stderr_lc.contains("not found")
            || stderr_lc.contains("reference")
            || stderr_lc.contains("ref")
            || stderr.contains("definitely_no_such_ref_xyz"),
        "stderr should mention the missing reference. got: {stderr}"
    );
}
