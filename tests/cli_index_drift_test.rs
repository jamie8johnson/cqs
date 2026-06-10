//! TC-HAP-V1.38-4 (#1463): integration test for `cqs index --model X`
//! drift detection (#1505) exercised end-to-end via the binary.
//!
//! The drift-detection helper `check_index_model_drift` has thorough
//! unit tests at `src/cli/commands/index/build.rs` (covering name
//! match, repo match, mismatch bail, case-sensitivity pin, whitespace
//! drift) — but none of them invoke the binary. A regression that
//! reordered the call sequence inside `cmd_index` (e.g., dropped the
//! `check_index_model_drift(...)?` call entirely, or moved it to AFTER
//! the embedder load) would not break any unit test, yet would let the
//! footgun back through: `cqs index --model embeddinggemma-300m`
//! against a 1024-dim bge-large index would silently feed 768-dim
//! embeddings into the wrong-shape store.
//!
//! This test builds a `.cqs/index.db` initialized with stored
//! `model_name = "BAAI/bge-large-en-v1.5"`, then runs
//! `cqs index --model embeddinggemma-300m` against it. The binary must
//! exit non-zero with stderr that names BOTH model identifiers so the
//! operator gets the actionable error.

use assert_cmd::Command;
use cqs::store::ModelInfo;
use cqs::Store;
use tempfile::TempDir;

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

fn cqs_no_daemon() -> Command {
    let mut c = cqs();
    c.env("CQS_NO_DAEMON", "1");
    c
}

/// Build a `.cqs/index.db` whose stored `model_name` is the bge-large
/// HF repo path. Triggers drift bail when subsequently run with
/// `--model embeddinggemma-300m`.
fn seed_bge_large_store() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let db_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    let store = Store::open(&db_path).expect("open store");
    // Stored model is bge-large (1024-dim). The test uses the full HF
    // repo string because that's what `ModelInfo::new` writes — and the
    // drift check accepts either short name or repo as a match (see
    // `check_index_model_drift_repo_match_passes` unit test).
    store
        .init(&ModelInfo::new("BAAI/bge-large-en-v1.5", 1024))
        .expect("init store with bge-large model_name");
    drop(store);
    dir
}

/// `cqs index --model embeddinggemma-300m` against a bge-large-stamped
/// index MUST bail before any embedding work. Pin both the exit code
/// and the recovery hint shape so an operator getting this error has
/// the actionable details instead of a downstream dim-mismatch panic.
#[test]
fn index_drift_detection_bails_on_model_mismatch() {
    let dir = seed_bge_large_store();

    let result = cqs_no_daemon()
        .args(["--model", "embeddinggemma-300m", "index"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs index --model embeddinggemma-300m");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        !result.status.success(),
        "drift mismatch must fail the index command. stdout={stdout} stderr={stderr}"
    );

    // Recovery-hint contract: stderr must surface both the stored and
    // requested model identifiers so the operator can decide between
    // `--force` (rebuild) and switching `--model` back. Without this,
    // the error reads as a generic store failure.
    assert!(
        stderr.contains("bge-large") || stderr.contains("BAAI/bge-large-en-v1.5"),
        "stderr must name the stored model so operator knows what's already on disk. \
         stderr={stderr}"
    );
    assert!(
        stderr.contains("embeddinggemma-300m") || stderr.contains("google/embeddinggemma"),
        "stderr must name the requested --model so operator knows what they typed. \
         stderr={stderr}"
    );
}

/// Sanity counterpart: `cqs index` against a matching-model store
/// must NOT bail with a drift error (it may still fail later for
/// unrelated reasons like missing embedder model files in the test
/// environment, but stderr must not contain the drift-specific hint).
/// This pins the no-op branch of `check_index_model_drift` so a
/// regression that flipped the bail condition (`==` → `!=`) is caught.
#[test]
fn index_drift_detection_passes_on_model_match() {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let db_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    let store = Store::open(&db_path).expect("open store");
    // Init with embeddinggemma so subsequent `cqs index --model
    // embeddinggemma-300m` matches.
    store
        .init(&ModelInfo::new("google/embeddinggemma-300m", 768))
        .expect("init store with embeddinggemma model_name");
    drop(store);

    let result = cqs_no_daemon()
        .args(["--model", "embeddinggemma-300m", "index", "--dry-run"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs index --dry-run");

    let stderr = String::from_utf8_lossy(&result.stderr).to_string();
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();

    // The exit code may be 0 (dry-run path took over) or non-zero for
    // unrelated reasons in the test sandbox. The contract this test
    // pins is: stderr must NOT contain the drift-specific recovery hint.
    // The actual hint string lives in `check_index_model_drift`'s
    // bail; "drift" is the load-bearing keyword.
    assert!(
        !stderr.contains("model drift") && !stderr.contains("drift detected"),
        "matching model must not trigger drift bail. stderr={stderr} stdout={stdout}"
    );
}
