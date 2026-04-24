//! TC-HAP-1.29-9 — `cmd_gather` CLI integration tests.
//!
//! `cqs gather` runs seed search → BFS call-graph expansion → chunk pack.
//! The library-level pipeline (`cqs::gather`, `cqs::gather_cross_index_with_index`)
//! has targeted unit coverage in `tests/gather_test.rs`, and the dispatch
//! handler has its shape pinned in `tests/batch_handlers_test.rs`. What was
//! NOT covered: the CLI wrapper in `src/cli/commands/search/gather.rs` —
//! argv parsing, embedder load, envelope wrap, token-budget merge, ANSI
//! output in text mode.
//!
//! Subprocess pattern + `slow-tests` gate because `cmd_gather` cold-loads
//! the embedder (~2-5 s).

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

/// Set up a tiny indexed project with a handful of functions and a
/// plausible call graph. `cqs gather` needs code to seed-search on and
/// a call graph to BFS-expand.
fn setup_gather_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Parse a JSON configuration file from disk.
pub fn parse_config(path: &str) -> String {
    let content = read_file(path);
    validate_json(&content);
    content
}

/// Read a file as a UTF-8 string.
pub fn read_file(path: &str) -> String {
    std::fs::read_to_string(path).expect("read failed")
}

/// Validate that the given text is well-formed JSON.
pub fn validate_json(content: &str) -> bool {
    !content.is_empty()
}

/// Completely unrelated helper — should NOT appear in a gather on JSON.
pub fn multiply(a: i32, b: i32) -> i32 {
    a * b
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

/// JSON mode: pin the envelope shape and confirm that `chunks`, `query`,
/// `expansion_capped`, and `search_degraded` fields are all present.
#[test]
#[serial]
fn test_gather_json_returns_gather_output_envelope() {
    let dir = setup_gather_project();

    let output = cqs()
        .args(["gather", "parse JSON config", "--json", "-n", "3"])
        .current_dir(dir.path())
        .output()
        .expect("cqs gather failed to spawn");

    assert!(
        output.status.success(),
        "gather should succeed. stdout={} stderr={}",
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

    // GatherOutput inner shape
    let data = &parsed["data"];
    assert_eq!(
        data["query"], "parse JSON config",
        "query must echo the requested query"
    );
    assert!(
        data["chunks"].is_array(),
        "chunks must be an array: {stdout}"
    );
    assert!(
        data["expansion_capped"].is_boolean(),
        "expansion_capped must be boolean: {stdout}"
    );
    assert!(
        data["search_degraded"].is_boolean(),
        "search_degraded must be boolean: {stdout}"
    );
}

/// `--tokens` flag activates token-budget packing: `token_count` and
/// `token_budget` appear in the envelope. Without `--tokens` those fields
/// are omitted (they `skip_serializing_if = "Option::is_none"`).
#[test]
#[serial]
fn test_gather_with_tokens_flag_adds_token_fields() {
    let dir = setup_gather_project();

    let output = cqs()
        .args([
            "gather",
            "parse config",
            "--json",
            "-n",
            "3",
            "--tokens",
            "500",
        ])
        .current_dir(dir.path())
        .output()
        .expect("cqs gather --tokens failed to spawn");

    assert!(
        output.status.success(),
        "gather --tokens should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    let data = &parsed["data"];
    assert!(
        data["token_count"].is_number(),
        "data.token_count must be numeric when --tokens is set, got: {}",
        data["token_count"]
    );
    assert_eq!(
        data["token_budget"],
        serde_json::json!(500),
        "data.token_budget must echo the requested budget"
    );
}

/// Without `--tokens`, the two token fields must NOT appear in the JSON
/// envelope (they're serde-skipped when `None`).
#[test]
#[serial]
fn test_gather_without_tokens_omits_token_fields() {
    let dir = setup_gather_project();

    let output = cqs()
        .args(["gather", "parse config", "--json", "-n", "2"])
        .current_dir(dir.path())
        .output()
        .expect("cqs gather failed to spawn");

    assert!(output.status.success(), "gather should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    let data = &parsed["data"];
    assert!(
        data.get("token_count").is_none(),
        "token_count must be omitted without --tokens, got: {}",
        data.get("token_count").unwrap_or(&serde_json::Value::Null)
    );
    assert!(
        data.get("token_budget").is_none(),
        "token_budget must be omitted without --tokens, got: {}",
        data.get("token_budget").unwrap_or(&serde_json::Value::Null)
    );
}
