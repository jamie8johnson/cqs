//! TC-HAP-V1.38-7 (#1463): integration tests for `cqs hook install` and
//! `cqs hook status` exercised end-to-end via the binary.
//!
//! Pre-fix, the `cmd_install` and `cmd_status` wrappers around
//! `write_hook_script` (each handling CWD discovery, JSON-vs-text
//! branching, marker version detection, and per-hook dispatch) had
//! ZERO direct tests. The unit tests at `src/cli/commands/infra/hook.rs`
//! drove `write_hook_script` directly because `cmd_install` resolves the
//! project root from the workspace, not the temp dir — sidestepping the
//! very thing that breaks under refactor.
//!
//! These tests construct a minimal `.git/hooks/`-bearing tempdir, run
//! `cqs hook install` / `status` via `assert_cmd::Command::current_dir`,
//! and assert against the JSON envelope shape.
//!
//! No embedder needed — `cqs hook` does not load ONNX. No `slow-tests`
//! gate.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

const MANAGED_HOOKS: &[&str] = &["post-checkout", "post-merge", "post-rewrite"];
const HOOK_MARKER_PREFIX: &str = "# cqs:hook";

fn cqs() -> Command {
    #[allow(deprecated)]
    let mut c = Command::cargo_bin("cqs").expect("Failed to find cqs binary");
    // Kept-v1 compat set: the default wire shape is V2Bare since
    // v1.40.0. These tests pin `CQS_OUTPUT_FORMAT=v1` to exercise the
    // surviving legacy-envelope contract, so `parsed["data"][...]`
    // assertions keep working. The bare default is asserted end-to-end in
    // tests/cli_envelope_test.rs, tests/cli_dead_test.rs, and
    // tests/cli_chat_format_test.rs.
    c.env("CQS_OUTPUT_FORMAT", "v1");
    c
}

/// Force CLI mode — different dev machines may have leftover daemon sockets.
fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// Build a minimal git-repo-shaped tempdir: `.git/hooks/` exists.
fn make_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    fs::create_dir_all(dir.path().join(".git").join("hooks")).expect("mkdir .git/hooks");
    // .git/HEAD makes some git tooling happier; not required for our flow.
    fs::write(
        dir.path().join(".git").join("HEAD"),
        "ref: refs/heads/main\n",
    )
    .expect("write HEAD");
    dir
}

/// `cqs hook install` writes all three managed hooks with the cqs marker.
#[test]
fn install_writes_managed_hooks_with_marker() {
    let dir = make_repo();

    let result = cqs_no_daemon()
        .args(["hook", "install", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs hook install");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    assert!(
        result.status.success(),
        "hook install must succeed; stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));
    let installed = parsed["data"]["installed"]
        .as_array()
        .unwrap_or_else(|| panic!("data.installed missing: {parsed:?}"));
    assert_eq!(
        installed.len(),
        MANAGED_HOOKS.len(),
        "expected all managed hooks installed: {parsed:?}"
    );

    // Every managed hook now exists on disk and carries the cqs marker.
    for &hook in MANAGED_HOOKS {
        let path = dir.path().join(".git").join("hooks").join(hook);
        let body = fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("hook file missing: {}", path.display()));
        assert!(
            body.contains(HOOK_MARKER_PREFIX),
            "hook {} missing marker prefix; body={body:?}",
            path.display()
        );
    }
}

/// `cqs hook install` is idempotent — second run reports the existing
/// hooks as already-installed (or upgraded), not as fresh installs.
#[test]
fn install_is_idempotent() {
    let dir = make_repo();

    cqs_no_daemon()
        .args(["hook", "install", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("first install");

    let result = cqs_no_daemon()
        .args(["hook", "install", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("second install");

    assert!(result.status.success(), "second install must succeed");
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));
    // First-install populates `installed`; idempotent re-run produces
    // an empty `installed` (with everything in `upgraded` or
    // `skipped_existing` depending on marker version) — pin the
    // bucket-shape contract here.
    let installed_len = parsed["data"]["installed"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(usize::MAX);
    assert_eq!(
        installed_len, 0,
        "second install must NOT re-classify hooks as fresh installs: {parsed:?}"
    );

    // Files must still carry the marker.
    for &hook in MANAGED_HOOKS {
        let path = dir.path().join(".git").join("hooks").join(hook);
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains(HOOK_MARKER_PREFIX));
    }
}

/// `cqs hook status` reports installed hooks accurately + classifies
/// foreign hooks as `foreign` (not `installed`) so `cqs hook install`
/// can later refuse to clobber them.
#[test]
fn status_classifies_installed_and_foreign() {
    let dir = make_repo();

    // Pre-create a foreign hook (no cqs marker) on the post-merge slot.
    let foreign_path = dir.path().join(".git").join("hooks").join("post-merge");
    fs::write(&foreign_path, "#!/bin/sh\n# someone else's hook\nexit 0\n")
        .expect("write foreign hook");

    // Install. `cmd_install` already refuses to clobber foreign
    // (no-marker) files by default — the existing post-merge stays
    // untouched, post-checkout + post-rewrite get installed fresh.
    cqs_no_daemon()
        .args(["hook", "install", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("install");

    let result = cqs_no_daemon()
        .args(["hook", "status", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("hook status");

    assert!(result.status.success(), "hook status must succeed");
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("--json output must be JSON. got: {stdout}"));

    let installed: Vec<&str> = parsed["data"]["installed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let foreign: Vec<&str> = parsed["data"]["foreign"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(
        installed.contains(&"post-checkout"),
        "post-checkout must be classified as installed: {parsed:?}"
    );
    assert!(
        installed.contains(&"post-rewrite"),
        "post-rewrite must be classified as installed: {parsed:?}"
    );
    assert!(
        foreign.contains(&"post-merge"),
        "third-party post-merge hook must be classified as foreign \
         (not installed) so install --no-overwrite refuses to clobber it: {parsed:?}"
    );
}
