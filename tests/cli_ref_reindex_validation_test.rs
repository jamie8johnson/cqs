//! TC-HAP-V1.38-2 (#1463): integration test for `cqs ref reindex`'s
//! LLM/HyDE flag-dependency validation, exercised end-to-end via the
//! binary.
//!
//! `cqs ref reindex` (alias of `ref update`) accepts `--llm-summaries`,
//! `--improve-docs`, `--improve-all`, `--hyde-queries`, etc., and
//! enforces flag dependencies up front:
//!   - `--improve-docs` requires `--llm-summaries`
//!   - `--improve-all` requires `--improve-docs` (and transitively
//!     requires `--llm-summaries`)
//!
//! Pre-fix, the test surface for `cmd_ref_update` was parse-only —
//! `cli_ref_test.rs::test_ref_update_reindexes_source` exercises the
//! happy path without these flags, and the parse-only test in
//! `cli/mod.rs` confirms the flags bind to the right struct fields but
//! NEVER runs the LLM/HyDE pass. A regression that:
//!   - dropped the `--improve-docs requires --llm-summaries` bail
//!   - reordered the bail to AFTER the (slow) index pipeline
//!   - swapped the bail messages between the two checks
//! …would leave the operator with either silent misconfiguration (the
//! LLM pass quietly skips because llm_summaries=false) OR a 30-min
//! reindex that fails at the end.
//!
//! These tests run the binary against an empty tempdir (no project,
//! no ref configured) and assert the bail fires BEFORE any project
//! lookup. The validation lives at `cli/commands/infra/reference.rs`
//! lines 619-630 and is the unit gate keeping the LLM/HyDE feature
//! surface coherent.

#![cfg(feature = "llm-summaries")]

use assert_cmd::Command;
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

/// `cqs ref reindex foo --improve-docs` (no `--llm-summaries`) MUST
/// bail with the dependency message before any project-root walk or
/// config load runs. Pin the bail-text so a future rephrasing is a
/// deliberate change agents can spot.
#[test]
fn ref_reindex_improve_docs_requires_llm_summaries() {
    let dir = TempDir::new().expect("tempdir");

    let result = cqs_no_daemon()
        .args(["ref", "reindex", "nonexistent-ref", "--improve-docs"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs ref reindex");

    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        !result.status.success(),
        "ref reindex with --improve-docs but no --llm-summaries must fail. \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("--improve-docs requires --llm-summaries"),
        "stderr must surface the dependency hint verbatim so an operator \
         can tell parse-time vs config-time failures apart. stderr={stderr}"
    );
}

/// `cqs ref reindex foo --improve-all` (no `--improve-docs`) MUST bail
/// with the cascade dependency message. Pin so a future cascade
/// rewrite (e.g. collapsing the two checks into one) doesn't lose the
/// distinct error message.
#[test]
fn ref_reindex_improve_all_requires_improve_docs() {
    let dir = TempDir::new().expect("tempdir");

    let result = cqs_no_daemon()
        .args(["ref", "reindex", "nonexistent-ref", "--improve-all"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs ref reindex");

    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    assert!(
        !result.status.success(),
        "ref reindex with --improve-all but no --improve-docs must fail. \
         stderr={stderr}"
    );
    assert!(
        stderr.contains("--improve-all requires --improve-docs"),
        "stderr must surface the cascade dependency hint. stderr={stderr}"
    );
}

/// Sanity counterpart: `cqs ref reindex foo --llm-summaries` (only the
/// LLM flag, no improve-* flags) must NOT bail at validation time.
/// It will fail later because no ref is configured, but the failure
/// reason must be "ref not found", not the dependency hint. Pin the
/// pass branch of the validation so a regression that bailed on
/// `--llm-summaries` alone (e.g. by inverting the check) is caught.
#[test]
fn ref_reindex_llm_summaries_alone_passes_validation() {
    let dir = TempDir::new().expect("tempdir");

    let result = cqs_no_daemon()
        .args(["ref", "reindex", "nonexistent-ref", "--llm-summaries"])
        .current_dir(dir.path())
        .output()
        .expect("run cqs ref reindex");

    let stderr = String::from_utf8_lossy(&result.stderr).to_string();

    // Must fail (no ref configured), but NOT for the dependency reason.
    assert!(
        !result.status.success(),
        "ref reindex against a tempdir with no ref configured must fail \
         (ref-not-found). stderr={stderr}"
    );
    assert!(
        !stderr.contains("--improve-docs requires") && !stderr.contains("--improve-all requires"),
        "validation must NOT fire when only --llm-summaries is passed. \
         Got the dependency error instead of ref-not-found. stderr={stderr}"
    );
}
