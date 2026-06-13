//! Worktree overlay — delta-discovery integration tests over a real-git
//! worktree fixture (PR-1, plan
//! `docs/plans/2026-06-12-worktree-overlay-implementation.md` §11).
//!
//! These exercise `cqs::worktree_overlay::discover_delta` + `fingerprint`
//! against a real `git init` + `git worktree add` layout. They are pure
//! git + FS — **no embedder** — so they run unconditionally (not gated
//! behind `slow-tests`). The embedder-dependent `build_overlay` path is
//! tested in-crate under `slow-tests`.

mod common;

use common::{git_in, worktree_fixture};
use cqs::worktree::overlay_root;
use cqs::worktree_overlay::{discover_delta, fingerprint, DeltaStatus};
use std::path::PathBuf;

/// Helper: does `masked_origins` contain a repo-relative path?
fn masks(delta: &cqs::worktree_overlay::Delta, rel: &str) -> bool {
    delta.masked_origins.contains(&PathBuf::from(rel))
}

/// Helper: is a repo-relative path in the parse set?
fn parses(delta: &cqs::worktree_overlay::Delta, rel: &str) -> bool {
    delta.parse_set.iter().any(|p| p == &PathBuf::from(rel))
}

#[test]
fn clean_worktree_has_empty_delta() {
    let (_tmp, parent, wt) = worktree_fixture();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(
        delta.masked_origins.is_empty(),
        "a freshly-added worktree at the same commit has no delta, got {:?}",
        delta.masked_origins
    );
    assert!(delta.parse_set.is_empty());
}

#[test]
fn modified_file_masks_and_parses() {
    let (_tmp, parent, wt) = worktree_fixture();
    // Uncommitted edit in the worktree.
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 100 }\npub fn beta() -> i32 { 2 }\n",
    )
    .unwrap();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(masks(&delta, "src/lib.rs"), "modified file masked");
    assert!(parses(&delta, "src/lib.rs"), "modified file parsed");
    // The untouched file is neither masked nor parsed.
    assert!(!masks(&delta, "src/util.rs"));
}

#[test]
fn deleted_file_masks_only() {
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::remove_file(wt.join("src/util.rs")).unwrap();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(masks(&delta, "src/util.rs"), "deleted file masked");
    assert!(
        !parses(&delta, "src/util.rs"),
        "deleted file is NOT in the parse set (no content to index)"
    );
    let rec = delta
        .records
        .iter()
        .find(|r| r.new == "src/util.rs")
        .expect("record present");
    assert_eq!(rec.status, DeltaStatus::Deleted);
}

#[test]
fn committed_change_in_worktree_is_in_delta() {
    // Correction #2: the diff base is the PARENT's HEAD, so a change the
    // lane COMMITTED (not just uncommitted) is still in the delta.
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 7 }\npub fn beta() -> i32 { 2 }\n",
    )
    .unwrap();
    git_in(&wt, &["add", "src/lib.rs"]);
    git_in(&wt, &["commit", "-q", "-m", "lane edit"]);
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(
        masks(&delta, "src/lib.rs"),
        "a committed lane edit is in the delta (parent-HEAD diff base)"
    );
    assert!(parses(&delta, "src/lib.rs"));
}

#[test]
fn renamed_file_masks_old_and_new() {
    let (_tmp, parent, wt) = worktree_fixture();
    // git-tracked rename (commit it so --find-renames sees a clean R).
    git_in(&wt, &["mv", "src/util.rs", "src/renamed.rs"]);
    git_in(&wt, &["commit", "-q", "-m", "rename util"]);
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(masks(&delta, "src/util.rs"), "old rename path masked");
    assert!(masks(&delta, "src/renamed.rs"), "new rename path masked");
    assert!(
        !parses(&delta, "src/util.rs"),
        "old rename path NOT parsed (gone)"
    );
    assert!(parses(&delta, "src/renamed.rs"), "new rename path parsed");
}

#[test]
fn untracked_file_masks_and_parses() {
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::write(wt.join("src/fresh.rs"), "pub fn fresh() -> i32 { 9 }\n").unwrap();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(masks(&delta, "src/fresh.rs"), "untracked file masked");
    assert!(parses(&delta, "src/fresh.rs"), "untracked file parsed");
    let rec = delta
        .records
        .iter()
        .find(|r| r.new == "src/fresh.rs")
        .expect("record present");
    assert_eq!(rec.status, DeltaStatus::Untracked);
}

#[test]
fn gitignored_file_absent_from_delta() {
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::write(wt.join(".gitignore"), "ignored.rs\n").unwrap();
    std::fs::write(wt.join("ignored.rs"), "pub fn nope() {}\n").unwrap();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(
        !masks(&delta, "ignored.rs"),
        "a .gitignore'd file is excluded from the untracked delta"
    );
}

#[test]
fn unsupported_extension_masks_but_does_not_parse() {
    let (_tmp, parent, wt) = worktree_fixture();
    // A modified tracked file with no supported extension: still masked
    // (origin-level), but excluded from the parse set.
    std::fs::write(wt.join("README.unknownext"), "data\n").unwrap();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(
        masks(&delta, "README.unknownext"),
        "unsupported-extension origin still masked"
    );
    assert!(
        !parses(&delta, "README.unknownext"),
        "unsupported-extension origin not parsed"
    );
}

#[cfg(unix)]
#[test]
fn symlink_masks_but_does_not_parse() {
    let (_tmp, parent, wt) = worktree_fixture();
    // An untracked symlink with a supported-looking extension: masked but
    // filtered from the parse set (symlink_metadata skips it).
    std::os::unix::fs::symlink("src/lib.rs", wt.join("link.rs")).unwrap();
    let delta = discover_delta(&wt, &parent).expect("discover");
    assert!(masks(&delta, "link.rs"), "symlink origin masked");
    assert!(
        !parses(&delta, "link.rs"),
        "symlink filtered from parse set"
    );
}

#[test]
fn fingerprint_stable_across_repeated_discovery() {
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::write(
        wt.join("src/lib.rs"),
        "pub fn alpha() -> i32 { 5 }\npub fn beta() -> i32 { 2 }\n",
    )
    .unwrap();
    let d1 = discover_delta(&wt, &parent).expect("discover 1");
    let fp1 = fingerprint(&wt, &d1);
    let d2 = discover_delta(&wt, &parent).expect("discover 2");
    let fp2 = fingerprint(&wt, &d2);
    assert_eq!(fp1, fp2, "fingerprint stable when nothing changed");
}

#[test]
fn fingerprint_changes_on_edit_and_reverts() {
    let (_tmp, parent, wt) = worktree_fixture();
    let lib = wt.join("src/lib.rs");
    let original = std::fs::read_to_string(&lib).unwrap();

    // Clean baseline.
    let d_clean = discover_delta(&wt, &parent).expect("discover clean");
    let fp_clean = fingerprint(&wt, &d_clean);

    // Edit → fingerprint moves.
    std::fs::write(
        &lib,
        "pub fn alpha() -> i32 { 99 }\npub fn beta() -> i32 { 2 }\n",
    )
    .unwrap();
    let d_edit = discover_delta(&wt, &parent).expect("discover edit");
    let fp_edit = fingerprint(&wt, &d_edit);
    assert_ne!(fp_clean, fp_edit, "edit must move the fingerprint");

    // Revert to byte-identical original → back to the clean fingerprint
    // (working tree == parent HEAD → file falls out of the delta).
    std::fs::write(&lib, original).unwrap();
    let d_revert = discover_delta(&wt, &parent).expect("discover revert");
    let fp_revert = fingerprint(&wt, &d_revert);
    assert!(
        d_revert.masked_origins.is_empty(),
        "reverted file is no longer in the delta"
    );
    assert_eq!(
        fp_clean, fp_revert,
        "reverting to identical content restores the clean fingerprint"
    );
}

#[test]
fn fingerprint_content_sensitive_for_same_record_set() {
    // Two different edits to the SAME file produce the same record metadata
    // (one Modified record) but different content → different fingerprints.
    let (_tmp, parent, wt) = worktree_fixture();
    let lib = wt.join("src/lib.rs");

    std::fs::write(
        &lib,
        "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 20 }\n",
    )
    .unwrap();
    let d1 = discover_delta(&wt, &parent).expect("discover 1");
    let fp1 = fingerprint(&wt, &d1);

    std::fs::write(
        &lib,
        "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 30 }\n",
    )
    .unwrap();
    let d2 = discover_delta(&wt, &parent).expect("discover 2");
    let fp2 = fingerprint(&wt, &d2);

    assert_ne!(
        fp1, fp2,
        "same record set, different content → different fingerprint"
    );
}

// ===== overlay_root predicate: out-of-tree worktree (real git) =====
//
// `worktree_fixture()` builds an OUT-OF-TREE worktree (`tmp/wt` sibling to
// `tmp/parent`), which the nested-only `parent_index_boundary_crossed`
// predicate cannot detect. These pin that `overlay_root` resolves it via the
// `.git`-link `lookup_main_cqs_dir` half — from the worktree root AND from a
// subdirectory (the F2 regression: the half must walk up to the `.git`-
// bearing root, not read `cwd/.git` directly).
//
// `lookup_main_cqs_dir` only returns `WorktreeUseMain` when the MAIN project
// has a `.cqs/` and the worktree does not, so the fixture's parent gets one.

#[test]
fn overlay_root_some_for_out_of_tree_worktree_root() {
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::create_dir_all(parent.join(".cqs")).unwrap();

    // The resolver redirected the worktree's reads to the parent's index, so
    // `resolved_root` is the parent. Canonicalize for a byte-exact compare
    // against the predicate's canonicalized worktree root.
    let got = overlay_root(&wt, &parent);
    let want = dunce::canonicalize(&wt).unwrap_or(wt);
    assert_eq!(
        got,
        Some(want),
        "out-of-tree worktree root is overlay-eligible"
    );
}

#[test]
fn overlay_root_some_from_out_of_tree_subdirectory() {
    let (_tmp, parent, wt) = worktree_fixture();
    std::fs::create_dir_all(parent.join(".cqs")).unwrap();

    // cwd is a subdirectory of the worktree (`wt/src/`), not the root.
    // `lookup_main_cqs_dir` reads `<dir>/.git`, which only exists at the
    // worktree root — so the predicate must walk up first (the F2 fix).
    let subdir = wt.join("src");
    let got = overlay_root(&subdir, &parent);
    let want = dunce::canonicalize(&wt).unwrap_or(wt);
    assert_eq!(
        got,
        Some(want),
        "a subdirectory inside an out-of-tree worktree is still overlay-eligible"
    );
}
