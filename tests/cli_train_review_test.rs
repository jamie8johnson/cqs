//! Audit P2 #46 — `cmd_plan` / `cmd_task` / `cmd_affected` / `cmd_ci`
//! integration tests.
//!
//! Each of these CLI wrappers performs its own composition: argument parsing
//! → embedder load → library call → JSON envelope wrap (and for `cmd_ci`,
//! exit-code mapping for gate failures). The library-level functions have
//! unit tests but the CLI surface — token-budget merging, exit codes, JSON
//! envelope shape — was unverified.
//!
//! Subprocess pattern + `slow-tests` gate because each command needs an
//! initialized index and (for plan/task) a loaded embedder.

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Set up a tiny git-tracked indexed project. `cmd_affected` and `cmd_ci`
/// run `git diff` so the working tree must be a real git repo.
fn setup_git_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Entry point.
pub fn main() {
    process(42);
}

/// Process input.
pub fn process(input: i32) -> i32 {
    validate(input)
}

/// Check input.
fn validate(input: i32) -> i32 {
    input + 1
}
"#,
    )
    .expect("Failed to write lib.rs");

    StdCommand::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init failed");
    StdCommand::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .status()
        .expect("git config user.email failed");
    StdCommand::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir.path())
        .status()
        .expect("git config user.name failed");
    StdCommand::new("git")
        .args(["add", "src/lib.rs"])
        .current_dir(dir.path())
        .status()
        .expect("git add failed");
    StdCommand::new("git")
        .args(["commit", "-q", "-m", "initial"])
        .current_dir(dir.path())
        .status()
        .expect("git commit failed");

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

// ============================================================================
// P2 #46 (a) — `cmd_plan`
// ============================================================================

#[test]
#[serial]
fn test_plan_json_returns_template_and_checklist() {
    let dir = setup_git_project();

    let output = cqs()
        .args(["plan", "add a flag for verbose output", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs plan");

    assert!(
        output.status.success(),
        "plan should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — raw: {}", e, stdout));

    // Envelope shape
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert!(parsed["data"].is_object(), "data must be object: {stdout}");

    // PlanResult inner shape — `template`, `template_description`, `scout`,
    // `checklist`, `patterns` are the documented fields. Pin the keys.
    let data = &parsed["data"];
    assert!(
        data["template"].is_string(),
        "data.template must be a string, got: {}",
        data["template"]
    );
    assert!(
        data["checklist"].is_array(),
        "data.checklist must be an array, got: {}",
        data["checklist"]
    );
    assert!(
        data["scout"].is_object() || data["scout"].is_array(),
        "data.scout must be present (object or array), got: {}",
        data["scout"]
    );
    // checklist should be non-empty for any plan template
    let checklist = data["checklist"].as_array().unwrap();
    assert!(
        !checklist.is_empty(),
        "checklist should be non-empty for any task description, got: {data}"
    );
}

// ============================================================================
// P2 #46 (b) — `cmd_task`
// ============================================================================

#[test]
#[serial]
fn test_task_json_returns_implementation_brief() {
    let dir = setup_git_project();

    let output = cqs()
        .args(["task", "improve validation logic", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs task");

    assert!(
        output.status.success(),
        "task should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // Envelope
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());
    assert!(parsed["data"].is_object(), "data must be object: {stdout}");

    // TaskResult publishes scout + impact + placement + notes-shaped payload.
    // Pin that the data is a non-empty object — a regression that returned
    // `{}` would still pass `is_object()` but fail this check.
    let data = &parsed["data"];
    assert!(
        data.as_object().is_some_and(|m| !m.is_empty()),
        "data object must have at least one key, got: {data}"
    );
}

// ============================================================================
// P2 #46 (c) — `cmd_affected`
// ============================================================================

#[test]
#[serial]
fn test_affected_json_lists_dependents_or_empty() {
    let dir = setup_git_project();

    // Modify a real function so `git diff` produces a non-empty diff.
    fs::write(
        dir.path().join("src/lib.rs"),
        r#"
/// Entry point.
pub fn main() {
    process(42);
}

/// Process input.
pub fn process(input: i32) -> i32 {
    validate(input)
}

/// Check input.
fn validate(input: i32) -> i32 {
    let extra = input * 2;
    input + 1 + extra
}
"#,
    )
    .expect("rewrite lib.rs");

    let output = cqs()
        .args(["affected", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs affected");

    assert!(
        output.status.success(),
        "affected should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // Envelope
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());

    let data = &parsed["data"];
    // Either populated branch (DiffImpactResult fields) or
    // empty_affected_json branch — both must publish `changed_functions` and
    // a `summary` object.
    assert!(
        data["changed_functions"].is_array(),
        "data.changed_functions must be an array, got: {}",
        data["changed_functions"]
    );
    assert!(
        data["summary"].is_object(),
        "data.summary must be an object, got: {}",
        data["summary"]
    );
    assert!(
        data["summary"]["changed_count"].is_number(),
        "data.summary.changed_count must be numeric"
    );
    // `overall_risk` is added by cmd_affected itself (line 66 of affected.rs);
    // pin that the field exists at the top level of `data`.
    assert!(
        data["overall_risk"].is_string(),
        "data.overall_risk must be a string, got: {}",
        data["overall_risk"]
    );
}

// ============================================================================
// P2 #46 (d) — `cmd_ci`
// ============================================================================

#[test]
#[serial]
fn test_ci_with_clean_diff_returns_low_risk_and_exits_zero() {
    let dir = setup_git_project();

    // No modifications → empty diff → no changed functions → low risk → gate passes.
    let output = cqs()
        .args(["ci", "--json", "--gate", "high"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs ci");

    assert!(
        output.status.success(),
        "ci on clean diff should exit 0. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // Envelope
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());

    // CiReport has `review` + `gate` + `dead_in_diff` fields.
    let data = &parsed["data"];
    assert!(
        data["review"].is_object(),
        "data.review must be object, got: {}",
        data["review"]
    );
    assert!(
        data["gate"].is_object(),
        "data.gate must be object, got: {}",
        data["gate"]
    );
    assert_eq!(
        data["gate"]["passed"], true,
        "clean diff with high gate should pass"
    );
    assert_eq!(
        data["review"]["risk_summary"]["overall"], "low",
        "clean diff should report low overall risk"
    );
}

/// `cmd_ci` with `--gate off` should always pass even if the diff is risky.
/// Pins the off-gate path. Constructing a "guaranteed high-risk" diff in a
/// minimal fixture is fragile; the cleaner contract test is "off ⇒ pass".
#[test]
#[serial]
fn test_ci_with_gate_off_always_exits_zero() {
    let dir = setup_git_project();

    // Modify validate() — flips its body, which is the only thing main → process
    // → validate calls. In a real diff the risk would be flagged, but `--gate off`
    // bypasses the failure exit.
    fs::write(
        dir.path().join("src/lib.rs"),
        r#"
/// Entry point.
pub fn main() {
    process(42);
}

/// Process input.
pub fn process(input: i32) -> i32 {
    validate(input) * 100
}

/// Check input — REWRITTEN.
fn validate(input: i32) -> i32 {
    if input < 0 {
        return 0;
    }
    input + 1
}
"#,
    )
    .expect("rewrite lib.rs");

    let output = cqs()
        .args(["ci", "--json", "--gate", "off"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs ci");

    assert!(
        output.status.success(),
        "ci with --gate off must exit 0 regardless of risk. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    let data = &parsed["data"];
    assert_eq!(
        data["gate"]["passed"], true,
        "--gate off must always report passed=true"
    );
}

/// `cmd_ci --tokens N` should add `token_count` and `token_budget` to the
/// JSON output (per `src/cli/commands/review/ci.rs:42-46`).
#[test]
#[serial]
fn test_ci_token_budget_adds_token_fields() {
    let dir = setup_git_project();

    let output = cqs()
        .args(["ci", "--json", "--gate", "off", "--tokens", "200"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs ci --tokens");

    assert!(
        output.status.success(),
        "ci with --tokens should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    let data = &parsed["data"];
    assert!(
        data["token_count"].is_number(),
        "data.token_count must be numeric, got: {}",
        data["token_count"]
    );
    assert_eq!(
        data["token_budget"],
        serde_json::json!(200),
        "data.token_budget must echo the requested budget"
    );
}

// ============================================================================
// TC-HAP-1.29-8 — `cmd_ci` happy path
//
// The existing `cmd_ci` tests cover:
//   * clean diff → exit 0 + low risk
//   * gate off → always exit 0
//   * --tokens N → token fields in JSON
//
// What was NOT covered: a *non-empty* diff that represents real code
// changes, going through the full library-level `review_diff` + dead-code
// scan + gate evaluation, with the happy-path JSON shape asserted
// end-to-end.
// ============================================================================

/// Modify a real function body (not just whitespace), then run `cqs ci`.
/// The ci report must include the expected top-level fields: `review`,
/// `gate`, and `dead_in_diff` — that's the contract documented in the
/// library-level `ci::run_ci_analysis` tests, but never pinned at the
/// CLI boundary.
#[test]
#[serial]
fn test_ci_happy_path_non_empty_diff_emits_full_report() {
    let dir = setup_git_project();

    // Rewrite validate() with real behavioral changes. Not guarding the
    // risk level — the point is to exercise the populated-review path.
    fs::write(
        dir.path().join("src/lib.rs"),
        r#"
/// Entry point.
pub fn main() {
    process(42);
}

/// Process input with a transform now.
pub fn process(input: i32) -> i32 {
    let base = validate(input);
    base.saturating_mul(2)
}

/// Check input — now returns the input unchanged.
fn validate(input: i32) -> i32 {
    input
}
"#,
    )
    .expect("rewrite lib.rs");

    // Use --gate off so risk level doesn't drive exit code — we just want
    // the JSON envelope to populate.
    let output = cqs()
        .args(["ci", "--json", "--gate", "off"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to run cqs ci");

    assert!(
        output.status.success(),
        "ci should exit 0 with --gate off. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // Envelope shape
    assert_eq!(parsed["version"], 1, "envelope version should be 1");
    assert!(
        parsed["error"].is_null(),
        "ci happy path should produce no envelope error"
    );
    assert!(
        parsed["data"].is_object(),
        "data must be an object, got {}",
        parsed["data"]
    );

    // CiReport shape — three top-level fields. Presence matters more than
    // exact counts (which depend on how the parser chunks the diff).
    let data = &parsed["data"];
    assert!(
        data["review"].is_object(),
        "data.review must be an object, got {}",
        data["review"]
    );
    assert!(
        data["gate"].is_object(),
        "data.gate must be an object, got {}",
        data["gate"]
    );
    assert!(
        data["dead_in_diff"].is_array() || data["dead_in_diff"].is_object(),
        "data.dead_in_diff must be present, got {}",
        data["dead_in_diff"]
    );

    // Review subtree: risk_summary and reviewed sections must be there.
    let review = &data["review"];
    assert!(
        review["risk_summary"].is_object(),
        "review.risk_summary must be an object, got {}",
        review["risk_summary"]
    );

    // Gate subtree: `passed` must be true (we set --gate off explicitly).
    assert_eq!(
        data["gate"]["passed"],
        serde_json::json!(true),
        "--gate off must report gate.passed=true even with risky changes"
    );
}
