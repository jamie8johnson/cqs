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
//! - `overlay_seed_family_cli_direct_marks_skipped_no_daemon`: scout / gather /
//!   task build their seed overlay on the DAEMON path only. CLI-direct (no
//!   daemon) serves the parent index but, like `search`, marks
//!   `_meta.worktree_overlay = "skipped-no-daemon"` so degradation is visible.
//!   Opt-out (`--no-overlay` / `=0`) and non-worktree runs emit no marker. The
//!   active seed-overlay path is covered daemon-side in `handlers/search.rs`.
//! - `overlay_graph_family_cli_direct_marks_skipped_no_daemon`: the pure-graph
//!   commands `callers` / `callees` / `impact` / `dead` carry the whole-result
//!   `skipped-no-daemon` marker CLI-direct (they serve the parent graph
//!   WHOLESALE).
//! - `diff_family_cli_direct_emits_two_axis_partial_skip`: the diff commands
//!   `review` / `ci` / `impact-diff` / `affected` instead emit a TWO-AXIS
//!   partial-skip pair CLI-direct — `_meta.overlay_graph = "diff-subject-only"`
//!   (coverage: the diff subject reflects the worktree, the caller graph does
//!   not) + `_meta.worktree_overlay = "graph-skipped-no-daemon"` (degradation:
//!   the graph splice wanted a daemon). A whole-result skip would deny the
//!   applied diff subject. Opt-out, non-worktree, and empty-diff runs emit
//!   nothing on either axis.
//!
//! Gated behind `slow-tests`: each runs `cqs init`/`index`, which cold-loads
//! the embedder. The pure mask/merge/meta logic is covered unconditionally by
//! the `overlay_merge` unit module in `src/cli/commands/search/query.rs`.

#![cfg(feature = "slow-tests")]

mod common;

use common::{git_in, worktree_fixture};
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

/// Part A degradation parity — scout / gather / task build their seed overlay on
/// the DAEMON path only. This CLI-direct test (daemon disabled) pins the honest
/// behavior: each serves the PARENT index but, like `search`, marks
/// `_meta.worktree_overlay = "skipped-no-daemon"` when the overlay was
/// wanted-and-skipped, so an agent reading the JSON can tell degradation
/// happened (the asymmetry search had and the seed family lacked). No
/// `overlay_graph` marker rides — that appears only when the overlay was
/// actually applied (daemon-side). The active seed-overlay path (a worktree-added
/// symbol surfacing as a seed + the `_meta.overlay_graph = "seed-only"` marker)
/// is covered by the daemon-side `overlay_scout_seed_overlaid` /
/// `overlay_gather_seed_overlaid` tests in `src/cli/batch/handlers/search.rs`.
#[test]
fn overlay_seed_family_cli_direct_marks_skipped_no_daemon() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // scout/gather take a query positional; task takes a description. `alpha`
    // (the fixture's committed symbol) works for all three.
    for cmd_name in ["scout", "gather", "task"] {
        // Request the overlay via the env equivalent; CLI-direct still can't build.
        let mut cmd = cqs(&wt);
        cmd.env("CQS_WORKTREE_OVERLAY", "1");
        let v = run_json(cmd.args([cmd_name, "alpha", "--json"]));
        assert_eq!(
            v.get("_meta").and_then(|m| m.get("worktree_overlay")),
            Some(&Value::String("skipped-no-daemon".into())),
            "CLI-direct {cmd_name} must mark skipped-no-daemon (degradation visible); got {v}"
        );
        assert!(
            v.get("_meta")
                .and_then(|m| m.get("overlay_graph"))
                .is_none(),
            "CLI-direct {cmd_name} did not overlay — no overlay_graph marker; got {v}"
        );
    }
}

/// Opt-out beats default-on for the seed family too: `--no-overlay` (and the
/// `=0` env) suppress the marker — parent-truth by contract, no degradation
/// signal. Representative over `scout` (all three share the marker helper).
#[test]
fn overlay_seed_family_opt_out_no_marker() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // --no-overlay flag.
    let v_flag = run_json(cqs(&wt).args(["scout", "alpha", "--json", "--no-overlay"]));
    assert!(
        v_flag
            .get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "--no-overlay scout must emit no worktree_overlay meta; got {v_flag}"
    );

    // CQS_WORKTREE_OVERLAY=0 env opt-out.
    let mut cmd = cqs(&wt);
    cmd.env("CQS_WORKTREE_OVERLAY", "0");
    let v_env = run_json(cmd.args(["scout", "alpha", "--json"]));
    assert!(
        v_env
            .get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "CQS_WORKTREE_OVERLAY=0 scout must emit no worktree_overlay meta; got {v_env}"
    );
}

/// Non-worktree parity: from a regular repo (no parent index to overlay onto),
/// the seed family emits no `worktree_overlay` marker even with an explicit
/// env request — the default-on flip never fires outside a worktree (the
/// recall-gate fence). Mirrors `overlay_no_worktree_no_overlay` for the seed
/// surface.
#[test]
fn overlay_seed_family_non_worktree_no_marker() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn compute_widget_total() -> i32 { 7 }\n",
    )
    .unwrap();
    init_and_index(dir.path());

    // A regular `tempfile::TempDir` is not a git worktree, so `overlay_root`
    // returns None and the marker never fires — even under an explicit env opt-in.
    let mut cmd = cqs(dir.path());
    cmd.env("CQS_WORKTREE_OVERLAY", "1");
    let v = run_json(cmd.args(["scout", "widget total", "--json"]));
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "non-worktree scout must emit no worktree_overlay meta; got {v}"
    );
}

/// Graph-family CLI-direct honest degradation. `callers` / `callees` / `impact`
/// / `dead` build their call-graph overlay daemon-side only (phase 1); a
/// CLI-direct run (daemon disabled) serves the PARENT call graph WHOLESALE. Like
/// `search` and the seed family, they now mark
/// `_meta.worktree_overlay = "skipped-no-daemon"` when the overlay was
/// wanted-and-skipped, so an agent reading the JSON can tell degradation
/// happened. No `overlay_graph` marker rides — that appears only when the overlay
/// actually applied (`"full"` on the daemon path). The active full-overlay merge
/// is covered daemon-side by the `callers_overlay` / `callees_overlay` /
/// `impact_overlay` / `dead_overlay` core tests.
///
/// RED before the fix: the graph adapters never called `mark_cli_direct_overlay_skip`,
/// so the marker was absent and this assertion failed.
#[test]
fn overlay_graph_family_cli_direct_marks_skipped_no_daemon() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // (command, positional). `alpha` is the fixture's committed symbol; `dead`
    // takes no positional (it scans the whole index).
    for (cmd_name, positional) in [
        ("callers", Some("alpha")),
        ("callees", Some("alpha")),
        ("impact", Some("alpha")),
        ("dead", None),
    ] {
        let mut cmd = cqs(&wt);
        cmd.env("CQS_WORKTREE_OVERLAY", "1");
        let mut args: Vec<&str> = vec![cmd_name];
        if let Some(p) = positional {
            args.push(p);
        }
        args.extend(["--json", "--overlay"]);
        let v = run_json(cmd.args(&args));
        assert_eq!(
            v.get("_meta").and_then(|m| m.get("worktree_overlay")),
            Some(&Value::String("skipped-no-daemon".into())),
            "CLI-direct {cmd_name} must mark skipped-no-daemon (degradation visible); got {v}"
        );
        assert!(
            v.get("_meta")
                .and_then(|m| m.get("overlay_graph"))
                .is_none(),
            "CLI-direct {cmd_name} did not overlay — no overlay_graph marker; got {v}"
        );
    }
}

/// Opt-out beats default-on for the graph family too: `--no-overlay` and the
/// `=0` env each suppress the marker — parent-truth by contract, no degradation
/// signal. Representative over `callers` (all four share the marker helper).
#[test]
fn overlay_graph_family_opt_out_no_marker() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // --no-overlay flag.
    let v_flag = run_json(cqs(&wt).args(["callers", "alpha", "--json", "--no-overlay"]));
    assert!(
        v_flag
            .get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "--no-overlay callers must emit no worktree_overlay meta; got {v_flag}"
    );

    // CQS_WORKTREE_OVERLAY=0 env opt-out.
    let mut cmd = cqs(&wt);
    cmd.env("CQS_WORKTREE_OVERLAY", "0");
    let v_env = run_json(cmd.args(["callers", "alpha", "--json"]));
    assert!(
        v_env
            .get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "CQS_WORKTREE_OVERLAY=0 callers must emit no worktree_overlay meta; got {v_env}"
    );
}

/// Non-worktree parity: from a regular repo (no parent index to overlay onto),
/// the graph family emits no `worktree_overlay` marker even under an explicit
/// env opt-in — the default-on flip never fires outside a worktree. Mirrors
/// `overlay_seed_family_non_worktree_no_marker` for the graph surface.
#[test]
fn overlay_graph_family_non_worktree_no_marker() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn compute_widget_total() -> i32 { 7 }\npub fn caller_of_widget() -> i32 { compute_widget_total() }\n",
    )
    .unwrap();
    init_and_index(dir.path());

    // A regular `tempfile::TempDir` is not a git worktree, so `overlay_root`
    // returns None and the marker never fires — even under an explicit env opt-in.
    let mut cmd = cqs(dir.path());
    cmd.env("CQS_WORKTREE_OVERLAY", "1");
    let v = run_json(cmd.args(["callers", "compute_widget_total", "--json"]));
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "non-worktree callers must emit no worktree_overlay meta; got {v}"
    );
}

/// The four diff commands + their CLI-direct `--json --overlay` arg lists. `ci`
/// pins `--gate off` so the gate result never changes the process exit (the
/// markers are gate-independent, and `run_json` asserts success).
const DIFF_CMDS: &[(&str, &[&str])] = &[
    ("review", &["review", "--json", "--overlay"]),
    ("ci", &["ci", "--json", "--gate", "off", "--overlay"]),
    ("impact-diff", &["impact-diff", "--json", "--overlay"]),
    ("affected", &["affected", "--json", "--overlay"]),
];

/// Assert the CLI-direct two-axis partial-skip marker PAIR: the coverage axis
/// `_meta.overlay_graph = "diff-subject-only"` AND the degradation axis
/// `_meta.worktree_overlay = "graph-skipped-no-daemon"`, both in the same result.
fn assert_partial_skip_pair(v: &Value, label: &str) {
    assert_eq!(
        v.get("_meta").and_then(|m| m.get("overlay_graph")),
        Some(&Value::String("diff-subject-only".into())),
        "{label}: coverage axis must be diff-subject-only; got {v}"
    );
    assert_eq!(
        v.get("_meta").and_then(|m| m.get("worktree_overlay")),
        Some(&Value::String("graph-skipped-no-daemon".into())),
        "{label}: degradation axis must be graph-skipped-no-daemon; got {v}"
    );
}

/// Assert NEITHER partial-skip axis is present (opt-out / non-worktree / empty
/// diff — skip-when-default on both axes).
fn assert_no_partial_skip_markers(v: &Value, label: &str) {
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("overlay_graph"))
            .is_none(),
        "{label}: overlay_graph must be absent; got {v}"
    );
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "{label}: worktree_overlay must be absent; got {v}"
    );
}

/// CLI-direct review emits the two-axis partial-skip vocabulary AND applies the
/// diff subject. Unlike the pure-graph commands (whole-result `skipped-no-daemon`),
/// review PARTIALLY applies the overlay: the diff subject IS the worktree
/// (`cli_diff_overlay_root` diffs this checkout, so `changed_functions` reflect
/// its uncommitted edits) while only the caller-graph splice is skipped. This
/// pins BOTH halves of the honesty contract: the marker PAIR is present
/// (coverage `"diff-subject-only"` + degradation `"graph-skipped-no-daemon"`)
/// AND the worktree's `alpha` edit is present in `changed_functions` — the
/// applied diff subject the coverage value describes.
///
/// RED before the fix: CLI-direct review emitted neither marker (deliberate
/// silence), so the pair assertion failed against main.
#[test]
fn review_cli_direct_emits_two_axis_partial_skip() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // Worktree-only edit to `alpha` (parent stays clean).
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 99 }\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("edit worktree lib.rs");

    // Explicit overlay request from the eligible worktree, daemon disabled.
    let v = run_json(cqs(&wt).args(["review", "--json", "--overlay"]));
    assert_partial_skip_pair(&v, "review");
    // The diff-subject half IS applied: the worktree's `alpha` edit is reviewed.
    assert!(
        review_changed_fn_names(&v).iter().any(|n| n == "alpha"),
        "review CLI-direct must diff the worktree (the `alpha` edit is present): {v}"
    );
}

/// Both axes together, across the WHOLE diff family. From an eligible worktree
/// with the daemon disabled and a worktree-only edit, every diff command emits
/// the marker PAIR. This is the both-axes assertion the design pins (coverage
/// value AND degradation value in one result), swept over all four commands.
/// RED against main: none of the four emitted either marker CLI-direct.
#[test]
fn diff_family_cli_direct_emits_two_axis_partial_skip() {
    let (_holder, _parent, wt) = fixture_with_parent_index();
    // Worktree-only edit to `alpha` — the diff subject the overlay applies.
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 99 }\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("edit worktree lib.rs");
    for (label, args) in DIFF_CMDS {
        let v = run_json(cqs(&wt).args(*args));
        assert_partial_skip_pair(&v, label);
    }
}

/// Opt-out suppresses BOTH axes for the whole family. To bite the opt-out gate
/// independent of the empty-diff gate, the PARENT working tree also holds a
/// (distinct) edit, so the `--no-overlay` diff subject (the served/main root) is
/// NON-empty — yet no marker rides, because the overlay never engaged.
#[test]
fn diff_family_opt_out_no_markers() {
    let (_holder, parent, wt) = fixture_with_parent_index();
    // Worktree edit (would be the overlay subject) ...
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 99 }\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("edit worktree lib.rs");
    // ... and a DISTINCT parent edit, so the opted-out (parent-truth) diff is
    // non-empty — isolating the opt-out gate from the emptiness gate.
    std::fs::write(
        parent.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 77 }\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("edit parent lib.rs");
    for (label, base_args) in DIFF_CMDS {
        let mut args: Vec<&str> = base_args
            .iter()
            .filter(|a| **a != "--overlay")
            .copied()
            .collect();
        args.push("--no-overlay");
        let v = run_json(cqs(&wt).args(&args));
        assert_no_partial_skip_markers(&v, &format!("{label} --no-overlay"));
    }
    // Sanity: the opted-out review really did diff the (non-empty) parent, so the
    // absence above is the opt-out gate biting, not an empty diff.
    let review = run_json(cqs(&wt).args(["review", "--json", "--no-overlay"]));
    assert!(
        review_changed_fn_names(&review)
            .iter()
            .any(|n| n == "alpha"),
        "opt-out review must diff the (edited) parent — `alpha` present: {review}"
    );
}

/// Non-worktree parity: from a standalone git repo (its own `.cqs/`, no parent to
/// overlay onto), the diff family emits neither axis even under an explicit
/// `--overlay` — `overlay_root` returns None, so the diff subject never resolves
/// to a worktree. A real git repo (not a linked worktree) is required so the
/// commands' `git diff` succeeds; the uncommitted edit makes the diff non-empty,
/// isolating the non-worktree gate from the emptiness gate.
#[test]
fn diff_family_non_worktree_no_markers() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn compute_widget_total() -> i32 { 7 }\npub fn caller_of_widget() -> i32 { compute_widget_total() }\n",
    )
    .unwrap();
    // Standalone git repo with a commit: `git diff` has a base, and the own
    // `.cqs/` makes `overlay_root` return None (not a linked worktree).
    git_in(root, &["init", "-q", "-b", "main"]);
    git_in(root, &["config", "user.email", "t@e.com"]);
    git_in(root, &["config", "user.name", "T"]);
    git_in(root, &["add", "-A"]);
    git_in(root, &["commit", "-q", "-m", "init"]);
    init_and_index(root);
    // Uncommitted edit so the diff is non-empty.
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn compute_widget_total() -> i32 { 99 }\npub fn caller_of_widget() -> i32 { compute_widget_total() }\n",
    )
    .unwrap();
    for (label, args) in DIFF_CMDS {
        let v = run_json(cqs(root).args(*args));
        assert_no_partial_skip_markers(&v, &format!("{label} non-worktree"));
    }
}

/// Empty diff suppresses BOTH axes for the whole family. From an eligible
/// worktree with NO uncommitted edit, the diff subject resolves to the worktree
/// (`overlay_root` IS Some), yet the diff is empty — nothing is overlaid, so
/// neither marker rides. Isolates the empty-diff gate (only the emptiness check
/// suppresses here, since the worktree probe succeeded).
#[test]
fn diff_family_empty_diff_no_markers() {
    let (_holder, _parent, wt) = fixture_with_parent_index();
    // No edit — the worktree is clean, so the overlay diff is empty.
    for (label, args) in DIFF_CMDS {
        let v = run_json(cqs(&wt).args(*args));
        assert_no_partial_skip_markers(&v, &format!("{label} empty-diff"));
    }
}

/// `changed_functions` names from a v1-envelope `cqs review --json` payload
/// (under `data`; falls back to a bare top-level array for envelope robustness).
fn review_changed_fn_names(v: &Value) -> Vec<String> {
    v.get("data")
        .and_then(|d| d.get("changed_functions"))
        .or_else(|| v.get("changed_functions"))
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// #2110 — `--no-overlay` diff-subject parity (CLI-direct). From an
/// (out-of-tree) worktree holding an uncommitted edit to `alpha`, the diff
/// subject the review describes must follow the overlay tri-state, matching the
/// daemon:
///
/// - **default-on** (worktree eligible): the diff comes from the WORKTREE, so
///   the lane's `alpha` edit appears in `changed_functions`.
/// - **`--no-overlay`**: full parent-truth — the diff subject reverts to the
///   served/main root (the parent is clean), so `alpha` is ABSENT.
///
/// Pre-fix the CLI-direct sites probed the cwd unconditionally and diffed the
/// worktree even under `--no-overlay`, so `alpha` leaked in — the divergence vs
/// the daemon this pins shut. The out-of-tree worktree also exercises
/// `cli_parent_diff_root` resolving main's root (`find_project_root` returns the
/// worktree root there).
#[test]
fn review_no_overlay_reverts_diff_subject_to_parent() {
    let (_holder, _parent, wt) = fixture_with_parent_index();

    // Worktree-only edit to `alpha` (parent stays clean).
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 99 }\npub fn beta() -> i32 { 2 }\n",
    )
    .expect("edit worktree lib.rs");

    // Overlay-ON control (default-on from an eligible worktree): the review
    // diffs the worktree, so the lane's `alpha` edit is present.
    let on = run_json(cqs(&wt).args(["review", "--json"]));
    assert!(
        review_changed_fn_names(&on).iter().any(|n| n == "alpha"),
        "default-on review from a worktree must diff the worktree (edits `alpha`): {on}"
    );

    // `--no-overlay`: parent-truth. The served/main root is clean, so `alpha`
    // (the worktree-only edit) must NOT appear.
    let off = run_json(cqs(&wt).args(["review", "--json", "--no-overlay"]));
    assert!(
        !review_changed_fn_names(&off).iter().any(|n| n == "alpha"),
        "--no-overlay review must diff the served parent (clean), so the worktree's \
         `alpha` edit must be ABSENT: {off}"
    );
}

/// A `cqs` invocation like [`cqs`] but with an isolated `XDG_DATA_HOME`, so a
/// `cqs ref add` writes its reference index under the fixture
/// (`data_local_dir()/cqs/refs/<name>`) instead of the developer's real
/// `~/.local/share`. The reference entry recorded in `.cqs.toml` is an absolute
/// path, so a later search resolves it regardless of this env — the isolation
/// only keeps the index build (and the model classifier centroids) off the real
/// home.
fn cqs_ref(dir: &Path, xdg_data: &Path) -> Command {
    let mut c = cqs(dir);
    c.env("XDG_DATA_HOME", xdg_data);
    c
}

/// [`fixture_with_parent_index`] plus a registered reference named `refname`
/// whose one-function corpus is indexed into an isolated XDG data dir.
///
/// The reference is added FROM the worktree so its entry lands in
/// `wt/.cqs.toml` — the config `find_reference` loads for a `--ref` /
/// `--include-refs` query run from the worktree, since `find_project_root`
/// resolves the worktree's own root (its `.git` link file) while the `.cqs/`
/// INDEX still redirects to the parent. Returns the fixture tuple plus the
/// persistent XDG data dir the caller keeps alive and stamps onto every
/// invocation that must see the reference.
fn fixture_with_parent_index_and_ref(
    refname: &str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let (holder, parent, wt) = fixture_with_parent_index();
    let xdg_data = holder.path().join("xdg-data");
    std::fs::create_dir_all(&xdg_data).expect("mkdir xdg-data");

    // Tiny reference corpus (one function), indexed into the isolated refs dir.
    let ref_src = holder.path().join("refsrc");
    std::fs::create_dir_all(&ref_src).expect("mkdir refsrc");
    std::fs::write(
        ref_src.join("reflib.rs"),
        "pub fn reference_marker_symbol() -> i32 { 11 }\n",
    )
    .expect("write reflib.rs");

    let added = cqs_ref(&wt, &xdg_data)
        .args([
            "ref",
            "add",
            refname,
            ref_src.to_str().expect("utf8 refsrc"),
        ])
        .status()
        .expect("spawn cqs ref add")
        .success();
    assert!(added, "cqs ref add must succeed");

    (holder, parent, wt, xdg_data)
}

/// A pure `--ref` query searches ONLY the reference store
/// (`retrieve_ref_scoped` never applies the worktree overlay — it is
/// parent-truth by construction), so from an eligible worktree with the daemon
/// disabled, `--overlay --ref <name>` degrades nothing and MUST emit no
/// `_meta.worktree_overlay` marker. Previously the marker was recorded before
/// the `--ref` branch, so a ref-scoped query overclaimed `skipped-no-daemon`.
/// This matches the daemon (its ref-scoped path is parent-truth by
/// construction) and the seed family's ref gate.
///
/// RED without the gate: the pre-fix `cmd_query` set the marker unconditionally,
/// so this assertion fails (the ref envelope carries `skipped-no-daemon`).
#[test]
fn overlay_ref_scoped_cli_direct_no_marker() {
    let (_holder, _parent, wt, xdg_data) = fixture_with_parent_index_and_ref("reflib");

    // `--threshold 0` guarantees the single ref chunk returns (non-empty → a
    // clean exit-0 result envelope for `run_json` to read); `--overlay` is the
    // explicit request that made the pre-fix marker fire.
    let v = run_json(cqs_ref(&wt, &xdg_data).args([
        "reference marker symbol",
        "--json",
        "-n",
        "5",
        "--threshold",
        "0",
        "--overlay",
        "--ref",
        "reflib",
    ]));
    assert!(
        v.get("_meta")
            .and_then(|m| m.get("worktree_overlay"))
            .is_none(),
        "pure --ref searches only the reference store (no overlay applies) — the \
         skipped-no-daemon marker must be ABSENT; got {v}"
    );
}

/// Control for the ref gate: `--include-refs` must KEEP the marker. Its project
/// half flows through `retrieve_project` (the overlay-capable path), so from an
/// eligible worktree with no daemon the overlay was wanted-and-skipped and the
/// marker fires. This pins the gate as specifically "ref-scoped only" — a
/// broader gate that also suppressed `--include-refs` would hide honest
/// degradation on the path that really does serve the parent for its project
/// half.
#[test]
fn overlay_include_refs_cli_direct_marks_skipped_no_daemon() {
    let (_holder, _parent, wt, xdg_data) = fixture_with_parent_index_and_ref("reflib");

    // `alpha` is the parent corpus's committed symbol, so the project half
    // returns results and the merged (project + reference) set is non-empty.
    let v = run_json(cqs_ref(&wt, &xdg_data).args([
        "alpha",
        "--json",
        "-n",
        "5",
        "--overlay",
        "--include-refs",
    ]));
    assert_eq!(
        v.get("_meta").and_then(|m| m.get("worktree_overlay")),
        Some(&Value::String("skipped-no-daemon".into())),
        "--include-refs degrades for real (its project half overlays) — the \
         skipped-no-daemon marker must be PRESENT; got {v}"
    );
}
