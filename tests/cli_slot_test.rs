#![cfg(feature = "slow-tests")]
//! Reason this file is gated: subprocess-spawning CLI tests for `cqs slot
//! {list,create,promote,remove,active}`. Each test runs the full cqs binary
//! cold start which is too expensive for PR-time CI. Run via
//! `cargo test --features slow-tests` or nightly ci-slow.yml.
//!
//! TC-HAP-V1.33-3: the slot subcommand surface (5 verbs) had zero
//! binary-level integration tests. `tests/slots_and_cache_integration.rs`
//! covers the library-level helpers but skips the entire `cmd_slot`
//! dispatch path including `--json` envelope shape per subcommand,
//! the `bail!` when global `--slot` is passed (P2.13), exit-code
//! stability, and `cqs slot promote` swapping the active pointer.

use assert_cmd::Command;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// Spin up an empty project with a `.cqs` directory so `find_project_root`
/// resolves to the temp dir.
fn setup_project() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir_all(&cqs_dir).expect("create .cqs dir");
    dir
}

fn run_slot_json(dir: &TempDir, args: &[&str]) -> Value {
    let output = cqs_no_daemon()
        .arg("--json")
        .arg("slot")
        .args(args)
        .current_dir(dir.path())
        .output()
        .expect("cqs slot failed to spawn");
    assert!(
        output.status.success(),
        "cqs slot {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("slot JSON parse failed: {e}\nstdout={stdout}"))
}

/// TC-HAP-V1.33-3: `cqs slot list --json` against a fresh `.cqs/` returns
/// a parseable envelope. Empty before any slot creation.
#[test]
#[serial]
fn slot_list_after_init_shows_no_slots() {
    let dir = setup_project();
    let envelope = run_slot_json(&dir, &["list"]);
    assert!(envelope["data"].is_object(), "data must be object");
    let slots = envelope["data"]["slots"]
        .as_array()
        .expect("slots must be array");
    assert!(slots.is_empty(), "fresh .cqs/ has no slots; got: {slots:?}");
}

/// TC-HAP-V1.33-3: `slot create` adds a slot dir and `slot list` reflects it.
#[test]
#[serial]
fn slot_create_then_list_includes_new_slot() {
    let dir = setup_project();
    run_slot_json(&dir, &["create", "test_slot_a"]);

    let envelope = run_slot_json(&dir, &["list"]);
    let slots = envelope["data"]["slots"].as_array().unwrap();
    let names: Vec<&str> = slots.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"test_slot_a"),
        "list must include created slot, got: {names:?}"
    );
    // Slot dir on disk
    let slot_dir = dir.path().join(".cqs/slots/test_slot_a");
    assert!(slot_dir.exists(), "slot dir must exist on disk");
}

/// TC-HAP-V1.33-3: `slot promote` swaps the active pointer. Pins the
/// atomic-pointer-update contract.
#[test]
#[serial]
fn slot_promote_swaps_active_pointer() {
    let dir = setup_project();
    run_slot_json(&dir, &["create", "alpha"]);
    run_slot_json(&dir, &["create", "beta"]);

    // Promote beta and verify `slot active` reflects it.
    run_slot_json(&dir, &["promote", "beta"]);
    let active = run_slot_json(&dir, &["active"]);
    let active_name = active["data"]
        .as_str()
        .or_else(|| active["data"]["active"].as_str())
        .expect("active envelope must carry name");
    assert_eq!(
        active_name, "beta",
        "after promote beta, active must be beta. envelope: {active}"
    );
}

/// TC-HAP-V1.33-3: P2.13 — `--slot` global flag is rejected on `slot`
/// subcommands (positional name overrides the global). Pins the bail.
#[test]
#[serial]
fn slot_subcommand_rejects_global_slot_flag() {
    let dir = setup_project();
    let result = cqs_no_daemon()
        .args(["--slot", "ignored", "slot", "list"])
        .current_dir(dir.path())
        .output()
        .expect("spawn cqs");

    assert!(
        !result.status.success(),
        "global --slot on `cqs slot` must fail (P2.13)"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("--slot has no effect") || stderr.contains("project-scoped"),
        "P2.13 bail message must explain why; got: {stderr}"
    );
}
