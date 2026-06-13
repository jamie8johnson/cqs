//! CLI integration tests for the worktree-search-overlay query surface
//! (result-trust §3, plan `docs/plans/2026-06-12-worktree-overlay-implementation.md`
//! §11 PR-2 set). These exercise the `--overlay` flag end-to-end through the
//! real `cqs` binary:
//!
//! - **#14** `overlay_no_worktree_no_overlay`: `--overlay` in a regular repo is
//!   byte-identical to a flag-off search (the non-negotiable regression fence —
//!   `retrieve_project` is traversed by every search).
//! - **#15** `overlay_cli_direct_degrades_honestly`: from a worktree, no daemon,
//!   `--overlay` serves the parent index and marks
//!   `_meta.worktree_overlay = "skipped-no-daemon"` + warns.
//! - **#19** `overlay_scout_not_overlaid`: `cqs scout` from a worktree emits NO
//!   `worktree_overlay` meta — the scope guard (the overlay hook is
//!   `retrieve_project` + the FTS short-circuits, which scout bypasses).
//!
//! Gated behind `slow-tests`: each runs `cqs init`/`index`, which cold-loads
//! the embedder. The pure mask/merge/meta logic is covered unconditionally by
//! the `overlay_merge` unit module in `src/cli/commands/search/query.rs`.

#![cfg(feature = "slow-tests")]

mod common;

use common::worktree_fixture;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// A `cqs` invocation pinned to the v1 envelope (so `_meta` is always present
/// in a stable position) with the daemon disabled (CLI-direct path — phase 1
/// builds overlays daemon-side only, so CLI-direct is the honest-skip case).
fn cqs(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cqs"));
    c.current_dir(dir);
    c.env("CQS_OUTPUT_FORMAT", "v1");
    c.env("CQS_NO_DAEMON", "1");
    c
}

fn run_json(cmd: &mut Command) -> Value {
    let out = cmd.output().expect("spawn cqs");
    assert!(
        out.status.success(),
        "cqs failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "cqs stdout was not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn init_and_index(dir: &Path) {
    assert!(
        cqs(dir).arg("init").status().expect("init").success(),
        "cqs init failed"
    );
    assert!(
        cqs(dir).arg("index").status().expect("index").success(),
        "cqs index failed"
    );
}

/// #14 — the regression fence. In a regular repo (no parent worktree to overlay
/// onto), `--overlay` must produce byte-identical output to a flag-off search.
/// `retrieve_project` is on every search path, so an `apply_overlay` that
/// mutated the inactive path would corrupt all searches.
#[test]
fn overlay_no_worktree_no_overlay() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn compute_widget_total() -> i32 { 7 }\npub fn helper_routine() {}\n",
    )
    .unwrap();
    init_and_index(dir.path());

    let off = run_json(cqs(dir.path()).args(["widget total", "--json", "-n", "5"]));
    let on = run_json(cqs(dir.path()).args(["widget total", "--json", "-n", "5", "--overlay"]));

    assert_eq!(
        off, on,
        "--overlay in a regular repo must be byte-identical to flag-off; \
         off={off}\non={on}"
    );
    // And no overlay meta on either (regular repo → not eligible).
    assert!(
        on.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "regular repo must emit no worktree_overlay meta; got {on}"
    );
}

/// Index the parent corpus inside the `worktree_fixture` so reads from the
/// worktree resolve to the parent's `.cqs/` (the deliberate worktree→parent
/// redirect this whole feature exists to mitigate).
fn fixture_with_parent_index() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let (holder, parent, wt) = worktree_fixture();
    init_and_index(&parent);
    (holder, parent, wt)
}

/// #15 — CLI-direct honest degradation. From the worktree, with the daemon
/// disabled, `--overlay` is eligible (the worktree redirects to the parent
/// index) but cannot build (phase 1 builds daemon-side only). The search must
/// still succeed, serve the parent index, and mark the skip in `_meta`.
#[test]
fn overlay_cli_direct_degrades_honestly() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    let out = cqs(&wt)
        .args(["alpha", "--json", "-n", "5", "--overlay"])
        .output()
        .expect("spawn cqs from worktree");
    assert!(
        out.status.success(),
        "search from worktree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(
        v.get("_meta").and_then(|m| m.get("worktree_overlay")),
        Some(&Value::String("skipped-no-daemon".into())),
        "CLI-direct overlay must degrade to skipped-no-daemon; got {v}"
    );
    // The honest-degradation warning reaches stderr.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("overlay skipped") && stderr.contains("daemon not running"),
        "expected the honest-degradation warn on stderr; got: {stderr}"
    );
}

/// Flag-off from the worktree emits NO worktree_overlay meta (only the
/// pre-existing `worktree_stale` signal). Confirms the meta is opt-in.
#[test]
fn overlay_off_from_worktree_emits_no_overlay_meta() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    let v = run_json(cqs(&wt).args(["alpha", "--json", "-n", "5"]));
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "flag-off search must emit no worktree_overlay meta; got {v}"
    );
}

/// #19 — scope guard. `cqs scout` from a worktree, even with `--overlay`
/// requested, emits NO `worktree_overlay` meta: scout's seed retrieval bypasses
/// `query_core`/`retrieve_project` (the only overlay hook), so the overlay
/// cannot apply to it. A half-worktree graph/scout answer would be a new
/// calibration lie; the architecture excludes it.
#[test]
fn overlay_scout_not_overlaid() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // scout takes no --overlay flag; request it via the env equivalent to prove
    // the env path also doesn't leak into scout.
    let mut cmd = cqs(&wt);
    cmd.env("CQS_WORKTREE_OVERLAY", "1");
    let v = run_json(cmd.args(["scout", "alpha", "--json"]));
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "scout must never carry worktree_overlay meta (scope guard); got {v}"
    );
}
