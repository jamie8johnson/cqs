//! Worktree write guard end-to-end.
//!
//! From a git worktree nested under a parent Cargo workspace, cqs's
//! project-root discovery walks up past the worktree's own `.git` to the
//! parent's index. The worktree→main-index discovery makes this
//! deliberate for *reads*; a WRITE command silently mutating the parent
//! index defeats worktree isolation. The dispatch-time guard
//! (`guard_parent_index_write` → `worktree::parent_index_boundary_crossed`)
//! refuses such a write unless acknowledged via `--parent-index` or
//! `CQS_PARENT_INDEX_OK=1`.
//!
//! Fixture (mirrors `.claude/worktrees/<agent>/` under the cqs repo):
//!
//! ```text
//! <tmp>/workspace/            .git/ (dir)  Cargo.toml [workspace]  .cqs/
//! <tmp>/workspace/wt/         .git (file)  Cargo.toml (member)     (no .cqs/)
//! ```
//!
//! `cqs slot create` is the write under test — it only creates a slot
//! directory (no embedder, no network), so the test stays fast and runs
//! unconditionally (no `slow-tests` gate). The guard fires before any
//! command work, so the refuse path never touches an embedder either.

mod common;

use common::cqs_v1 as cqs;
use std::fs;
use tempfile::TempDir;

/// Build the parent-workspace + nested-worktree fixture. Returns the
/// tempdir (kept alive) and the worktree path to invoke from.
fn worktree_under_workspace() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let workspace = dir.path().join("workspace");
    // Parent: a real repo (`.git/` dir) that is also a Cargo workspace root,
    // with a pre-existing `.cqs/` index dir so root discovery lands on it.
    fs::create_dir_all(workspace.join(".git")).unwrap();
    fs::create_dir_all(workspace.join(".cqs")).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nmembers = [\"wt\"]\n",
    )
    .unwrap();

    // Nested worktree: a `.git` *file* (linked worktree) + a member
    // Cargo.toml, and crucially NO `.cqs/` of its own.
    let wt = workspace.join("wt");
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join(".git"), "gitdir: /abs/.git/worktrees/wt\n").unwrap();
    fs::write(
        wt.join("Cargo.toml"),
        "[package]\nname = \"wt\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();

    (dir, wt)
}

/// A WRITE command (`slot create`) invoked from the nested worktree must
/// be refused — its project-root discovery crossed up into the parent
/// workspace's index.
#[test]
fn write_from_worktree_refused_without_acknowledgment() {
    let (_dir, wt) = worktree_under_workspace();

    let assert = cqs()
        .args(["slot", "create", "guarded"])
        .current_dir(&wt)
        .env("CQS_NO_DAEMON", "1")
        // Ensure no ambient acknowledgment leaks in from the host env.
        .env_remove("CQS_PARENT_INDEX_OK")
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("parent index") || stderr.contains("worktree"),
        "refusal message must name the parent-index/worktree hazard; got: {stderr}"
    );

    // The write must NOT have happened: no slot under the parent's `.cqs/`.
    let parent_cqs = wt.parent().unwrap().join(".cqs");
    assert!(
        !parent_cqs.join("slots").join("guarded").exists(),
        "refused write must leave the parent index untouched"
    );
}

/// The same write proceeds once the `--parent-index` flag acknowledges
/// the boundary crossing — and lands in the parent's `.cqs/`.
#[test]
fn write_from_worktree_proceeds_with_flag() {
    let (_dir, wt) = worktree_under_workspace();

    cqs()
        .args(["slot", "create", "viaflag", "--parent-index"])
        .current_dir(&wt)
        .env("CQS_NO_DAEMON", "1")
        .env_remove("CQS_PARENT_INDEX_OK")
        .assert()
        .success();

    let parent_cqs = wt.parent().unwrap().join(".cqs");
    assert!(
        parent_cqs.join("slots").join("viaflag").exists(),
        "acknowledged write must create the slot under the parent's .cqs/"
    );
}

/// The env var `CQS_PARENT_INDEX_OK=1` is an equivalent acknowledgment.
#[test]
fn write_from_worktree_proceeds_with_env() {
    let (_dir, wt) = worktree_under_workspace();

    cqs()
        .args(["slot", "create", "viaenv"])
        .current_dir(&wt)
        .env("CQS_NO_DAEMON", "1")
        .env("CQS_PARENT_INDEX_OK", "1")
        .assert()
        .success();

    let parent_cqs = wt.parent().unwrap().join(".cqs");
    assert!(
        parent_cqs.join("slots").join("viaenv").exists(),
        "env-acknowledged write must create the slot under the parent's .cqs/"
    );
}

/// Reads are never gated: `slot list` from the worktree resolves to the
/// parent's `.cqs/` (the worktree→main read path) and succeeds with no
/// acknowledgment.
#[test]
fn read_from_worktree_unaffected() {
    let (_dir, wt) = worktree_under_workspace();

    cqs()
        .args(["slot", "list"])
        .current_dir(&wt)
        .env("CQS_NO_DAEMON", "1")
        .env_remove("CQS_PARENT_INDEX_OK")
        .assert()
        .success();
}

/// A WRITE from a regular (non-worktree) project is never gated: the
/// resolved root equals the invocation's own git root, so no boundary is
/// crossed. Guards against a false-positive guard that fires on any
/// non-trivial root resolution.
#[test]
fn write_from_regular_repo_not_gated() {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(repo.join(".cqs")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname=\"r\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();

    cqs()
        .args(["slot", "create", "plain"])
        .current_dir(&repo)
        .env("CQS_NO_DAEMON", "1")
        .env_remove("CQS_PARENT_INDEX_OK")
        .assert()
        .success();
    assert!(repo.join(".cqs").join("slots").join("plain").exists());
}
