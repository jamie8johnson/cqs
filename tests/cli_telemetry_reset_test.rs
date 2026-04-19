//! Audit P3 #118 — `cmd_telemetry_reset` integration tests pinning current
//! non-atomic copy-then-truncate behaviour.
//!
//! `cqs telemetry --reset` archives the current `telemetry.jsonl` to a
//! timestamped file, then truncates the original and writes a single
//! `{"event":"reset",...}` entry. The two-step `fs::copy` → `fs::write`
//! at `src/cli/commands/infra/telemetry_cmd.rs:552-567` is NOT atomic:
//! a crash between the copy and the write loses the live log.
//!
//! These tests do NOT fix the non-atomicity (a separate agent owns that
//! fix). They pin the current happy-path behaviour so the fix has a
//! regression net:
//! - the original `telemetry.jsonl` lines land in the archive,
//! - the post-reset `telemetry.jsonl` contains exactly one reset entry
//!   carrying the supplied reason, and
//! - the archive file name follows the `telemetry_YYYYMMDD_HHMMSS.jsonl`
//!   pattern produced by `format_utc_timestamp`.
//!
//! The fix should make the test assertions tighter (atomic-rename naming,
//! no observable half-state) without breaking these pins. We also assert
//! that the source today does NOT yet route through any `atomic_replace`
//! helper — so the fix can demonstrably swap the implementation.
//!
//! These tests do NOT need an indexed project — `cmd_telemetry_reset`
//! works on the raw `<cqs_dir>/telemetry.jsonl` path with no embedder
//! load. NOT gated `slow-tests`.

use assert_cmd::Command;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Build a tempdir with `.cqs/telemetry.jsonl` containing a few command
/// entries. The reset path uses `find_project_root` which falls back to
/// CWD when no project markers exist; since we set `current_dir` to the
/// tempdir, cqs_dir resolves to `<tempdir>/.cqs/`.
fn setup_telemetry_dir(lines: &[&str]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("mkdir .cqs");
    let telem_path = cqs_dir.join("telemetry.jsonl");
    let body: String = lines
        .iter()
        .map(|l| format!("{l}\n"))
        .collect::<Vec<_>>()
        .join("");
    fs::write(&telem_path, body).expect("write telemetry.jsonl");
    dir
}

/// Happy path: existing telemetry → reset archives + writes single reset
/// entry. Pins:
/// - archive file appears with `telemetry_*.jsonl` shape
/// - archive content equals the original line set
/// - post-reset `telemetry.jsonl` is exactly one JSONL line with
///   `event=reset` and the supplied `reason`.
#[test]
#[serial]
fn test_telemetry_reset_archives_and_truncates() {
    let original_lines = [
        r#"{"cmd":"search","query":"foo","ts":1700000000}"#,
        r#"{"cmd":"impact","query":"bar","ts":1700000005}"#,
        r#"{"cmd":"callers","query":"baz","ts":1700000010}"#,
    ];
    let dir = setup_telemetry_dir(&original_lines);
    let cqs_dir = dir.path().join(".cqs");
    let telem_path = cqs_dir.join("telemetry.jsonl");

    let original_body = fs::read_to_string(&telem_path).expect("read original");

    // CQS_TELEMETRY=0 hard-disables the dispatch-level `log_command` that
    // would otherwise write a `{"cmd":"telemetry",...}` entry into the
    // live file BEFORE our reset handler runs (per `cli/telemetry.rs:70-78`).
    // Without this opt-out, the archive contains an extra row and the
    // assertion below races with the wall clock.
    let output = cqs()
        .args(["telemetry", "--reset", "--reason", "audit-p3-118-test"])
        .env("CQS_TELEMETRY", "0")
        .current_dir(dir.path())
        .output()
        .expect("cqs telemetry --reset failed to spawn");

    assert!(
        output.status.success(),
        "telemetry --reset should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The archive should exist alongside the original. Pattern:
    // `telemetry_YYYYMMDD_HHMMSS.jsonl` per `format_utc_timestamp`.
    let archives: Vec<_> = fs::read_dir(&cqs_dir)
        .expect("read .cqs dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("telemetry_") && name.ends_with(".jsonl")
        })
        .collect();
    assert_eq!(
        archives.len(),
        1,
        "expected exactly one archive file, got {}",
        archives.len()
    );

    let archive_path = archives[0].path();
    let archive_body = fs::read_to_string(&archive_path).expect("read archive");
    assert_eq!(
        archive_body, original_body,
        "archive must equal original telemetry content (fs::copy preserves byte-for-byte)"
    );

    // Validate the timestamp segment shape: 8 digits + '_' + 6 digits.
    let archive_name = archive_path.file_name().unwrap().to_string_lossy();
    let stem = archive_name
        .strip_prefix("telemetry_")
        .and_then(|s| s.strip_suffix(".jsonl"))
        .expect("archive name should match telemetry_*.jsonl");
    assert_eq!(
        stem.len(),
        15,
        "archive timestamp must be YYYYMMDD_HHMMSS (15 chars), got {stem:?}"
    );
    assert_eq!(
        stem.as_bytes()[8],
        b'_',
        "archive timestamp must have '_' between date and time"
    );

    // Post-reset telemetry.jsonl: exactly one line, parses as the reset event
    // with the supplied reason.
    let post_body = fs::read_to_string(&telem_path).expect("read post-reset telemetry");
    let lines: Vec<&str> = post_body.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "post-reset telemetry must contain exactly one line. got: {post_body:?}"
    );
    let reset_entry: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("reset entry must be valid JSON: {e}\nline={}", lines[0]));
    assert_eq!(
        reset_entry["event"], "reset",
        "first (only) line must be a reset event"
    );
    assert_eq!(
        reset_entry["reason"], "audit-p3-118-test",
        "reset event must echo the --reason argument"
    );
    assert!(
        reset_entry["ts"].is_number(),
        "reset event must carry a numeric ts field"
    );
}

/// Pins the missing-file branch at `telemetry_cmd.rs:524-527`. With no
/// existing telemetry file, reset is a no-op that prints "No telemetry
/// file to reset." and exits 0. The audit-fix should preserve this — the
/// reset path should never fail when there is nothing to reset.
#[test]
#[serial]
fn test_telemetry_reset_no_file_is_noop() {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("mkdir .cqs");

    // CQS_TELEMETRY=0 prevents the dispatch-level log from creating the
    // file before our handler runs.
    let output = cqs()
        .args(["telemetry", "--reset"])
        .env("CQS_TELEMETRY", "0")
        .current_dir(dir.path())
        .output()
        .expect("cqs telemetry --reset failed to spawn");

    assert!(
        output.status.success(),
        "reset on missing file should be a no-op success. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No telemetry file to reset"),
        "no-op path should print the explanatory line. got: {stdout}"
    );

    // No archive should be created.
    let archives: Vec<_> = fs::read_dir(&cqs_dir)
        .expect("read .cqs dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("telemetry_") && name.ends_with(".jsonl")
        })
        .collect();
    assert!(
        archives.is_empty(),
        "no-op reset must not create an archive. got: {archives:?}"
    );
}

/// Pins (b) from the audit prompt: today the source uses `fs::copy` +
/// `fs::write`, NOT `crate::fs::atomic_replace`. This test asserts that
/// observable property by re-reading the source. When the fix lands and
/// the implementation switches to an atomic helper, this test will fail
/// — at which point the fix author should DELETE this test (it was a
/// pre-fix marker, not a permanent contract).
///
/// This is a documentation-of-record test: it pins the audit observation
/// in the test suite so a future grep finds it next to the behavioural
/// tests above.
#[test]
fn test_telemetry_reset_currently_uses_non_atomic_copy_then_write() {
    let src_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/cli/commands/infra/telemetry_cmd.rs");
    let src = fs::read_to_string(&src_path).expect("read telemetry_cmd.rs");

    // The implementation today uses `fs::copy(&current, &archive)` followed
    // by `fs::write(&current, ...)`. Both calls live inside `cmd_telemetry_reset`.
    assert!(
        src.contains("fs::copy(&current, &archive)"),
        "audit P3 #118: telemetry_cmd.rs must currently use fs::copy for archive. \
         If this test fails after a fix lands, DELETE this test — the audit-finding \
         was resolved."
    );
    assert!(
        src.contains("fs::write(&current,"),
        "audit P3 #118: telemetry_cmd.rs must currently use fs::write to truncate. \
         Same instruction: if this fails after the fix, DELETE the test."
    );
    // Sanity: the audit-fix candidate `atomic_replace` is NOT yet in this
    // file. When the fix routes through it, this assertion flips and the
    // test should be removed.
    assert!(
        !src.contains("atomic_replace"),
        "audit P3 #118: source must not yet route through atomic_replace. \
         If it does, the fix has landed — DELETE this test."
    );
}
