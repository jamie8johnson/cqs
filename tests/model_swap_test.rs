//! Integration tests for `cqs model { show, list, swap }`.
//!
//! These tests stand up a real `.cqs/index.db` inside a `TempDir`, then
//! invoke the binary via `assert_cmd` with `CQS_NO_DAEMON=1` to keep daemon
//! state from leaking between tests.
//!
//! The full backup-reindex-restore round-trip is hard to test without an
//! ONNX model on disk (the reindex pass calls `Embedder::new` which
//! downloads the model). Those flows are covered by the manual smoke test
//! in the implementation report; here we pin the parts that are reachable
//! without a real embedder:
//!
//! 1. `cqs model show --json` reads the recorded model from a seeded store.
//! 2. `cqs model list --json` enumerates every preset and marks the current
//!    index's model with `current: true`.
//! 3. `cqs model swap <current_model>` is a no-op (does not delete `.cqs/`).
//! 4. `cqs model swap garbage` errors with a hint about valid presets.

use assert_cmd::Command;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

/// Get a Command for the cqs binary.
fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Force CLI mode — different dev machines may have leftover daemon sockets
/// in `$XDG_RUNTIME_DIR` that would otherwise serve our test queries.
fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// Seed `<dir>/.cqs/index.db` with a Store initialized to the given model
/// (name + dim). No chunks — `cmd_model_show` only needs the metadata row.
fn seed_store(dir: &TempDir, model_repo: &str, dim: usize) {
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir_all(&cqs_dir).expect("create .cqs dir");
    let store_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let store = cqs::Store::open(&store_path).expect("open seed store");
    store
        .init(&cqs::store::ModelInfo::new(model_repo, dim))
        .expect("init seed store");
    // Write something to chunks so chunk_count is non-zero and we don't
    // get a misleading 0 in the show output. Re-using the eval helper
    // would pull a much bigger fixture; here we just want the DB to exist
    // and report its model.
    drop(store); // close before tests reopen via `cqs model show`
}

/// Test 1 — `cqs model show --json` against a seeded store reflects the
/// recorded model and dim. Pinned because this is the simplest end-to-end
/// surface and confirms the readonly path works against the actual schema.
#[test]
#[serial]
fn test_model_show_against_seeded_store() {
    let dir = TempDir::new().expect("tempdir");
    seed_store(&dir, "BAAI/bge-large-en-v1.5", 1024);

    let result = cqs_no_daemon()
        .args(["model", "show", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs model show");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        result.status.success(),
        "model show should succeed against seeded store. stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("model show --json output must be JSON. got: {stdout}"));
    assert_eq!(
        parsed["data"]["model"].as_str(),
        Some("BAAI/bge-large-en-v1.5"),
        "data.model field must reflect seeded model. got: {parsed:?}"
    );
    assert_eq!(parsed["data"]["dim"].as_u64(), Some(1024));
    // Field exists even if 0 — required by JSON contract.
    assert!(parsed["data"].get("total_chunks").is_some());
    assert!(parsed["data"].get("index_db_size_bytes").is_some());
    assert!(parsed["data"].get("hnsw_size_bytes").is_some());
}

/// Test 2 — `cqs model list --json` lists every preset and flips the
/// `current` flag on the one matching the seeded store's model. Pinning
/// the array shape prevents accidental schema drift when a new preset is
/// added without updating the JSON output struct.
#[test]
#[serial]
fn test_model_list_includes_current() {
    let dir = TempDir::new().expect("tempdir");
    seed_store(&dir, "BAAI/bge-large-en-v1.5", 1024);

    let result = cqs_no_daemon()
        .args(["model", "list", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs model list");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        result.status.success(),
        "model list should succeed. stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("model list --json output must be JSON. got: {stdout}"));
    let arr = parsed["data"]
        .as_array()
        .expect("model list --json data must be a JSON array");
    assert!(
        arr.len() >= 3,
        "expected at least 3 presets (bge-large, e5-base, v9-200k); got {arr:?}"
    );

    // Current = bge-large (matches the seeded repo).
    let current_count = arr
        .iter()
        .filter(|e| e["current"].as_bool() == Some(true))
        .count();
    assert_eq!(
        current_count, 1,
        "exactly one preset must be marked current; got {arr:?}"
    );
    let bge = arr
        .iter()
        .find(|e| e["name"].as_str() == Some("bge-large"))
        .expect("bge-large preset missing from list");
    assert_eq!(bge["current"].as_bool(), Some(true));
    assert_eq!(bge["dim"].as_u64(), Some(1024));
    assert_eq!(bge["repo"].as_str(), Some("BAAI/bge-large-en-v1.5"));
}

/// Test 3 — `cqs model swap` to the *current* model is a no-op: the index
/// directory's modification time must not change, and the command exits 0
/// without invoking the reindex pipeline. This is the cheapest way to
/// confirm the early-exit short-circuit fires before any destructive ops.
#[test]
#[serial]
fn test_model_swap_same_preset_is_noop() {
    let dir = TempDir::new().expect("tempdir");
    seed_store(&dir, "BAAI/bge-large-en-v1.5", 1024);

    let cqs_dir = dir.path().join(".cqs");
    let index_db = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    // Use the index.db file itself as the "did anything destructive
    // happen?" signal. Opening the store touches WAL/SHM siblings which
    // bumps the directory mtime — those don't indicate a swap actually
    // ran. The DB file mtime only changes on a real reindex pass.
    let db_mtime_before = fs::metadata(&index_db)
        .expect("stat index.db")
        .modified()
        .ok();
    let db_size_before = fs::metadata(&index_db).expect("stat index.db").len();

    let result = cqs_no_daemon()
        .args(["model", "swap", "bge-large", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs model swap bge-large (current)");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        result.status.success(),
        "swap to current model must succeed (no-op). stderr={stderr} stdout={stdout}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
        panic!("swap (no-op) --json output must be JSON. got: {stdout} stderr={stderr}")
    });
    assert_eq!(
        parsed["data"]["noop"].as_bool(),
        Some(true),
        "swap to current model must report data.noop=true. got: {parsed:?}"
    );

    // index.db must be the same file (size + mtime unchanged) — no
    // backup-and-recreate happened.
    assert!(
        index_db.exists(),
        ".cqs/index.db must still exist after no-op swap"
    );
    let db_mtime_after = fs::metadata(&index_db)
        .expect("stat index.db after")
        .modified()
        .ok();
    let db_size_after = fs::metadata(&index_db).expect("stat index.db after").len();
    assert_eq!(
        db_mtime_before, db_mtime_after,
        "index.db mtime must not change on no-op swap. before={db_mtime_before:?} after={db_mtime_after:?}"
    );
    assert_eq!(
        db_size_before, db_size_after,
        "index.db size must not change on no-op swap"
    );
    // No backup directory should exist either.
    let backup = dir.path().join(".cqs.BAAI-bge-large-en-v1.5.bak");
    assert!(
        !backup.exists(),
        "no backup should be created on a no-op swap. backup={backup:?}"
    );
}

/// Test 4 — `cqs model swap garbage-preset` errors and the message names
/// the valid presets so the user knows what to type. Belt-and-braces: also
/// verify the original `.cqs/` is untouched (the validation must fire
/// before any backup or reindex begins).
#[test]
#[serial]
fn test_model_swap_unknown_preset_errors() {
    let dir = TempDir::new().expect("tempdir");
    seed_store(&dir, "BAAI/bge-large-en-v1.5", 1024);

    let cqs_dir = dir.path().join(".cqs");
    let index_db_before = fs::metadata(cqs_dir.join(cqs::INDEX_DB_FILENAME))
        .expect("stat index.db")
        .len();

    let result = cqs_no_daemon()
        .args(["model", "swap", "garbage-preset-name"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs model swap garbage");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        !result.status.success(),
        "swap to unknown preset must fail. stdout={stdout} stderr={stderr}"
    );

    // Error must mention the preset name AND list the valid presets so
    // the user can recover without `--help`. These three names are stable
    // commitments — adding a new preset is fine, removing one would break
    // this assertion intentionally.
    let err_text = stderr.to_lowercase();
    assert!(
        err_text.contains("unknown preset") || err_text.contains("garbage-preset-name"),
        "error must mention the rejected preset. stderr={stderr}"
    );
    assert!(
        err_text.contains("bge-large"),
        "error must list bge-large as a valid preset. stderr={stderr}"
    );
    assert!(
        err_text.contains("e5-base"),
        "error must list e5-base as a valid preset. stderr={stderr}"
    );
    assert!(
        err_text.contains("v9-200k"),
        "error must list v9-200k as a valid preset. stderr={stderr}"
    );

    // Original index untouched — pre-flight validation runs before any
    // backup or rename.
    let index_db_after = fs::metadata(cqs_dir.join(cqs::INDEX_DB_FILENAME))
        .expect("stat index.db after failed swap")
        .len();
    assert_eq!(
        index_db_before, index_db_after,
        "index.db must not change when swap fails on validation"
    );
    let backup = dir.path().join(".cqs.BAAI-bge-large-en-v1.5.bak");
    assert!(
        !backup.exists(),
        "no backup should be created when validation fails. backup={backup:?}"
    );
}

/// Test 5 — `cqs model show` against a directory with no `.cqs/` index
/// must error cleanly (not panic) and tell the user how to bootstrap.
/// Pinned as a smoke test for the preflight check.
#[test]
#[serial]
fn test_model_show_no_index_errors_cleanly() {
    let dir = TempDir::new().expect("tempdir");

    let result = cqs_no_daemon()
        .args(["model", "show"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs model show with no index");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        !result.status.success(),
        "model show with no index must fail. stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("no index") || stderr.to_lowercase().contains("cqs init"),
        "error must hint at `cqs init`. stderr={stderr}"
    );
}
