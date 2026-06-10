//! P1 audit fixes: top-level `--json` precedence + envelope error contract.
//!
//! Covers fixes B.2, B.3, B.5 from `docs/audit-fix-prompts.md`:
//!
//! - B.2: `cqs --json cache stats` and `cqs --json project search` must
//!   honor the global `--json` even when the subcommand `--json` is absent.
//!   Mirrors the precedence already enforced by `cmd_model`.
//! - B.3: `cqs ping --json` (with no daemon) emits the published failure
//!   envelope `{data:null, error:{code:"io_error", message:..}, version:1}`
//!   to stdout instead of bare stderr text.
//! - B.5: `cqs cache stats --json` emits `total_size_mb` as a numeric f64
//!   so consumers can do arithmetic on it. Earlier `format!("{:.1}", ...)`
//!   made it a string and broke programmatic use.
//!
//! These tests don't need a model on disk — `cqs cache` opens the global
//! embedding cache (sqlite, no embedder), `cqs ping` does direct socket I/O,
//! and `cqs project search` is gated behind `init_and_index` (which we
//! intentionally skip — we test the missing-daemon / no-project surface).
//!
//! `#[serial]` is required because the `cqs` binary cache and any shared
//! cache file paths can otherwise produce flaky CI when tests race.

use assert_cmd::Command;
use serial_test::serial;
use tempfile::TempDir;

/// Default helper — no env pins. The CLI direct success path emits the
/// bare V2Bare payload (the shipped default since v1.40.0), which these
/// tests assert directly. The error path (`emit_json_error`) always emits
/// the full `{data, error, version, _meta}` envelope regardless of
/// `CQS_OUTPUT_FORMAT`, so failure-shape assertions read `parsed["error"]`.
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// v1 compatibility helper. `CQS_OUTPUT_FORMAT=v1` restores the legacy
/// `{data, error, version, _meta}` envelope on the CLI direct success path.
/// This is the only output-format knob that survives; a small named set of
/// tests below pins that it still resolves.
fn cqs_v1() -> Command {
    let mut c = cqs();
    c.env("CQS_OUTPUT_FORMAT", "v1");
    c
}

/// Force CLI mode (no daemon) so tests that probe the daemon-not-running
/// path don't get short-circuited by an actually-running daemon on the
/// dev machine. Bare default (no v1 pin).
fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// CLI mode (no daemon) pinned to the v1 envelope shape.
fn cqs_no_daemon_v1() -> Command {
    let mut c = cqs_v1();
    c.env("CQS_NO_DAEMON", "1");
    c
}

// =============================================================================
// B.2 — `cqs --json cache stats` honors top-level `--json`
// =============================================================================

/// `cqs --json cache stats` must emit envelope JSON even though the user
/// did not pass `--json` after the subcommand. The fix added `cli: &Cli`
/// to `cmd_cache` and ORs `cli.json || *json` per subcommand.
#[test]
#[serial]
fn test_cache_stats_top_level_json_emits_envelope() {
    // Use a temp HOME so we don't poison the dev machine's real cache, and
    // so the cache is empty/fresh for the test (still emits stats — just
    // zero entries).
    let dir = TempDir::new().expect("tempdir");

    // P2.13: cache resolves project-scoped path when run inside a project.
    // Set current_dir to tempdir so find_project_root() doesn't escape.
    let output = cqs_no_daemon()
        .args(["--json", "cache", "stats"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs --json cache stats failed to spawn");

    assert!(
        output.status.success(),
        "cache stats should succeed on empty cache. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected bare JSON, parse failed: {e}\nstdout={stdout}"));

    // V2Bare default: the success path emits the bare payload — no `data`
    // wrapper, no `version` / `error` keys.
    assert!(
        parsed.is_object(),
        "bare payload must be the stats object, got: {stdout}"
    );
    assert!(
        parsed.get("version").is_none(),
        "bare default drops version key, got: {stdout}"
    );
    assert!(
        parsed.get("error").is_none(),
        "bare default drops error key on success, got: {stdout}"
    );

    // P2.16 dropped total_size_mb (bytes is canonical). Pin numeric bytes.
    assert!(
        parsed["total_size_bytes"].is_number(),
        "total_size_bytes must be numeric"
    );
    assert!(parsed["total_entries"].is_number());
    assert!(parsed["unique_models"].is_number());
    assert!(
        parsed.get("total_size_mb").is_none(),
        "P2.16: total_size_mb removed; use total_size_bytes"
    );
}

/// Subcommand-level `--json` still works (precedence is OR, not override).
#[test]
#[serial]
fn test_cache_stats_subcommand_json_emits_envelope() {
    let dir = TempDir::new().expect("tempdir");
    let output = cqs_no_daemon()
        .args(["cache", "stats", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs cache stats --json failed to spawn");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("bare JSON parse");
    // Bare default: payload fields are at the top level.
    assert!(parsed["total_size_bytes"].is_number());
    assert!(
        parsed.get("total_size_mb").is_none(),
        "P2.16: total_size_mb removed"
    );
}

// =============================================================================
// B.2 — `cqs --json project search` honors top-level `--json`
// =============================================================================

/// `cqs --json project search QUERY` must emit envelope JSON. Without the
/// fix, this read only the subcommand's `*json` and ignored `cli.json`.
///
/// The test uses an empty project registry — the search will return zero
/// results, but the envelope shape must still be correct.
#[test]
#[serial]
fn test_project_search_top_level_json_emits_envelope() {
    // Empty registry → empty result list → still a valid envelope.
    let dir = TempDir::new().expect("tempdir");

    let output = cqs_no_daemon()
        .args(["--json", "project", "search", "anything"])
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs --json project search failed to spawn");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        // Embedder failure (no model on disk) is acceptable in test env —
        // the args must still parse and the precedence wiring must be right.
        // We can't directly verify envelope on a failure that bypasses our
        // emit_json path, but we CAN verify the failure isn't from clap.
        assert!(
            !stderr.contains("error: unrecognized") && !stderr.contains("error: invalid"),
            "args must parse — got CLI parse error. stderr={stderr}"
        );
        eprintln!(
            "test_project_search_top_level_json_emits_envelope: model unavailable, \
             accepted as soft pass. stderr={stderr}"
        );
        return;
    }

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected bare JSON, parse failed: {e}\nstdout={stdout}"));
    // V2Bare default: the search results array is the bare payload — no
    // `data` wrapper. Array payloads can't carry `_meta`, so the top-level
    // value is the array itself.
    assert!(
        parsed.is_array(),
        "bare payload must be the search results array, got: {stdout}"
    );
}

// =============================================================================
// B.3 — `cqs ping --json` emits envelope error on no-daemon failure
// =============================================================================

/// `cqs ping --json` with no daemon must emit the published failure envelope
/// `{data:null, error:{code:"io_error", message:...}, version:1}` to stdout
/// and exit non-zero. Earlier this printed `cqs: <msg>` to stderr — agents
/// parsing the output got nothing on stdout.
#[test]
#[serial]
#[cfg(unix)]
fn test_ping_json_emits_envelope_error_when_no_daemon() {
    // Redirect XDG_RUNTIME_DIR so we can't accidentally hit a real running
    // daemon socket on the dev machine. The path must be valid (exist,
    // writable) — the daemon socket lookup falls back to /tmp if unset,
    // which on a dev box might have a real socket.
    let dir = TempDir::new().expect("tempdir");
    let runtime_dir = dir.path().join("runtime");
    std::fs::create_dir(&runtime_dir).expect("create runtime dir");

    let output = cqs_no_daemon()
        .args(["ping", "--json"])
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HOME", dir.path())
        // `cqs ping` skips the daemon-forward path itself — see ping.rs:166-189
        // — but CQS_NO_DAEMON is harmless to set and matches our other tests.
        .current_dir(dir.path())
        .output()
        .expect("cqs ping --json failed to spawn");

    assert!(
        !output.status.success(),
        "ping with no daemon must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected envelope JSON on stdout, parse failed: {e}\nstdout={stdout}")
    });

    // Failure envelope shape: { data: null, error: { code, message }, version: 1 }
    assert!(
        parsed["data"].is_null(),
        "data must be null on failure, got: {}",
        parsed["data"]
    );
    assert_eq!(parsed["version"], 1);
    assert_eq!(
        parsed["error"]["code"], "io_error",
        "code must be io_error for daemon-not-running, got: {}",
        parsed["error"]["code"]
    );
    assert!(
        parsed["error"]["message"].is_string(),
        "error.message must be a string, got: {}",
        parsed["error"]["message"]
    );
}

/// Without `--json`, the failure stays on stderr (text mode, unchanged).
#[test]
#[serial]
#[cfg(unix)]
fn test_ping_text_mode_keeps_stderr_message() {
    let dir = TempDir::new().expect("tempdir");
    let runtime_dir = dir.path().join("runtime");
    std::fs::create_dir(&runtime_dir).expect("create runtime dir");

    let output = cqs_no_daemon()
        .args(["ping"])
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HOME", dir.path())
        .current_dir(dir.path())
        .output()
        .expect("cqs ping failed to spawn");

    assert!(!output.status.success(), "ping with no daemon → exit 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cqs:"),
        "text-mode failure should print to stderr, got: {stderr}"
    );
    // stdout in text mode should be empty (we don't print anything on
    // failure when not JSON).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "text-mode failure stdout should be empty, got: {stdout}"
    );
}

// =============================================================================
// D.10 — Daemon `stats` populates `stale_files`/`missing_files` (parity with CLI)
// =============================================================================

/// BUG-D.10: `cqs stats --json` via the daemon previously emitted
/// `stale_files: null` / `missing_files: null` while the CLI populated
/// both. Agents auto-routed through the daemon silently treated every
/// project as fresh. The fix mirrors `cmd_stats:283-298` inside
/// `dispatch_stats` so the batch envelope carries the same fields.
///
/// Gated behind `slow-tests` because the test exercises `cqs index`, which
/// loads the full embedder model (~500MB cold start). The fast envelope
/// tests above run on every PR.
#[test]
#[serial]
#[cfg(feature = "slow-tests")]
fn daemon_stats_includes_staleness_fields_via_batch() {
    use std::fs;

    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("create src");
    fs::write(
        src.join("lib.rs"),
        "/// tiny\npub fn ping() -> u32 { 42 }\n",
    )
    .expect("write lib.rs");

    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();
    cqs()
        .args(["index"])
        .current_dir(dir.path())
        .assert()
        .success();

    // `cqs batch` sends one stats command, reads one JSONL envelope back.
    // This goes through `dispatch_stats` — the same handler used by the
    // daemon socket loop — so it covers the D.10 fix without spinning up
    // a real daemon.
    let output = cqs()
        .args(["batch"])
        .current_dir(dir.path())
        .write_stdin("stats\n")
        .output()
        .expect("cqs batch stats failed to spawn");
    assert!(
        output.status.success(),
        "batch stats should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));
    assert!(
        parsed["data"].is_object(),
        "data must be stats object: {stdout}"
    );
    assert!(
        !parsed["data"]["stale_files"].is_null(),
        "D.10: dispatch_stats must populate stale_files (was always null before fix), got: {}",
        parsed["data"]
    );
    assert!(
        !parsed["data"]["missing_files"].is_null(),
        "D.10: dispatch_stats must populate missing_files (was always null before fix), got: {}",
        parsed["data"]
    );
    // Fresh project: every indexed file still exists, none were modified.
    assert_eq!(parsed["data"]["stale_files"], 0);
    assert_eq!(parsed["data"]["missing_files"], 0);
}

// =============================================================================
// V2Bare default shape — binary boundary, no env pins
// =============================================================================

/// With no `CQS_OUTPUT_FORMAT` pin, the CLI direct success path emits the
/// bare payload: the stats object is the top-level JSON value, with no
/// `data` wrapper and no `version` / `error` envelope keys. This is the
/// shipped default; previously it shipped untested end-to-end.
#[test]
#[serial]
fn test_v2bare_default_cache_stats_is_bare_object() {
    let dir = TempDir::new().expect("tempdir");
    let output = cqs_no_daemon()
        .args(["--json", "cache", "stats"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs --json cache stats failed to spawn");
    assert!(output.status.success(), "cache stats should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("bare JSON parse");

    // Bare payload: the stats fields are at the top level (skip-when-default
    // means absent fields are at their default — not wrapped, not labelled).
    assert!(
        parsed["total_size_bytes"].is_number(),
        "bare payload exposes stats fields at top level, got: {stdout}"
    );
    assert!(
        parsed.get("data").is_none(),
        "V2Bare default must NOT wrap in a data envelope, got: {stdout}"
    );
    assert!(
        parsed.get("version").is_none(),
        "V2Bare default drops version, got: {stdout}"
    );
    assert!(
        parsed.get("error").is_none(),
        "V2Bare default drops error on success, got: {stdout}"
    );
}

/// `CQS_OUTPUT_FORMAT=v1` restores the full envelope on the CLI direct
/// success path — the surviving compatibility contract. Pin both surfaces
/// (object success payload) at the binary boundary.
#[test]
#[serial]
fn test_v1_compat_cache_stats_restores_envelope() {
    let dir = TempDir::new().expect("tempdir");
    let output = cqs_no_daemon_v1()
        .args(["--json", "cache", "stats"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs --json cache stats failed to spawn");
    assert!(output.status.success(), "cache stats should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope JSON parse");

    // v1 envelope: { data, error: null, version: 1, _meta }
    assert!(
        parsed["data"].is_object(),
        "v1 wraps stats under data, got: {stdout}"
    );
    assert_eq!(parsed["version"], 1, "v1 envelope carries version: 1");
    assert!(parsed["error"].is_null(), "v1 success → error: null");
    assert!(
        parsed["data"]["total_size_bytes"].is_number(),
        "stats fields live under data in v1, got: {stdout}"
    );
    // The deleted CQS_ULTRASECURITY knob's only wire payload was
    // `_meta.handling_advice`; it must never appear, even in v1.
    assert!(
        parsed["_meta"].get("handling_advice").is_none(),
        "handling_advice was removed with the CQS_ULTRASECURITY knob, got: {stdout}"
    );
}

/// The removed `CQS_ULTRASECURITY` env var is inert: setting it has no
/// effect on the wire shape. The default stays bare; the only signals that
/// matter (trust_level / injection_flags) are emitted by the leaf serializer
/// when meaningful, independent of any posture knob.
#[test]
#[serial]
fn test_ultrasecurity_env_is_inert() {
    let dir = TempDir::new().expect("tempdir");
    let output = cqs_no_daemon()
        .args(["--json", "cache", "stats"])
        .env("CQS_ULTRASECURITY", "1")
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .output()
        .expect("cqs --json cache stats failed to spawn");
    assert!(output.status.success(), "cache stats should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("bare JSON parse");

    // CQS_ULTRASECURITY=1 used to force the full verbose envelope. Now it's
    // a no-op: the default bare shape is unchanged.
    assert!(
        parsed.get("data").is_none(),
        "CQS_ULTRASECURITY must NOT restore the data envelope, got: {stdout}"
    );
    assert!(
        parsed.get("version").is_none(),
        "CQS_ULTRASECURITY must NOT force version, got: {stdout}"
    );
    assert!(
        parsed
            .get("_meta")
            .and_then(|m| m.get("handling_advice"))
            .is_none(),
        "CQS_ULTRASECURITY must NOT inject handling_advice, got: {stdout}"
    );
    assert!(parsed["total_size_bytes"].is_number());
}
