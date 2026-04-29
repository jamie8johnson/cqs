//! TC-HAP-1.30.1-3 — Integration tests for `cqs status`.
//!
//! Pins the 6-row behaviour matrix from the `cmd_status` docstring
//! (`src/cli/commands/infra/status.rs:24-37`). A regression that swaps
//! `state:` and `modified_files=` lines, drops `--watch-fresh`, or
//! changes the no-daemon exit-code goes undetected without these.
//!
//! Subprocess pattern (mirrors `tests/cli_ref_test.rs`):
//! - `assert_cmd::Command::cargo_bin("cqs")` for binary invocation.
//! - Per-test `$XDG_RUNTIME_DIR` so the daemon-socket lookup never
//!   collides with the dev machine's real socket.
//! - File-level `slow-tests` gate matches existing convention; the
//!   subprocess cold-starts the cqs binary (~2 s) which is slow enough
//!   to keep out of the default suite.
//!
//! These tests cover the *no-daemon* paths only. Daemon-up paths (rows
//! 4-6 of the matrix) require a `UnixListener` mock and are tracked
//! separately — the no-daemon failure modes are the highest-value pin
//! because they're what scripts hit when they forget to start the
//! daemon.

#![cfg(feature = "slow-tests")]
#![cfg(unix)]

#[test]
fn cqs_status_no_flag_exits_one_with_gate_message() {
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .arg("status")
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "status without --watch-fresh must exit 1",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--watch-fresh"),
        "stderr should hint at --watch-fresh, got: {}",
        stderr,
    );
}

#[test]
fn cqs_status_wait_without_watch_fresh_exits_one() {
    // `--wait` requires `--watch-fresh` per the matrix. Test that the
    // gate fires before the daemon is even probed.
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .args(["status", "--wait"])
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn cqs_status_watch_fresh_without_daemon_exits_one_with_friendly_msg() {
    // `--watch-fresh` with no daemon (empty XDG_RUNTIME_DIR has no
    // socket) must surface a friendly error and exit 1.
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .args(["status", "--watch-fresh"])
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The "cqs:" prefix is the conventional error format from
    // `emit_no_daemon` in `infra/status.rs`.
    assert!(
        stderr.contains("cqs:") || stderr.contains("daemon"),
        "stderr should describe the no-daemon condition, got: {}",
        stderr,
    );
}

#[test]
fn cqs_status_watch_fresh_json_no_daemon_emits_error_envelope() {
    // JSON mode must surface the no-daemon error via the envelope shape
    // (parity with `cqs ping --json`), not as a free-form text line.
    let tmp = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::cargo_bin("cqs")
        .unwrap()
        .args(["status", "--watch-fresh", "--json"])
        .env("XDG_RUNTIME_DIR", tmp.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The JSON error envelope writes an `"error"` key. We don't pin the
    // full shape because the envelope module owns its layout — any
    // valid envelope key satisfies the regression bar.
    assert!(
        stdout.contains("\"error\"") || stdout.contains("\"code\""),
        "stdout should contain a JSON error envelope, got: {}",
        stdout,
    );
}
