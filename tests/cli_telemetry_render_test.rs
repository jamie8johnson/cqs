//! End-to-end guard for the `cqs telemetry` text renderer's kind-fallback
//! rate branch.
//!
//! The renderer in `print_telemetry_text` chooses between two displays per
//! command in the `Kind Fallbacks` section:
//!
//! - `fb_count <= cmd_total` → a per-invocation percentage
//!   (`N / M  (P% of invocations)`), and
//! - `fb_count >  cmd_total` → a bare-count fallback line
//!   (`N / M  (fallbacks / invocations)`), because a fan-out invocation can
//!   fire more fallbacks than there are invocations, and a percentage there
//!   would read >100% — the misleading output the branch exists to avoid.
//!
//! That branch is `println!`-only (the function returns `()`), so the
//! sibling data-layer unit tests — which assert `fallbacks <= invocations`
//! on the aggregated `TelemetryOutput` — pass whether or not the renderer
//! picks the right branch. Flipping the comparison (`<=` → `>`) in
//! `print_telemetry_text` leaves every unit test green while the dashboard
//! prints `150% of invocations`. This re-execs the real binary and reads
//! the rendered stdout so the branch is actually constrained.

mod common;

use common::cqs_v1 as cqs;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

/// Build a tempdir whose `.cqs/telemetry.jsonl` holds the given lines.
/// `find_project_root` falls back to CWD when no markers exist, and the
/// child's `current_dir` is the tempdir, so cqs_dir resolves to
/// `<tempdir>/.cqs/`.
fn setup(lines: &[&str]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir(&cqs_dir).expect("mkdir .cqs");
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(cqs_dir.join("telemetry.jsonl"), body).expect("write telemetry.jsonl");
    dir
}

/// Extract the `Kind Fallbacks` section lines from rendered text output.
fn fallback_section(stdout: &str) -> String {
    let mut out = String::new();
    let mut in_section = false;
    for line in stdout.lines() {
        if line.contains("Kind Fallbacks") {
            in_section = true;
            continue;
        }
        if in_section {
            // Section ends at the next blank line or top-level header.
            if line.trim().is_empty() || line.starts_with("Sessions:") {
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// When a single command's fallback count EXCEEDS its invocation count
/// (fan-out: two `callers` invocations, three fallbacks fired within one of
/// them), the renderer must show the bare-count line, NOT a >100% percentage.
///
/// Bites the `fb_count <= cmd_total` → `fb_count > cmd_total` mutation: under
/// it the renderer enters the percentage branch and prints
/// `(150% of invocations)`. This test then sees a `% of invocations` token
/// in the fallback section and fails. On real code the section carries
/// `(fallbacks / invocations)` and no percentage.
#[test]
#[serial]
fn fallback_over_invocations_renders_bare_count_not_over_100_percent() {
    let dir = setup(&[
        r#"{"cmd":"callers","query":"a","ts":1700000001}"#,
        r#"{"cmd":"callers","query":"b","ts":1700000002}"#,
        // Three fallbacks all tagged with the top-level command `callers`
        // (a fan-out within the second invocation). 3 fallbacks > 2 invokes.
        r#"{"event":"kind_fallback","cmd":"callers","fallback_from":"impact","kind":"const","name":"X","definitions":1,"ts":1700000002}"#,
        r#"{"event":"kind_fallback","cmd":"callers","fallback_from":"deps","kind":"const","name":"X","definitions":1,"ts":1700000002}"#,
        r#"{"event":"kind_fallback","cmd":"callers","fallback_from":"trace","kind":"const","name":"X","definitions":1,"ts":1700000002}"#,
    ]);

    let output = cqs()
        .arg("telemetry")
        .env("CQS_TELEMETRY", "0")
        .env("CQS_NO_DAEMON", "1")
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .output()
        .expect("spawn cqs telemetry");

    assert!(
        output.status.success(),
        "telemetry render should succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let section = fallback_section(&stdout);

    assert!(
        section.contains("callers") && section.contains("3 / 2"),
        "expected a `callers  3 / 2` fallback line, got section:\n{section}\nfull:\n{stdout}"
    );
    assert!(
        section.contains("fallbacks / invocations"),
        "fb-count exceeding invocations must render the bare-count label, \
         got section:\n{section}"
    );
    // The load-bearing guard: NO per-invocation percentage may appear for a
    // command whose fallback count exceeds its invocations — that is exactly
    // the misleading >100% the branch suppresses.
    assert!(
        !section.contains("% of invocations"),
        "fb-count exceeding invocations must NOT render a `% of invocations` \
         rate (would be the misleading >100% output), got section:\n{section}"
    );
}

/// When the fallback count is WITHIN the invocation count (one fallback over
/// two invocations), the renderer shows the per-invocation percentage. Pins
/// the OTHER side of the branch: the `<= cmd_total` arm must produce the
/// percentage (here 50%), so a mutation that swaps `>`/`==`/`<` on the
/// comparison or deletes the percentage arm is caught.
#[test]
#[serial]
fn fallback_within_invocations_renders_percentage() {
    let dir = setup(&[
        r#"{"cmd":"callers","query":"a","ts":1700000001}"#,
        r#"{"cmd":"callers","query":"b","ts":1700000002}"#,
        r#"{"event":"kind_fallback","cmd":"callers","fallback_from":"impact","kind":"const","name":"X","definitions":1,"ts":1700000002}"#,
    ]);

    let output = cqs()
        .arg("telemetry")
        .env("CQS_TELEMETRY", "0")
        .env("CQS_NO_DAEMON", "1")
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .output()
        .expect("spawn cqs telemetry");

    assert!(output.status.success(), "telemetry render should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let section = fallback_section(&stdout);

    assert!(
        section.contains("50% of invocations"),
        "one fallback over two invocations must render `50% of invocations`, \
         got section:\n{section}\nfull:\n{stdout}"
    );
}
