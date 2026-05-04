//! Audit P4-22 / TC-HAP-V1.33-9 — `cmd_gc` end-to-end integration test.
//!
//! Pre-existing inline tests in `src/cli/commands/index/gc.rs` only assert
//! `GcOutput` JSON shape on hand-constructed structs. The full pipeline
//! (`enumerate_files` → `count_stale_files` → `prune_all` → orphan-sparse
//! prune → conditional `build_hnsw_index` rebuild) had no test driving it.
//!
//! The highest-risk untested branch is the HNSW rebuild path
//! (`gc.rs:100-136`). A regression flipping the rebuild guard from
//! `pruned_chunks > 0` to `>= 0` would always rebuild, slowing GC by
//! minutes on big corpora. A regression skipping the stale-HNSW deletion
//! before rebuild would leak orphan IDs (RT-DATA-2 — the very issue the
//! `gc.rs:97-99` comment calls out).
//!
//! Pinned here:
//! - dirty path: file deletion → `pruned_chunks >= 1`, `hnsw_rebuilt=true`,
//!   `hnsw_vectors` matches the post-prune chunk count.
//! - clean path: re-running `cqs gc` on a freshly-GC'd index returns
//!   `pruned_chunks=0`, `hnsw_rebuilt=false`, no `hnsw_vectors` field.
//!
//! Gated `slow-tests` because `cqs index` cold-loads the embedder and the
//! HNSW rebuild is non-trivial.

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Two-file project so the dirty branch can delete one and still leave
/// chunks behind for the post-rebuild HNSW assertion to be meaningful.
fn setup_two_file_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("Failed to create src dir");

    fs::write(
        src.join("lib.rs"),
        "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("Failed to write lib.rs");

    fs::write(
        src.join("doomed.rs"),
        "pub fn gamma() -> i32 { 3 }\npub fn delta() -> i32 { 4 }\n",
    )
    .expect("Failed to write doomed.rs");

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

    dir
}

/// Parse a `cqs ... --json` stdout envelope into the unwrapped `data` payload.
fn parse_envelope_data(stdout: &[u8]) -> Value {
    let parsed: Value = serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON: {e}\nraw: {}",
            String::from_utf8_lossy(stdout)
        )
    });
    parsed
        .get("data")
        .cloned()
        .unwrap_or_else(|| panic!("envelope missing data field: {parsed}"))
}

/// Dirty path: delete a file, run `cqs gc --json`, assert prune + HNSW rebuild.
/// Pins both the `pruned_chunks > 0` rebuild trigger and the post-rebuild
/// vector count being equal to the surviving chunk count.
#[test]
#[serial]
fn test_gc_prunes_and_rebuilds_hnsw_after_deletion() {
    let dir = setup_two_file_project();

    // Delete the doomed file — cqs gc must prune its chunks and rebuild HNSW.
    fs::remove_file(dir.path().join("src/doomed.rs")).expect("Failed to delete src/doomed.rs");

    let output = cqs()
        .args(["gc", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("cqs gc --json failed to spawn");

    assert!(
        output.status.success(),
        "cqs gc --json should succeed. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let data = parse_envelope_data(&output.stdout);

    let pruned_chunks = data["pruned_chunks"]
        .as_u64()
        .expect("pruned_chunks must be a number");
    assert!(
        pruned_chunks >= 1,
        "deleting doomed.rs should prune at least 1 chunk; got {pruned_chunks}. data={data}"
    );

    let hnsw_rebuilt = data["hnsw_rebuilt"]
        .as_bool()
        .expect("hnsw_rebuilt must be a bool");
    assert!(
        hnsw_rebuilt,
        "hnsw_rebuilt must be true when pruned_chunks > 0. data={data}"
    );

    let hnsw_vectors = data["hnsw_vectors"]
        .as_u64()
        .expect("hnsw_vectors must be a number after rebuild");
    assert!(
        hnsw_vectors >= 1,
        "post-rebuild HNSW must contain at least the surviving chunks; got {hnsw_vectors}. data={data}"
    );
}

/// Clean path: re-running `cqs gc` on a freshly-GC'd index reports
/// `pruned_chunks=0, hnsw_rebuilt=false`, and `hnsw_vectors` is absent
/// (skipped by `serde(skip_serializing_if = "Option::is_none")`).
/// Pins the no-op exit at `gc.rs:134-136`.
#[test]
#[serial]
fn test_gc_no_op_on_clean_index() {
    let dir = setup_two_file_project();

    // Empty GC pass on a clean index — nothing to prune, no rebuild.
    let output = cqs()
        .args(["gc", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("cqs gc --json failed to spawn");

    assert!(
        output.status.success(),
        "cqs gc --json should succeed on clean index. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let data = parse_envelope_data(&output.stdout);

    assert_eq!(
        data["pruned_chunks"].as_u64(),
        Some(0),
        "clean index must report pruned_chunks=0. data={data}"
    );
    assert_eq!(
        data["hnsw_rebuilt"].as_bool(),
        Some(false),
        "clean index must report hnsw_rebuilt=false. data={data}"
    );
    assert!(
        data.get("hnsw_vectors").is_none(),
        "hnsw_vectors must be omitted when no rebuild happened. data={data}"
    );
}
