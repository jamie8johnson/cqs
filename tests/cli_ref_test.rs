//! TC-HAP-1.29-5 — `cqs ref {add,list,remove,update}` CLI integration tests.
//!
//! The 4 subcommands under `cqs ref` do real work: `add` runs
//! `enumerate_files` + `run_index_pipeline` + `build_hnsw_index` +
//! `add_reference_to_config`; `list` reads every reference's DB for a
//! chunk count; `remove` validates existence + rewrites config + deletes
//! the reference dir; `update` re-indexes from source. Prior to v1.29.1
//! only the library helpers (`merge_results`, `search_reference`,
//! `validate_ref_name`) had tests — no integration test exercised the
//! CLI happy path. `tests/cli_drift_diff_test.rs` calls `cqs ref add` as
//! setup but only asserts on drift output, not the ref add shape.
//!
//! Subprocess pattern + `slow-tests` gate: `cqs ref add` cold-loads the
//! embedder (it indexes the source). Isolated via per-test
//! `$XDG_CONFIG_HOME` + `$XDG_DATA_HOME` so the global
//! `config_dir()/cqs/projects.toml` and `data_local_dir()/cqs/refs`
//! don't collide with the dev machine's real state. `HOME` is left
//! untouched so `hf_hub` sees the host's shared model cache (otherwise
//! every test would re-download ~1 GB of weights).
//!
//! `#[serial]` because refs share the XDG-scoped global directory and
//! the `Cli::find_project_root` walk uses the process CWD.

#![cfg(feature = "slow-tests")]

use assert_cmd::Command;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn cqs() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("cqs").expect("Failed to find cqs binary")
}

/// Write a small Rust source tree to `dir` with a single public function,
/// one per file. Returns the set of (file-path, function-name) for callers
/// that need to verify later chunk counts.
fn seed_source(dir: &Path) {
    fs::create_dir_all(dir).expect("mkdir source");
    fs::write(
        dir.join("lib_a.rs"),
        r#"
/// Process data alpha path.
pub fn process_alpha(x: i32) -> i32 {
    x + 1
}
"#,
    )
    .expect("write lib_a.rs");
    fs::write(
        dir.join("lib_b.rs"),
        r#"
/// Process data beta path.
pub fn process_beta(y: i32) -> i32 {
    y * 2
}
"#,
    )
    .expect("write lib_b.rs");
}

/// Host project (the CWD for `cqs ref` commands — reference entries get
/// written into `<host>/.cqs.toml`). Must have a `.cqs/` marker so
/// `find_project_root` stops walking upward before it hits `/tmp`.
fn setup_host() -> TempDir {
    let host = TempDir::new().expect("host tempdir");
    fs::create_dir_all(host.path().join(".cqs")).expect("mkdir .cqs");
    // Touch an empty `.cqs.toml` so `Config::load` finds the project
    // root consistently — otherwise `find_project_root` may pick the
    // temp-parent.
    fs::write(host.path().join(".cqs.toml"), "").expect("touch .cqs.toml");
    host
}

/// Builder that adds the XDG isolation envs to a `cqs` invocation.
/// `xdg` is the tempdir that stands in as both config and data root.
fn cqs_with_xdg(xdg: &Path) -> Command {
    let mut cmd = cqs();
    // `dirs::config_dir()` → `$XDG_CONFIG_HOME` on Linux. Project registry
    // lives at `config_dir()/cqs/projects.toml`; refs at
    // `data_local_dir()/cqs/refs`. Isolating both keeps each test run
    // from seeing the host's real projects/refs.
    cmd.env("XDG_CONFIG_HOME", xdg.join("config"));
    cmd.env("XDG_DATA_HOME", xdg.join("data"));
    cmd.env("CQS_NO_DAEMON", "1");
    cmd
}

/// `cqs ref add <name> <source>` with isolation env vars. Panics on
/// non-zero exit so tests surface the failing stderr immediately.
fn ref_add(xdg: &Path, host: &Path, name: &str, source: &Path) {
    cqs_with_xdg(xdg)
        .args([
            "ref",
            "add",
            name,
            source.to_str().expect("source utf8 path"),
        ])
        .current_dir(host)
        .assert()
        .success();
}

fn ref_list_json(xdg: &Path, host: &Path) -> Value {
    let output = cqs_with_xdg(xdg)
        .args(["--json", "ref", "list"])
        .current_dir(host)
        .output()
        .expect("cqs ref list spawn");
    assert!(
        output.status.success(),
        "cqs ref list must succeed. stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "ref list JSON parse failed: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// TC-HAP-1.29-5a: `ref add` → `ref list --json` reports the reference
/// with a chunk count ≥ 2 (one function per seeded file → at least one
/// chunk per file). Pins the round-trip through `.cqs.toml` plus the
/// per-reference chunk-count query in `cmd_ref_list`.
#[test]
#[serial]
fn test_ref_add_then_list_shows_reference() {
    let xdg = TempDir::new().expect("xdg tempdir");
    let host = setup_host();
    let source = TempDir::new().expect("source tempdir");
    seed_source(source.path());

    ref_add(xdg.path(), host.path(), "mylib", source.path());

    let envelope = ref_list_json(xdg.path(), host.path());
    // emit_json envelope: {data: [...], error: null, version: 1}
    let entries = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("data must be an array: {envelope}"));
    assert_eq!(
        entries.len(),
        1,
        "exactly one reference expected: {envelope}"
    );
    let entry = &entries[0];
    assert_eq!(entry["name"], "mylib", "name field must echo the add arg");
    assert!(
        entry["path"].is_string(),
        "path field must be string: {entry}"
    );
    let chunks = entry["chunks"]
        .as_u64()
        .unwrap_or_else(|| panic!("chunks field must be u64: {entry}"));
    assert!(
        chunks >= 2,
        "expected ≥2 chunks (one public fn per seed file), got {chunks}: {entry}"
    );
}

/// TC-HAP-1.29-5b: `ref add` → `ref remove` → `ref list --json` returns
/// an empty array. Pins the config-rewrite path + disk-cleanup path.
#[test]
#[serial]
fn test_ref_remove_deletes_from_config() {
    let xdg = TempDir::new().expect("xdg tempdir");
    let host = setup_host();
    let source = TempDir::new().expect("source tempdir");
    seed_source(source.path());

    ref_add(xdg.path(), host.path(), "tmpref", source.path());
    // Sanity: present before remove.
    assert_eq!(
        ref_list_json(xdg.path(), host.path())["data"]
            .as_array()
            .map(|a| a.len()),
        Some(1),
    );

    cqs_with_xdg(xdg.path())
        .args(["ref", "remove", "tmpref"])
        .current_dir(host.path())
        .assert()
        .success();

    let envelope = ref_list_json(xdg.path(), host.path());
    let entries = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("data must be an array: {envelope}"));
    assert!(
        entries.is_empty(),
        "list must be empty after remove, got {entries:?}"
    );
}

/// TC-HAP-1.29-5c: `ref add` a tiny corpus, edit a source file to add
/// a new public function, run `ref update`, then confirm the reported
/// chunk count grew. Pins the reindex path + prune path through
/// `run_index_pipeline`.
#[test]
#[serial]
fn test_ref_update_reindexes_source() {
    let xdg = TempDir::new().expect("xdg tempdir");
    let host = setup_host();
    let source = TempDir::new().expect("source tempdir");
    seed_source(source.path());

    ref_add(xdg.path(), host.path(), "evolving", source.path());
    let before = ref_list_json(xdg.path(), host.path());
    let chunks_before = before["data"][0]["chunks"].as_u64().expect("chunks before");

    // Add a new public function to lib_a.rs — `ref update` should pick
    // up the new chunk.
    fs::write(
        source.path().join("lib_a.rs"),
        r#"
/// Process data alpha path.
pub fn process_alpha(x: i32) -> i32 {
    x + 1
}

/// Additional processing step (NEW).
pub fn process_gamma(z: i32) -> i32 {
    z - 1
}
"#,
    )
    .expect("rewrite lib_a.rs");

    cqs_with_xdg(xdg.path())
        .args(["ref", "update", "evolving"])
        .current_dir(host.path())
        .assert()
        .success();

    let after = ref_list_json(xdg.path(), host.path());
    let chunks_after = after["data"][0]["chunks"].as_u64().expect("chunks after");
    assert!(
        chunks_after > chunks_before,
        "chunk count must grow after adding a new function. before={chunks_before} after={chunks_after}"
    );
}

/// TC-HAP-1.29-5d: `--weight` outside [0.0, 1.0] must be rejected at
/// the CLI layer before any indexing work starts. Pins the
/// `parse_unit_f32` value-parser contract.
///
/// Library-level coverage already exists for the `bail!` in
/// `cmd_ref_add` — this is the clap-level complement.
#[test]
#[serial]
fn test_ref_add_weight_rejects_out_of_range() {
    let xdg = TempDir::new().expect("xdg tempdir");
    let host = setup_host();
    let source = TempDir::new().expect("source tempdir");
    seed_source(source.path());

    // Weight > 1.0 is out of contract.
    let output = cqs_with_xdg(xdg.path())
        .args([
            "ref",
            "add",
            "tooheavy",
            source.path().to_str().expect("source utf8 path"),
            "--weight",
            "1.5",
        ])
        .current_dir(host.path())
        .output()
        .expect("cqs ref add spawn");

    assert!(
        !output.status.success(),
        "--weight 1.5 must exit non-zero. stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Belt-and-braces: the negative case must also be rejected.
    let output_neg = cqs_with_xdg(xdg.path())
        .args([
            "ref",
            "add",
            "toolight",
            source.path().to_str().expect("source utf8 path"),
            "--weight",
            "-0.1",
        ])
        .current_dir(host.path())
        .output()
        .expect("cqs ref add spawn");

    assert!(
        !output_neg.status.success(),
        "--weight -0.1 must exit non-zero. stderr={}",
        String::from_utf8_lossy(&output_neg.stderr),
    );

    // And nothing should have landed in the config — both attempts bailed.
    let envelope = ref_list_json(xdg.path(), host.path());
    let entries = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("data must be an array: {envelope}"));
    assert!(
        entries.is_empty(),
        "rejected ref add must not add entries to config, got {entries:?}"
    );
}
