//! TC-HP-2: CLI integration tests for `cqs notes add|update|remove` lifecycle.
//!
//! `cmd_notes_mutate` (v1.25.0 post-PR #945) had zero integration tests. Inline
//! tests in `src/cli/commands/io/notes.rs` only verified `NoteMutationOutput`
//! JSON serialization — nothing exercised `cmd_notes_add`, `cmd_notes_update`,
//! or `cmd_notes_remove` end-to-end. A broken text-trim, sentiment-clamp,
//! `ensure_notes_file` mkdir, or reindex path would ship silently.
//!
//! These tests drive the mutation handlers via the CLI binary (the real call
//! path a user hits) and inspect the resulting `docs/notes.toml` state using
//! `cqs::parse_notes` — we deliberately avoid `cqs notes list` here because
//! its `CommandContext` requires a populated index (which in turn needs the
//! embedding model to be downloaded). The on-disk round-trip is a truer test
//! of the mutation handlers anyway: it catches any TOML rewrite regression
//! before the reindex layer.
//!
//! All tests use `--no-reindex` so the handlers don't try to open a store.
//!
//! `#[serial]` is required because the notes file locking is per-process and
//! the shared assert_cmd binary cache can otherwise produce flaky CI.

use assert_cmd::Command;
use cqs::note::Note;
use predicates::prelude::*;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Spin up an empty project with a `.cqs` directory so `find_project_root`
/// resolves to the temp dir. Notes commands auto-create `docs/notes.toml`
/// via `ensure_notes_file`, so we don't pre-create it — that covers the path.
fn setup_notes_project() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let cqs_dir = dir.path().join(".cqs");
    fs::create_dir_all(&cqs_dir).expect("Failed to create .cqs dir");
    dir
}

fn notes_path(dir: &TempDir) -> PathBuf {
    dir.path().join("docs/notes.toml")
}

/// Read and parse `docs/notes.toml` via `cqs::parse_notes` (same code path
/// the `cqs notes list` handler uses). Returns an empty Vec if the file does
/// not exist so the caller can distinguish "file absent" from "empty list".
fn read_notes(dir: &TempDir) -> Vec<Note> {
    let path = notes_path(dir);
    if !path.exists() {
        return Vec::new();
    }
    cqs::parse_notes(&path).expect("parse_notes should succeed on test fixture")
}

/// Invoke `cqs --json notes add` and return the parsed JSON status envelope.
///
/// `--json` is a *global* (`Cli`) flag, so it must precede the subcommand.
fn notes_add_json(dir: &TempDir, text: &str, sentiment: &str, mentions: Option<&str>) -> Value {
    let mut args: Vec<&str> = vec![
        "--json",
        "notes",
        "add",
        text,
        "--sentiment",
        sentiment,
        "--no-reindex",
    ];
    if let Some(m) = mentions {
        args.push("--mentions");
        args.push(m);
    }

    let output = cqs()
        .args(&args)
        .current_dir(dir.path())
        .output()
        .expect("cqs notes add failed to spawn");
    assert!(
        output.status.success(),
        "cqs notes add failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("notes add JSON parse failed")
}

/// TC-HP-2a: add creates `docs/notes.toml` (including the parent `docs/`
/// directory) and persists exactly one note readable through `parse_notes`.
/// Covers `ensure_notes_file` mkdir, `rewrite_notes_file` append, and
/// round-trip through the parser.
#[test]
#[serial]
fn test_notes_add_creates_file_and_persists() {
    let dir = setup_notes_project();
    assert!(
        !notes_path(&dir).exists(),
        "notes.toml should not exist pre-add"
    );

    let json = notes_add_json(&dir, "hello from CLI", "0.5", None);
    // CLI emits via `emit_json`, so payload is wrapped in {data, error, version}.
    assert_eq!(json["data"]["status"], "added");
    assert_eq!(json["data"]["file"], "docs/notes.toml");
    // text_preview is either the full text or "first-100-chars...".
    assert!(
        json["data"]["text_preview"]
            .as_str()
            .unwrap()
            .contains("hello from CLI"),
        "text_preview should echo the note text, got {:?}",
        json["data"]["text_preview"]
    );
    // sentiment 0.5 lands in the "pattern" bucket (above +0.3).
    assert_eq!(json["data"]["type"], "pattern");

    assert!(
        notes_path(&dir).exists(),
        "notes.toml should be created by add"
    );

    let notes = read_notes(&dir);
    assert_eq!(notes.len(), 1, "parse_notes should return the added note");
    assert_eq!(notes[0].text, "hello from CLI");
    assert!(
        (notes[0].sentiment - 0.5).abs() < 1e-6,
        "sentiment round-trip failed: {}",
        notes[0].sentiment
    );
}

/// TC-HP-2b: update modifies text and sentiment in place.
#[test]
#[serial]
fn test_notes_update_changes_text_and_sentiment() {
    let dir = setup_notes_project();
    notes_add_json(&dir, "old text body", "0.0", None);

    cqs()
        .args([
            "--json",
            "notes",
            "update",
            "old text body",
            "--new-text",
            "new text body",
            "--new-sentiment",
            "-1",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    let notes = read_notes(&dir);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "new text body");
    assert!(
        (notes[0].sentiment - (-1.0)).abs() < 1e-6,
        "sentiment should be -1.0 after update, got {}",
        notes[0].sentiment
    );
    assert!(notes[0].is_warning());
}

/// TC-HP-2c: remove deletes the note by exact text match.
#[test]
#[serial]
fn test_notes_remove_deletes_note() {
    let dir = setup_notes_project();
    notes_add_json(&dir, "note to remove", "0.0", None);
    assert_eq!(read_notes(&dir).len(), 1);

    cqs()
        .args([
            "--json",
            "notes",
            "remove",
            "note to remove",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"removed\""));

    let after = read_notes(&dir);
    assert!(
        after.is_empty(),
        "read_notes should be empty after remove, got {} note(s)",
        after.len()
    );
}

/// TC-HP-2d: full add → update → remove lifecycle over a single notes.toml.
/// Spans all three mutation variants of `NotesCommand` against one file.
#[test]
#[serial]
fn test_notes_add_update_remove_lifecycle() {
    let dir = setup_notes_project();

    // 1. Add
    notes_add_json(&dir, "lifecycle note", "0.5", Some("foo.rs,bar"));
    let after_add = read_notes(&dir);
    assert_eq!(after_add.len(), 1);
    assert_eq!(after_add[0].text, "lifecycle note");
    assert!(after_add[0].mentions.iter().any(|m| m == "foo.rs"));
    assert!(after_add[0].mentions.iter().any(|m| m == "bar"));

    // 2. Update only sentiment (text unchanged)
    cqs()
        .args([
            "--json",
            "notes",
            "update",
            "lifecycle note",
            "--new-sentiment",
            "-0.5",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    let after_update = read_notes(&dir);
    assert_eq!(after_update.len(), 1);
    assert!(
        (after_update[0].sentiment - (-0.5)).abs() < 1e-6,
        "expected sentiment -0.5 after update, got {}",
        after_update[0].sentiment
    );
    // Mentions should be preserved across a sentiment-only update.
    assert!(after_update[0].mentions.iter().any(|m| m == "foo.rs"));

    // 3. Remove
    cqs()
        .args(["notes", "remove", "lifecycle note", "--no-reindex"])
        .current_dir(dir.path())
        .assert()
        .success();

    assert!(read_notes(&dir).is_empty());
}

/// TC-HP-2e: sentiment clamping at the CLI layer — passing 5.0 must round
/// down to 1.0 (see `cmd_notes_add` line 214) both in the JSON envelope and
/// on disk.
#[test]
#[serial]
fn test_notes_add_sentiment_clamps() {
    let dir = setup_notes_project();
    let json = notes_add_json(&dir, "clamp me", "5.0", None);
    // Sentiment field lives under data envelope.
    let sent = json["data"]["sentiment"].as_f64().unwrap();
    assert!(
        (sent - 1.0).abs() < 1e-6,
        "sentiment 5.0 must clamp to 1.0 in JSON envelope, got {sent}"
    );

    let notes = read_notes(&dir);
    assert_eq!(notes.len(), 1);
    assert!(
        (notes[0].sentiment - 1.0).abs() < 1e-6,
        "stored sentiment must also be clamped, got {}",
        notes[0].sentiment
    );
}

/// TC-HP-2f: update against a non-existent text errors cleanly instead of
/// silently rewriting the notes file.
#[test]
#[serial]
fn test_notes_update_missing_text_errors() {
    let dir = setup_notes_project();
    notes_add_json(&dir, "real note", "0.0", None);

    cqs()
        .args([
            "notes",
            "update",
            "does not exist",
            "--new-text",
            "anything",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .assert()
        .failure();

    // The original note must still be there, untouched.
    let notes = read_notes(&dir);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "real note");
}

/// TC-HP-2g: add rejects empty text at the validator boundary (line 207-209
/// of `cmd_notes_add`). The CLI must fail without creating notes.toml.
#[test]
#[serial]
fn test_notes_add_rejects_empty_text() {
    let dir = setup_notes_project();
    cqs()
        .args(["notes", "add", "", "--sentiment", "0", "--no-reindex"])
        .current_dir(dir.path())
        .assert()
        .failure();
    assert!(
        !notes_path(&dir).exists(),
        "Empty-text add must not create notes.toml"
    );
}

/// B.4: `cqs notes add ... --sentiment NaN` must be rejected at clap parse
/// time. Earlier `--sentiment` had no `value_parser`, so `NaN` slipped
/// through, was clamped via `f32::clamp` (which propagates NaN → still NaN),
/// and got written into `notes.toml` — poisoning every downstream consumer
/// that reads sentiment as a sort key. The fix wires the existing
/// `parse_finite_f32` parser onto the flag.
#[test]
#[serial]
fn test_notes_add_rejects_nan_sentiment() {
    let dir = setup_notes_project();
    let output = cqs()
        .args([
            "notes",
            "add",
            "noise",
            "--sentiment",
            "NaN",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .output()
        .expect("cqs notes add (NaN) failed to spawn");
    assert!(
        !output.status.success(),
        "--sentiment NaN must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("finite") || stderr.contains("NaN") || stderr.contains("invalid"),
        "stderr should explain why NaN was rejected, got: {stderr}"
    );
    assert!(
        !notes_path(&dir).exists(),
        "Rejected --sentiment NaN must not create notes.toml"
    );
}

/// B.4 (companion): `cqs notes add ... --sentiment Infinity` must also be
/// rejected — same parser path (`parse_finite_f32`).
#[test]
#[serial]
fn test_notes_add_rejects_infinity_sentiment() {
    let dir = setup_notes_project();
    let output = cqs()
        .args([
            "notes",
            "add",
            "noise",
            "--sentiment",
            "Infinity",
            "--no-reindex",
        ])
        .current_dir(dir.path())
        .output()
        .expect("cqs notes add (Infinity) failed to spawn");
    assert!(
        !output.status.success(),
        "--sentiment Infinity must exit non-zero. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
}
