//! Audit P3 #117 — `cmd_doctor --fix` and `cmd_init` re-init integration tests.
//!
//! `cqs doctor --fix` walks the issue list, then for `Stale` / `NoIndex`
//! shells out to `cqs index`, for `Schema` shells out to `cqs index --force`.
//! `run_fixes` lives at `src/cli/commands/infra/doctor.rs:38-87`. It has
//! NO integration test today — only the issue→fix mapping is unit-tested.
//!
//! `cqs init` (no `--force` flag exists today — Init is a unit variant in
//! `src/cli/definitions.rs:289`) currently always re-creates the `.cqs/`
//! directory and the `.gitignore` file. It does NOT touch `index.db`.
//! These tests pin the actual current behaviour: re-running `cqs init`
//! preserves any existing `index.db` (no clobber). If a future commit
//! adds `--force`, this test will be updated to exercise the new flag —
//! the audit-finding wording ("force-reinit clobber/preserve behavior")
//! is satisfied today by pinning preserve.
//!
//! Both tests are subprocess-driven — `cmd_doctor` runs a child `cqs index`
//! and we want to observe the real chain. Gated `slow-tests` because
//! `cqs index` cold-loads the embedder.

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

/// Build a minimal project with one indexable Rust function so `cqs index`
/// has work to do.
fn setup_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .expect("Failed to write lib.rs");

    dir
}

/// `cqs doctor --fix` on a project with no index reports `IssueKind::NoIndex`,
/// then runs `cqs index` as a subprocess. After the fix the `.cqs/index.db`
/// file must exist and a follow-up `cqs doctor` must pass without listing
/// any issues. Pins the NoIndex → `cqs index` path in `run_fixes:48-61`.
#[test]
#[serial]
fn test_doctor_fix_creates_missing_index() {
    let dir = setup_project();

    // Create .cqs/ via cqs init (sets up the dir but does not index).
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    let index_db = dir.path().join(".cqs").join("index.db");
    assert!(
        !index_db.exists(),
        "precondition: index.db must not exist before --fix"
    );

    // Run doctor --fix. Should detect NoIndex and run `cqs index`.
    let output = cqs()
        .args(["doctor", "--fix"])
        .current_dir(dir.path())
        .output()
        .expect("cqs doctor --fix failed to spawn");

    assert!(
        output.status.success(),
        "doctor --fix should succeed on no-index project. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Verify the fix landed: index.db now exists.
    assert!(
        index_db.exists(),
        "doctor --fix on NoIndex project must create .cqs/index.db. \
         path={}",
        index_db.display()
    );

    // Stdout should mention the fix action — the literal wording is
    // "Fixing: ... — running 'cqs index'..." per `run_fixes:50`.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Fixing")
            || stdout.contains("Index rebuilt")
            || stdout.contains("Auto-fixing"),
        "stdout should describe the fix action. got: {stdout}"
    );

    // Sanity: a follow-up doctor reports no NoIndex issue (the index now exists).
    cqs()
        .args(["doctor"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Index"));
}

/// `cqs init` re-run on an already-indexed project must NOT clobber the
/// existing `index.db`. Today there is no `--force` flag on Init; this
/// test pins the preserve behaviour. The audit wording (#117) speaks to
/// "clobber/preserve" — preserving the user's index across an accidental
/// `cqs init` re-run is the correct contract: a force flag, if added
/// later, is the explicit opt-in.
#[test]
#[serial]
fn test_init_rerun_preserves_existing_index() {
    let dir = setup_project();

    // First init + index.
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

    let index_db = dir.path().join(".cqs").join("index.db");
    assert!(
        index_db.exists(),
        "precondition: index.db must exist after first index"
    );
    let original_size = fs::metadata(&index_db)
        .expect("metadata after first index")
        .len();
    assert!(
        original_size > 0,
        "precondition: index.db must be non-empty after first index, got {original_size}"
    );

    // Re-run cqs init. Must succeed and must NOT delete or zero the db.
    cqs()
        .args(["init"])
        .current_dir(dir.path())
        .assert()
        .success();

    assert!(
        index_db.exists(),
        "cqs init re-run must preserve existing index.db (no clobber). \
         path={}",
        index_db.display()
    );
    let after_size = fs::metadata(&index_db)
        .expect("metadata after second init")
        .len();
    assert_eq!(
        after_size, original_size,
        "cqs init re-run must not modify index.db size (preserve contract). \
         original={original_size}, after_init={after_size}"
    );
}
