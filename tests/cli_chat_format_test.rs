//! Audit P2 #47 (a) — `cmd_chat` JSON formatting integration test.
//!
//! `cmd_chat` is the interactive REPL. The format-and-print body was extracted
//! into `crate::cli::json_envelope::format_envelope_to_string` (P1 D.1, Agent B)
//! so the chat surface inherits the same NaN/Infinity sanitization that
//! `write_json_line` (batch / daemon) and `emit_json` (CLI) perform.
//!
//! `format_envelope_to_string` itself is `pub` but lives behind a `pub(crate)`
//! `json_envelope` module — it can't be unit-tested from `tests/`. The
//! authoritative NaN tests are inline in `src/cli/json_envelope.rs:247-306`
//! (`emit_json_sanitizes_nan_to_null`, `emit_json_sanitizes_pos_and_neg_infinity`,
//! `format_envelope_to_string_handles_nan_payload`,
//! `format_envelope_to_string_passthrough_on_clean_value`).
//!
//! This file pins the **observable** chat behavior end-to-end: spawn
//! `cqs chat`, pipe a search command, EOF. Verify the REPL boots, processes
//! a query, and emits a JSON envelope that an agent can parse from stdout.
//! Subprocess pattern + slow-tests gate (chat warms the embedder on launch).

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

/// Set up a tiny indexed project so the chat REPL has a store to query.
fn setup_chat_project() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("create src");

    fs::write(
        src.join("lib.rs"),
        "/// Adds two numbers.\npub fn add_numbers(a: i32, b: i32) -> i32 { a + b }\n",
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

/// Pipe `search add_numbers\nexit\n` into `cqs chat`. Verify the REPL starts,
/// processes the search, and emits a parseable JSON envelope to stdout.
///
/// This is the smoke test for the `format_envelope_to_string` integration —
/// any bug that returned `Err` from the formatter (e.g. NaN in a real result)
/// would show up here as an "Error formatting output" stderr message and
/// missing stdout envelope.
#[test]
#[serial]
fn test_chat_emits_parseable_envelope_for_search_query() {
    let dir = setup_chat_project();

    // rustyline's `readline` reads from stdin until EOF if not a tty. The
    // `add_history_entry` writes to .cqs/chat_history (we let it).
    let output = cqs()
        .args(["chat"])
        .current_dir(dir.path())
        .write_stdin("search add_numbers\nexit\n")
        .output()
        .expect("Failed to run cqs chat");

    // Chat exits 0 on clean exit/quit/EOF.
    assert!(
        output.status.success(),
        "chat should exit 0 on clean shutdown. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The REPL prints the welcome banner first, then for each query a JSON
    // envelope (pretty-printed). Find the first `{` and parse from there.
    // Multi-line pretty JSON makes `lines().next()` insufficient.
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON envelope found in chat stdout: {stdout}"));
    let json_text = &stdout[json_start..];
    let parsed: serde_json::Value = serde_json::from_str(json_text.trim()).unwrap_or_else(|e| {
        // The envelope might be followed by another welcome banner or prompt
        // on EOF; try to read just up to the closing brace at depth 0.
        let mut depth = 0i32;
        let mut end = 0usize;
        for (i, ch) in json_text.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let trimmed = if end > 0 {
            &json_text[..end]
        } else {
            json_text
        };
        serde_json::from_str(trimmed).unwrap_or_else(|e2| {
            panic!("envelope JSON parse failed: first attempt {e}, second {e2}\nstdout={stdout}")
        })
    });

    // Envelope shape from format_envelope_to_string
    assert_eq!(parsed["version"], 1);
    assert!(parsed["error"].is_null(), "no error on success path");
    assert!(
        parsed["data"].is_array() || parsed["data"].is_object(),
        "data must be the search result array/object, got: {}",
        parsed["data"]
    );
}

/// Empty input lines and meta-commands (`help`, `clear`) should NOT produce
/// envelope output. Pins that meta-command handling short-circuits before
/// the format-and-print path.
#[test]
#[serial]
fn test_chat_meta_commands_do_not_emit_envelope() {
    let dir = setup_chat_project();

    // help → meta-command, no envelope. exit → exit cleanly.
    let output = cqs()
        .args(["chat"])
        .current_dir(dir.path())
        .write_stdin("help\n# a comment\n\nexit\n")
        .output()
        .expect("Failed to run cqs chat");

    assert!(
        output.status.success(),
        "chat should exit 0 cleanly. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // `help` prints "Available commands: ..." (no JSON braces). The welcome
    // banner doesn't contain `{` either. No envelope should be emitted.
    assert!(
        !stdout.contains('{') || !stdout.contains("\"version\""),
        "help/comment/blank should not trigger envelope emission. stdout={stdout}"
    );
    assert!(
        stdout.contains("Available commands"),
        "help meta-command should print command list. stdout={stdout}"
    );
}
