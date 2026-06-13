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
//! - `overlay_scout_cli_direct_serves_parent` (inverts the old
//!   `overlay_scout_not_overlaid` scope guard): scout's seed is overlay-capable
//!   now, but only on the DAEMON path. CLI-direct (no daemon) serves the parent
//!   index — no `worktree_overlay` / `overlay_graph` meta. The active seed-
//!   overlay path is covered daemon-side in `handlers/search.rs`.
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

/// #14 — the regression fence (default-on flip). In a regular repo /
/// main checkout (no parent worktree to overlay onto), the overlay must stay OFF
/// whether or not `--overlay` is passed — `overlay_root` returns None, so the
/// default-on path never fires. Both `--overlay` and the no-flag default must be
/// byte-identical to an explicit flag-off search. This is the recall-gate fence:
/// `cqs eval` runs from the main checkout and must be unaffected by the flip.
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

    // A regular `tempfile::TempDir` is not a git worktree, so `overlay_root`
    // returns None regardless of flag — default-on cannot fire here.
    let default_run = run_json(cqs(dir.path()).args(["widget total", "--json", "-n", "5"]));
    let explicit_off =
        run_json(cqs(dir.path()).args(["widget total", "--json", "-n", "5", "--no-overlay"]));
    let on = run_json(cqs(dir.path()).args(["widget total", "--json", "-n", "5", "--overlay"]));

    assert_eq!(
        default_run, on,
        "main-checkout default vs --overlay must be byte-identical (default-on \
         never fires outside a worktree); default={default_run}\non={on}"
    );
    assert_eq!(
        default_run, explicit_off,
        "main-checkout default vs --no-overlay must be byte-identical; \
         default={default_run}\nexplicit_off={explicit_off}"
    );
    // And no overlay meta on any (regular repo → not eligible).
    for (label, v) in [
        ("default", &default_run),
        ("--overlay", &on),
        ("--no-overlay", &explicit_off),
    ] {
        assert!(
            v.get("_meta")
                .and_then(|m| m.get("worktree_overlay"))
                .is_none(),
            "regular repo ({label}) must emit no worktree_overlay meta; got {v}"
        );
    }
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

/// Default-on flip: a NO-FLAG search from an eligible worktree now
/// activates the overlay by default. With the daemon disabled it degrades to
/// `skipped-no-daemon` (CLI-direct can't build, phase 1) — but QUIETLY: a
/// default-on feature must NOT warn on every worktree query when the daemon is
/// down. The skip meta is still present so JSON consumers can see it.
#[test]
fn overlay_default_on_from_worktree_skips_no_daemon_quietly() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    let out = cqs(&wt)
        .args(["alpha", "--json", "-n", "5"]) // no --overlay flag
        .output()
        .expect("spawn cqs from worktree");
    assert!(
        out.status.success(),
        "default-on worktree search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(
        v.get("_meta").and_then(|m| m.get("worktree_overlay")),
        Some(&Value::String("skipped-no-daemon".into())),
        "default-on overlay from a worktree must mark skipped-no-daemon; got {v}"
    );
    // QUIET degradation: the explicit-request warn must NOT appear for a default
    // activation. (Debug-level skip line is fine; the `warn!` line is not.)
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("overlay skipped: daemon not running"),
        "default-on degradation must be quiet (no warn); got stderr: {stderr}"
    );
}

/// Opt-out beats default-on. From the same eligible worktree, `--no-overlay`
/// and `CQS_WORKTREE_OVERLAY=0` each suppress the default-on activation, so no
/// `worktree_overlay` meta is emitted at all (the overlay never engaged).
#[test]
fn overlay_opt_out_beats_default_on_from_worktree() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // --no-overlay flag.
    let v_flag = run_json(cqs(&wt).args(["alpha", "--json", "-n", "5", "--no-overlay"]));
    assert!(
        v_flag
            .get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "--no-overlay must suppress default-on (no overlay meta); got {v_flag}"
    );

    // CQS_WORKTREE_OVERLAY=0 env opt-out.
    let mut cmd = cqs(&wt);
    cmd.env("CQS_WORKTREE_OVERLAY", "0");
    let v_env = run_json(cmd.args(["alpha", "--json", "-n", "5"]));
    assert!(
        v_env
            .get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "CQS_WORKTREE_OVERLAY=0 must suppress default-on (no overlay meta); got {v_env}"
    );
}

/// Part A — scout's seed IS overlay-capable now (this inverts the old
/// `overlay_scout_not_overlaid` scope guard). The seed retrieval routes through
/// the worktree overlay on the DAEMON path. This CLI-direct test (daemon
/// disabled) pins the honest CLI-direct behavior: scout serves the PARENT index
/// — it cannot build an overlay without a daemon (phase 1, like search). The
/// active seed-overlay path (a worktree-added symbol surfacing as a scout/gather
/// seed + the `_meta.overlay_graph = "seed-only"` marker) is covered by the
/// daemon-side end-to-end tests `overlay_scout_seed_overlaid` /
/// `overlay_gather_seed_overlaid` in `src/cli/batch/handlers/search.rs`.
///
/// CLI-direct scout emits NO `worktree_overlay` meta (it didn't overlay) and NO
/// `overlay_graph` marker (the marker rides only when the overlay was applied).
/// Note the asymmetry with `search`, which marks `skipped-no-daemon` on
/// CLI-direct — scout's CLI-direct honest-degradation marker is deferred (see
/// the lane report's residual note).
#[test]
fn overlay_scout_cli_direct_serves_parent() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // Request the overlay via the env equivalent; CLI-direct still can't build.
    let mut cmd = cqs(&wt);
    cmd.env("CQS_WORKTREE_OVERLAY", "1");
    let v = run_json(cmd.args(["scout", "alpha", "--json"]));
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "CLI-direct scout serves the parent index — no worktree_overlay meta; got {v}"
    );
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("overlay_graph"))
            .is_none(),
        "CLI-direct scout did not overlay — no overlay_graph marker; got {v}"
    );
}
