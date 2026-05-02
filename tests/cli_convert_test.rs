#![cfg(feature = "slow-tests")]
//! Reason this file is gated: subprocess-spawning CLI tests for `cqs convert`.
//! Each test runs the full cqs binary cold start which is too expensive for
//! PR-time CI. Run via `cargo test --features slow-tests` or nightly
//! ci-slow.yml.
//!
//! TC-HAP-V1.33-1: `cqs convert` and `convert_path` had zero end-to-end
//! tests. The submodules (html.rs, pdf.rs, chm.rs, cleaning.rs, naming.rs)
//! have unit tests for their own helpers, but the entry-point glue —
//! `cmd_convert` orchestrating overwrite-vs-skip, dry-run, the `output_dir`
//! SEC-4 canonicalize/warn logic, and the `clean_tags` CSV split — was
//! reachable only by the binary at runtime. These tests pin the orchestrator.

use assert_cmd::Command;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// TC-HAP-V1.33-1: single-file HTML conversion writes Markdown to the
/// inferred output dir (parent of the source). Pins the happy path
/// through `cmd_convert` → `convert_path` → `convert_file`.
#[test]
#[serial]
fn convert_html_file_writes_markdown_to_inferred_dir() {
    let dir = TempDir::new().expect("tempdir");
    let html_path = dir.path().join("page.html");
    fs::write(
        &html_path,
        "<html><body><h1>Title</h1><p>Body text.</p></body></html>",
    )
    .expect("write html");

    let assert = cqs()
        .args(["convert", html_path.to_str().unwrap()])
        .current_dir(dir.path())
        .assert()
        .success();

    let out = assert.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Default: output dir is parent of source (the tempdir itself).
    // The converter writes a file with a name derived from the title.
    let entries: Vec<_> = fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let md_files: Vec<&String> = entries.iter().filter(|n| n.ends_with(".md")).collect();
    assert!(
        !md_files.is_empty(),
        "convert must write at least one .md file. \
         dir contents: {entries:?}\nstdout: {stdout}"
    );
}

/// TC-HAP-V1.33-1: directory containing multiple HTML files converts
/// each. Pins the `convert_directory` branch of `convert_path`.
#[test]
#[serial]
fn convert_directory_processes_multiple_files() {
    let dir = TempDir::new().expect("tempdir");
    let src_dir = dir.path().join("docs");
    fs::create_dir_all(&src_dir).expect("mkdir docs");
    fs::write(
        src_dir.join("alpha.html"),
        "<html><body><h1>Alpha</h1><p>A.</p></body></html>",
    )
    .expect("write alpha");
    fs::write(
        src_dir.join("beta.html"),
        "<html><body><h1>Beta</h1><p>B.</p></body></html>",
    )
    .expect("write beta");

    cqs()
        .args(["convert", src_dir.to_str().unwrap()])
        .current_dir(dir.path())
        .assert()
        .success();

    // Output goes into the source dir by default.
    let entries: Vec<_> = fs::read_dir(&src_dir)
        .expect("readdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let md_count = entries.iter().filter(|n| n.ends_with(".md")).count();
    assert!(
        md_count >= 2,
        "directory convert must produce ≥2 .md files for 2 inputs. \
         got: {entries:?}"
    );
}

/// TC-HAP-V1.33-1: `--dry-run` must NOT write any files. Pins the
/// dry-run branch in `convert_file` / `finalize_output`.
#[test]
#[serial]
fn convert_dry_run_does_not_write_files() {
    let dir = TempDir::new().expect("tempdir");
    let html_path = dir.path().join("page.html");
    fs::write(
        &html_path,
        "<html><body><h1>Dry</h1><p>Run.</p></body></html>",
    )
    .expect("write html");

    cqs()
        .args(["convert", html_path.to_str().unwrap(), "--dry-run"])
        .current_dir(dir.path())
        .assert()
        .success();

    // No .md files should exist.
    let entries: Vec<_> = fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let md_count = entries.iter().filter(|n| n.ends_with(".md")).count();
    assert_eq!(
        md_count, 0,
        "dry-run must not write any .md files; got: {entries:?}"
    );
}
