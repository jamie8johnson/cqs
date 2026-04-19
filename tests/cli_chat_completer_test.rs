//! Audit P2 #47 (b) — `cmd_chat` tab completer integration test.
//!
//! `ChatHelper::complete` (in `src/cli/chat.rs`) drives rustyline's tab
//! completion against the batch subcommand registry derived from
//! `BatchInput::command()`. Both `ChatHelper` and `BatchInput` are
//! `pub(crate)`; the authoritative completer unit tests live inline at
//! `src/cli/chat.rs:308-365` (`test_complete_empty_prefix`,
//! `test_complete_partial_prefix`, `test_complete_after_space_returns_empty`,
//! `test_complete_no_match`).
//!
//! What we CAN verify externally is that `cqs chat`'s help meta-command
//! emits the same command list the completer derives from. A regression in
//! `command_names()` (e.g. `BatchInput::command()` returning empty
//! subcommands) would surface here as missing keywords in the help output.
//!
//! Subprocess + slow-tests (chat warms the embedder).

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

fn setup_chat_project() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("create src");

    fs::write(
        src.join("lib.rs"),
        "/// dummy\npub fn dummy_fn() -> i32 { 1 }\n",
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
        .success()
        .stdout(predicate::str::contains("Index complete"));

    dir
}

/// `help` in chat prints "Available commands: <comma-separated list>". This
/// list is built from the same `BatchInput::command().get_subcommands()`
/// iterator that feeds the rustyline completer (see `command_names()` in
/// `src/cli/chat.rs:100-113`). If the registry is empty (regression that
/// broke clap derivation), no commands appear here and the completer
/// silently degrades.
///
/// We assert presence of a representative subset — `search`, `scout`,
/// `callers`, `impact` — that an agent would expect to tab-complete.
#[test]
#[serial]
fn test_chat_help_lists_completer_commands() {
    let dir = setup_chat_project();

    let output = cqs()
        .args(["chat"])
        .current_dir(dir.path())
        .write_stdin("help\nexit\n")
        .output()
        .expect("Failed to run cqs chat");

    assert!(
        output.status.success(),
        "chat should exit 0. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // `help` prints: "Available commands: blame, callees, callers, ..."
    let cmd_line = stdout
        .lines()
        .find(|l| l.starts_with("Available commands:"))
        .unwrap_or_else(|| {
            panic!("'Available commands:' line missing from help output. stdout={stdout}")
        });

    // Subset of commands the completer must expose to tab-complete. If
    // `BatchInput::command()` regressed to an empty list, `command_names()`
    // would return only the meta-commands and these would all be missing.
    for cmd in ["search", "scout", "callers", "impact", "explain"] {
        assert!(
            cmd_line.contains(cmd),
            "completer command list missing '{cmd}'. line: {cmd_line}"
        );
    }

    // Pipeline syntax line is also part of the help output. Pin it so a
    // regression that dropped the pipeline reminder is caught.
    assert!(
        stdout.contains("Pipeline:"),
        "help should mention pipeline syntax. stdout={stdout}"
    );
    // Meta-commands hint
    assert!(
        stdout.contains("Meta:"),
        "help should mention meta-commands. stdout={stdout}"
    );
}

/// The completer derives its source-of-truth list from clap's subcommand
/// registry — every subcommand defined in `BatchInput` should appear in
/// `cqs --help` output too. Pins the broader contract that the chat
/// completer's command set is a superset of the batch subcommand surface.
///
/// Indirect coverage: if a future PR adds a new `BatchCmd` variant but the
/// helper list goes stale (which shouldn't happen given the derivation
/// from `BatchInput::command()`), this test won't catch it directly — but
/// the chat help test above will, since both build from the same iterator.
#[test]
#[serial]
fn test_chat_help_subset_matches_cqs_help_subset() {
    let dir = setup_chat_project();

    let chat_output = cqs()
        .args(["chat"])
        .current_dir(dir.path())
        .write_stdin("help\nexit\n")
        .output()
        .expect("cqs chat failed");
    assert!(chat_output.status.success());

    let chat_stdout = String::from_utf8_lossy(&chat_output.stdout);
    let chat_help_line = chat_stdout
        .lines()
        .find(|l| l.starts_with("Available commands:"))
        .expect("chat help line present");

    // `cqs --help` lists subcommands too. Pull the global help and confirm
    // that `search` and `scout` (also in the chat list) are available top-
    // level — the chat completer's commands map 1:1 to top-level CLI
    // subcommands.
    let cli_help = cqs().args(["--help"]).output().expect("cqs --help failed");
    let cli_stdout = String::from_utf8_lossy(&cli_help.stdout);

    for cmd in ["search", "scout"] {
        assert!(
            chat_help_line.contains(cmd),
            "chat help missing '{cmd}': {chat_help_line}"
        );
        assert!(
            cli_stdout.contains(cmd),
            "cqs --help missing '{cmd}': {cli_stdout}"
        );
    }
}
