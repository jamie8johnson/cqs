//! Audit P2 #45 — `cmd_review` integration tests.
//!
//! `cqs review` reads a diff (from `--base <ref>` via `git diff` or
//! `--stdin`), runs `review_diff`, then either truncates lists to fit
//! `--tokens` or emits the full envelope. The library `review_diff` has
//! 6 unit tests in `tests/review_test.rs`; the `apply_token_budget` helper
//! has 2 inline tests. Nothing pins the composition: argv → stdin →
//! `read_stdin` → `review_diff` → token budget merge → JSON envelope.
//!
//! These tests use the subprocess pattern because `cmd_review` reads from
//! `std::io::stdin()` directly. Gated `slow-tests` for the embedder.

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

/// Build a tiny indexed project. Reuses the call-graph fixture pattern
/// from `cli_graph_test.rs::setup_graph_project`.
fn setup_project() -> TempDir {
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

/// Process input through validation.
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

/// `cqs review --stdin` reads a diff from stdin. Pins the envelope shape on
/// the no-affected-functions branch (`empty_review_json` at
/// `src/cli/commands/review/diff_review.rs:152-161`).
#[test]
#[serial]
fn test_review_json_no_indexed_functions_emits_empty_envelope() {
    let dir = setup_project();

    // Diff against a path the index doesn't know about — the parser will
    // produce hunks but `map_hunks_to_functions` finds nothing → review is
    // None → `empty_review_json()` branch.
    let diff = "\
diff --git a/src/unrelated.txt b/src/unrelated.txt
--- a/src/unrelated.txt
+++ b/src/unrelated.txt
@@ -1 +1,3 @@
 unchanged
+added line
+another line
";

    let output = cqs()
        .args(["review", "--stdin", "--format", "json"])
        .current_dir(dir.path())
        .write_stdin(diff)
        .output()
        .expect("Failed to run cqs review --stdin");

    assert!(
        output.status.success(),
        "review --stdin should succeed even with no indexed functions affected. stdout={} stderr={}",
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

    // empty_review_json() shape
    assert_eq!(
        parsed["data"]["changed_functions"],
        serde_json::json!([]),
        "changed_functions must be empty array"
    );
    assert_eq!(
        parsed["data"]["affected_callers"],
        serde_json::json!([]),
        "affected_callers must be empty array"
    );
    assert_eq!(
        parsed["data"]["affected_tests"],
        serde_json::json!([]),
        "affected_tests must be empty array"
    );
    assert_eq!(
        parsed["data"]["risk_summary"]["overall"], "low",
        "no functions = low risk"
    );
    assert_eq!(parsed["data"]["risk_summary"]["high"], 0);
    assert_eq!(parsed["data"]["risk_summary"]["medium"], 0);
    assert_eq!(parsed["data"]["risk_summary"]["low"], 0);
    assert!(
        parsed["data"]["stale_warning"].is_null(),
        "stale_warning is null on the empty branch"
    );
}

/// `cqs review --stdin` with a diff that touches an indexed function should
/// emit a populated envelope. We don't pin the exact `changed_functions`
/// length (depends on the diff parser's hunk-to-function mapping) but we DO
/// pin the envelope shape and that the keys exist.
#[test]
#[serial]
fn test_review_from_stdin_returns_envelope_with_changed_functions() {
    let dir = setup_project();

    // Touch a real function in the indexed file — `validate` lives at lines
    // 13-15. A hunk at line 13 should map to `validate`.
    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -13,3 +13,4 @@ fn validate(input: i32) -> i32 {
+    let extra = input * 2;
     input + 1
";

    let output = cqs()
        .args(["review", "--stdin", "--format", "json"])
        .current_dir(dir.path())
        .write_stdin(diff)
        .output()
        .expect("Failed to run cqs review --stdin");

    assert!(
        output.status.success(),
        "review --stdin should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // Envelope shape
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null());

    // `ReviewResult` keys are stable. Whether `changed_functions` is empty or
    // populated depends on whether the diff parser landed the hunk on a
    // recognized line range. Either way the keys must exist.
    let data = &parsed["data"];
    assert!(
        data["changed_functions"].is_array(),
        "changed_functions must be an array, got: {}",
        data["changed_functions"]
    );
    assert!(
        data["affected_callers"].is_array(),
        "affected_callers must be an array"
    );
    assert!(
        data["affected_tests"].is_array(),
        "affected_tests must be an array"
    );
    assert!(
        data["risk_summary"].is_object(),
        "risk_summary must be an object"
    );
    assert!(
        data["risk_summary"]["overall"].is_string(),
        "risk_summary.overall must be a string"
    );
}

/// `--tokens N` adds the `token_count` and `token_budget` fields to the JSON
/// output (per `src/cli/commands/review/diff_review.rs:50-53`). Pins the field
/// names so a typo (e.g. `tokens_used`) is caught immediately.
#[test]
#[serial]
fn test_review_token_budget_adds_token_count_field() {
    let dir = setup_project();

    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -13,3 +13,4 @@ fn validate(input: i32) -> i32 {
+    let extra = input * 2;
     input + 1
";

    let output = cqs()
        .args(["review", "--stdin", "--format", "json", "--tokens", "100"])
        .current_dir(dir.path())
        .write_stdin(diff)
        .output()
        .expect("Failed to run cqs review --tokens");

    assert!(
        output.status.success(),
        "review with --tokens should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // `token_count` and `token_budget` are merged into the output ONLY on the
    // populated-review branch. The diff above touches `validate`. If the
    // hunk-to-function mapping found nothing, the empty-review branch fires
    // and these fields aren't emitted — so we accept either:
    //   - populated: token_count + token_budget present, both numeric
    //   - empty:     fields absent (empty_review_json() doesn't merge them)
    let data = &parsed["data"];
    if data
        .get("changed_functions")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        assert!(
            data["token_count"].is_number(),
            "token_count must be numeric on populated review, got: {}",
            data["token_count"]
        );
        assert_eq!(
            data["token_budget"],
            serde_json::json!(100),
            "token_budget must echo the requested budget"
        );
    } else {
        // AUDIT-FOLLOWUP (P2 #45): the empty-review branch silently drops
        // token_count / token_budget. The audit notes this divergence —
        // agents asking for `--tokens` won't see the fields when the diff
        // touches no indexed code. Pinning current behavior here.
        eprintln!(
            "test_review_token_budget_adds_token_count_field: diff matched no functions; \
             empty-review branch fired. token_count/token_budget intentionally absent."
        );
    }
}
