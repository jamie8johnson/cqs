//! TC-HAP-1.29-4 — `cqs project search` CLI integration test.
//!
//! The `Register/List/Remove` subcommands already have CLI tests in
//! `tests/cli_surface_test.rs`, but `Search` — the one that actually runs
//! `search_across_projects` — has only an envelope-shape test in
//! `tests/cli_envelope_test.rs` against an empty registry. Nothing pins
//! the end-to-end behaviour that results come back from multiple projects
//! and carry correct `project` tags.
//!
//! This file:
//!   1. spins up two temp-dir projects with distinct content
//!      (`foo_alpha` vs `foo_beta`),
//!   2. `cqs init && cqs index`es each,
//!   3. `cqs project register`s both into an isolated registry (tests
//!      rewrite `$XDG_CONFIG_HOME` so the per-machine
//!      `~/.config/cqs/projects.toml` is not touched; `HOME` is left
//!      alone so `hf_hub` reuses the host's shared model cache),
//!   4. runs `cqs --json project search "foo"`,
//!   5. asserts: (a) at least one result from each project, (b) each
//!      result's `project` field matches the registered name, (c) the
//!      CLI exited 0.
//!
//! Subprocess pattern + `slow-tests` gate: `cqs index` cold-loads the
//! embedder (real semantic embeddings are required for meaningful
//! cross-project search).

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Builder that adds the XDG isolation envs to a `cqs` invocation.
/// `xdg` is the tempdir that stands in as both config and data root.
/// We isolate XDG (where `projects.toml` / `refs` live) but leave `HOME`
/// alone so `hf_hub` keeps using the host's shared model cache.
fn cqs_with_xdg(xdg: &std::path::Path) -> Command {
    let mut cmd = cqs();
    cmd.env("XDG_CONFIG_HOME", xdg.join("config"));
    cmd.env("XDG_DATA_HOME", xdg.join("data"));
    cmd.env("CQS_NO_DAEMON", "1");
    cmd
}

/// Build a single project rooted at `dir` with `content` written to
/// `src/lib.rs`, then `cqs init && cqs index`.
///
/// We index explicitly (not just rely on lazy indexing) so the store on
/// disk is ready the moment `cqs project register` checks for
/// `.cqs/index.db`.
fn setup_project_with(dir: &std::path::Path, content: &str, xdg: &std::path::Path) {
    let src = dir.join("src");
    fs::create_dir_all(&src).expect("create src dir");
    fs::write(src.join("lib.rs"), content).expect("write lib.rs");

    cqs_with_xdg(xdg)
        .args(["init"])
        .current_dir(dir)
        .assert()
        .success();

    cqs_with_xdg(xdg)
        .args(["index"])
        .current_dir(dir)
        .assert()
        .success();
}

/// TC-HAP-1.29-4: register two projects with distinct indexable content,
/// search for a shared term, assert results tagged with both project names.
///
/// `foo_alpha` and `foo_beta` are deliberately chosen so the shared token
/// (`foo`) drives the semantic match and the suffix (`_alpha` / `_beta`)
/// lets us tie each result back to the source project without relying on
/// file-path heuristics.
#[test]
#[serial]
fn test_project_search_interleaves_results_from_multiple_projects() {
    // Single XDG tempdir shared across both projects —
    // `search_across_projects` loads the registry from
    // `config_dir()/cqs/projects.toml` (= `$XDG_CONFIG_HOME/cqs/...`
    // on Linux), so both `register` calls must point at the same XDG
    // root to end up in the same registry.
    let xdg = TempDir::new().expect("xdg tempdir");

    let proj_alpha = TempDir::new().expect("alpha tempdir");
    let proj_beta = TempDir::new().expect("beta tempdir");

    setup_project_with(
        proj_alpha.path(),
        r#"
/// Entry point for pipeline alpha.
pub fn foo_alpha(x: i32) -> i32 {
    // Alpha-specific processing.
    x + 1
}
"#,
        xdg.path(),
    );
    setup_project_with(
        proj_beta.path(),
        r#"
/// Entry point for pipeline beta.
pub fn foo_beta(x: i32) -> i32 {
    // Beta-specific processing.
    x * 2
}
"#,
        xdg.path(),
    );

    // Register both projects. `cqs project register` checks for
    // `.cqs/index.db`; setup_project_with already ran `cqs index`.
    cqs_with_xdg(xdg.path())
        .args([
            "project",
            "register",
            "alpha",
            proj_alpha.path().to_str().expect("alpha utf8 path"),
        ])
        .assert()
        .success();
    cqs_with_xdg(xdg.path())
        .args([
            "project",
            "register",
            "beta",
            proj_beta.path().to_str().expect("beta utf8 path"),
        ])
        .assert()
        .success();

    // Search. `-n 10` is plenty for 2 projects × 1 function each. A low
    // threshold keeps the test robust against minor embedding drift.
    let output = cqs_with_xdg(xdg.path())
        .args([
            "--json", "project", "search", "foo", "-n", "10", "-t", "0.0",
        ])
        .output()
        .expect("cqs project search spawn");

    assert!(
        output.status.success(),
        "cqs project search must exit 0. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("envelope JSON parse failed: {e}\nstdout={stdout}"));

    // Envelope shape (emit_json wraps payload in {data, error, version}).
    assert_eq!(envelope["version"], 1, "envelope version must be 1");
    assert!(envelope["error"].is_null(), "envelope.error must be null");
    let results = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("data must be an array: {stdout}"));
    assert!(
        !results.is_empty(),
        "expected at least one result from each project, got 0: {stdout}"
    );

    // Each result carries a project tag matching one of the registered names.
    // The `project` field is a load-bearing contract — agents routing
    // follow-up queries to a specific project grep on it.
    let projects: std::collections::HashSet<&str> = results
        .iter()
        .map(|r| {
            r["project"]
                .as_str()
                .unwrap_or_else(|| panic!("each result must have a string `project` field: {r}"))
        })
        .collect();

    assert!(
        projects.contains("alpha"),
        "at least one result must come from project 'alpha'. got projects={:?} results={stdout}",
        projects
    );
    assert!(
        projects.contains("beta"),
        "at least one result must come from project 'beta'. got projects={:?} results={stdout}",
        projects
    );

    // No stray project names — `project` field should only contain
    // registered names.
    for name in &projects {
        assert!(
            *name == "alpha" || *name == "beta",
            "unexpected project name in results: {name}"
        );
    }
}
