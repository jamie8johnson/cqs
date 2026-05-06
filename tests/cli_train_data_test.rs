//! TC-HAP-V1.36-10 — `cqs train-data` CLI subprocess integration test.
//!
//! `cmd_train_data` (`src/cli/commands/train/train_data.rs`) is a thin
//! wrapper around `cqs::train_data::generate_training_data` plus a
//! `println!` summary. The library function has unit tests in
//! `src/train_data/mod.rs`; what was unverified is the CLI surface —
//! arg parsing, summary line formatting, and exit code on a real git
//! repo.
//!
//! `slow-tests`-gated and serial because the test spins a fresh git
//! repo + invokes the cqs binary as a subprocess.

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Spin a git repo with two commits modifying the same Rust function.
/// `train-data` walks `git log` and emits one triplet per
/// (commit, changed function) pair.
fn setup_git_repo_with_history() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("mkdir src");

    fs::write(
        src.join("lib.rs"),
        r#"
/// Apply discount to a price.
pub fn discount(price: u32) -> u32 {
    price - (price / 10)
}
"#,
    )
    .expect("write v1");

    let git_init = |args: &[&str]| {
        StdCommand::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .expect("git command failed");
    };
    git_init(&["init", "-q"]);
    git_init(&["config", "user.email", "test@example.com"]);
    git_init(&["config", "user.name", "Test"]);
    git_init(&["add", "src/lib.rs"]);
    git_init(&[
        "commit",
        "-q",
        "-m",
        "feat: implement basic discount calculation for cart pricing",
    ]);

    // Second commit: rewrite the function body, with a long enough message
    // to clear `--min-msg-len` (default 15).
    fs::write(
        src.join("lib.rs"),
        r#"
/// Apply tiered discount based on price brackets.
pub fn discount(price: u32) -> u32 {
    if price > 100 { price - (price / 5) } else { price - (price / 20) }
}
"#,
    )
    .expect("write v2");

    git_init(&["add", "src/lib.rs"]);
    git_init(&[
        "commit",
        "-q",
        "-m",
        "feat: introduce tiered discount brackets for higher-priced items",
    ]);

    dir
}

/// TC-HAP-V1.36-10 happy path: `cqs train-data` runs against a real git
/// repo, emits the canonical summary line, and writes a non-empty JSONL
/// at `--output`. We don't pin exact triplet content (BM25 negatives
/// depend on the corpus) — only that the structure is right.
#[test]
#[serial]
fn train_data_emits_summary_line_and_writes_jsonl() {
    let dir = setup_git_repo_with_history();
    let output = dir.path().join("triplets.jsonl");

    let output_str = output.to_string_lossy().to_string();
    cqs()
        .args([
            "train-data",
            "--repos",
            ".",
            "--output",
            &output_str,
            "--max-commits",
            "10",
            "--max-files",
            "5",
            "--dedup-cap",
            "5",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        // `cmd_train_data` prints exactly:
        //   "Generated N triplets from M repos (X commits processed, Y skipped)"
        // Pin the prefix so a refactor that mangles the summary surfaces here.
        .stdout(predicate::str::contains("Generated "))
        .stdout(predicate::str::contains(" triplets from "))
        .stdout(predicate::str::contains(" repos ("))
        .stdout(predicate::str::contains(" commits processed"));

    // The JSONL output must exist. Whether it's non-empty depends on
    // whether the BM25 corpus produced enough hard negatives — for a
    // single-function repo with no negatives, the function logs and skips
    // the triplet. So we only assert the file was created (write path
    // exercised) rather than line count.
    assert!(
        output.exists(),
        "expected output file at {} after train-data run",
        output.display()
    );
}

/// TC-HAP-V1.36-10: rejects a non-git directory with a warn (skipped repo)
/// and exits 0 — the CLI is best-effort across multiple `--repos`, so
/// "not a repo" must not abort the whole run.
#[test]
#[serial]
fn train_data_skips_non_git_repo_and_exits_zero() {
    let dir = TempDir::new().expect("tempdir");
    fs::write(dir.path().join("README.md"), "not a git repo").expect("write");
    let output = dir.path().join("empty.jsonl");

    let output_str = output.to_string_lossy().to_string();
    cqs()
        .args([
            "train-data",
            "--repos",
            ".",
            "--output",
            &output_str,
            "--max-commits",
            "5",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("0 triplets"));
}
