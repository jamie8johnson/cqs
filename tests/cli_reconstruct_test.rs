//! Audit P3 #112 — `cmd_reconstruct` integration tests.
//!
//! `cqs reconstruct <path>` loads chunks for `path` from the index,
//! orders them by line, and prints the reassembled source. The inline
//! tests in `src/cli/commands/io/reconstruct.rs` cover only `assemble()`
//! over hand-built `ChunkSummary` slices and a serialization snapshot —
//! nothing pins the path from CLI argv → store query → printed output.
//!
//! These tests need an indexed project, so they cold-load the embedder
//! and are gated behind `slow-tests`. They use the subprocess pattern
//! (`cmd_reconstruct` takes `&CommandContext` which is `pub(crate)` and
//! cannot be constructed from an integration test).
//!
//! Coverage:
//! - relative path: `cqs reconstruct src/lib.rs --json` succeeds and
//!   round-trips function bodies inside `data.content`.
//! - absolute path: same path passed as `<tempdir>/src/lib.rs` is
//!   normalized via `Path::strip_prefix(root)` and resolves the same
//!   chunks (pins the absolute-path branch in `cmd_reconstruct:32-39`).
//! - unknown file: `cqs reconstruct src/no_such_file.rs --json` exits
//!   non-zero with an actionable error mentioning the missing path —
//!   pins the `bail!("No indexed chunks found for ...")` branch.

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

/// Build a tiny indexed project with two distinct functions in one file
/// so we can verify chunk reassembly preserves order + content.
fn setup_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n/// Subtracts two numbers.\npub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
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

/// Relative path: `cqs reconstruct src/lib.rs --json` returns an envelope
/// with `data.file` matching the input and `data.content` containing both
/// function signatures. Pins the relative-path branch in `cmd_reconstruct`
/// (the `else` arm at line 38: `normalize_path(Path::new(path))`).
#[test]
#[serial]
fn test_reconstruct_relative_path_emits_envelope_with_content() {
    let dir = setup_project();

    let output = cqs()
        .args(["reconstruct", "src/lib.rs", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("cqs reconstruct failed to spawn");

    assert!(
        output.status.success(),
        "reconstruct should succeed. stdout={} stderr={}",
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

    // ReconstructOutput inner shape
    assert_eq!(
        parsed["data"]["file"], "src/lib.rs",
        "file must round-trip the normalized rel path"
    );
    assert!(
        parsed["data"]["chunks"].as_u64().unwrap_or(0) >= 2,
        "expected at least 2 chunks (add + sub), got: {}",
        parsed["data"]["chunks"]
    );
    assert!(
        parsed["data"]["lines"].as_u64().unwrap_or(0) > 0,
        "lines must be > 0, got: {}",
        parsed["data"]["lines"]
    );

    let content = parsed["data"]["content"]
        .as_str()
        .expect("content must be a string");
    assert!(
        content.contains("pub fn add"),
        "content must include 'add'. got: {content}"
    );
    assert!(
        content.contains("pub fn sub"),
        "content must include 'sub'. got: {content}"
    );
}

/// Absolute path: pass `<tempdir>/src/lib.rs` and confirm it resolves the
/// same chunks as the relative form. Pins the `Path::is_absolute()` branch
/// at `cmd_reconstruct:32-37` — `strip_prefix(root)` should normalize to
/// the relative form before the store lookup.
#[test]
#[serial]
fn test_reconstruct_absolute_path_inside_project_resolves_same_chunks() {
    let dir = setup_project();
    let abs_path = dir.path().join("src/lib.rs");
    let abs_str = abs_path.to_str().expect("abs path must be UTF-8");

    let output = cqs()
        .args(["reconstruct", abs_str, "--json"])
        .current_dir(dir.path())
        .output()
        .expect("cqs reconstruct failed to spawn");

    assert!(
        output.status.success(),
        "absolute-path reconstruct should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    // After strip_prefix(root) + normalize_path, the file field must be the
    // relative form even though we passed an absolute path. (On Windows the
    // separator gets normalized to '/' by normalize_path.)
    assert_eq!(
        parsed["data"]["file"], "src/lib.rs",
        "absolute path must normalize to relative 'src/lib.rs'"
    );
    let content = parsed["data"]["content"]
        .as_str()
        .expect("content must be a string");
    assert!(
        content.contains("pub fn add") && content.contains("pub fn sub"),
        "absolute and relative paths must yield the same content"
    );
}

/// Unknown file: pins the `bail!("No indexed chunks found for ...")`
/// branch at `cmd_reconstruct:42-46`. The error must mention the path
/// the user passed so they can correct the typo.
#[test]
#[serial]
fn test_reconstruct_unknown_file_errors_with_actionable_message() {
    let dir = setup_project();

    let output = cqs()
        .args(["reconstruct", "src/no_such_file_xyz.rs", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("cqs reconstruct failed to spawn");

    assert!(
        !output.status.success(),
        "reconstruct on unknown file must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Don't bind exact phrasing — just confirm the message references the
    // missing path or the failure mode (matches `bail!` text or anyhow chain).
    let stderr_lc = stderr.to_lowercase();
    assert!(
        stderr.contains("no_such_file_xyz")
            || stderr_lc.contains("no indexed chunks")
            || stderr_lc.contains("not found")
            || stderr_lc.contains("index"),
        "stderr should explain the failure. got: {stderr}"
    );
}
