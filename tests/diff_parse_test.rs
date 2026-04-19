//! Audit P2 #49 + #50 — `parse_unified_diff` boundary cases.
//!
//! These tests pin behavior the existing inline tests in `src/diff_parse.rs`
//! don't cover:
//!
//! - **#49**: u32 overflow on huge hunk start/count silently defaults to 1.
//!   The current implementation parses `\d+` with no upper bound, then
//!   `.parse::<u32>()` returns `Err(_)` on > u32::MAX, which is swallowed
//!   by a `match` that emits `tracing::warn!` and falls back to 1. This
//!   misroutes downstream impact-diff to lines 1..6 of every file. We pin
//!   the *current* behavior (defaults to 1) so the audit-recommended fix
//!   (skip the hunk on overflow) is a deliberate change, not a regression.
//!
//! - **#50**: non-`b/`-prefixed `+++` paths fall through to the raw-path
//!   branch at `diff_parse.rs:55-60`. All existing `test_parse_unified_diff_*`
//!   tests use `+++ b/<path>` exclusively. A regression that removed the
//!   fallback would silently drop all hunks from non-git diff tools.
//!
//! Pure parser tests — no IO, no model, no daemon — so this file is NOT
//! gated behind `slow-tests`.

use cqs::{parse_unified_diff, DiffHunk};
use std::path::Path;

// ============================================================================
// P2 #49 — u32 overflow on huge hunk start/count
// ============================================================================

/// `@@ -1 +5000000000,3 @@` — start exceeds u32::MAX (4_294_967_295).
///
/// AUDIT-FOLLOWUP (P2 #49): the implementation at `src/diff_parse.rs:73-82`
/// catches `start.parse::<u32>()` failure with a `tracing::warn!` and falls
/// back to `start = 1`. Per the audit recommendation we should drop the
/// fallback and skip the hunk entirely (return early). Until that fix lands,
/// this test pins the current behavior so any change is intentional.
#[test]
fn test_parse_unified_diff_overflow_u32_start() {
    let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +5000000000,3 @@
+x
";
    let hunks = parse_unified_diff(diff);
    // AUDIT-FOLLOWUP (P2 #49): once the fix lands the assertion below should be
    // `assert!(hunks.is_empty())`. Today the parser silently defaults start
    // to 1, which misroutes impact-diff to the wrong line range.
    assert_eq!(
        hunks.len(),
        1,
        "current behavior: overflow → 1 fallback → 1 hunk emitted"
    );
    assert_eq!(
        hunks[0].start, 1,
        "AUDIT-FOLLOWUP (P2 #49): overflowing u32 start silently defaults to 1"
    );
    assert_eq!(
        hunks[0].count, 3,
        "count parses normally even when start overflowed"
    );
}

/// `@@ -1 +1,5000000000 @@` — count exceeds u32::MAX.
///
/// AUDIT-FOLLOWUP (P2 #49): same root cause as the start overflow above —
/// `count.parse::<u32>()` silently maps overflow to 1.
#[test]
fn test_parse_unified_diff_overflow_u32_count() {
    let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +1,5000000000 @@
+x
";
    let hunks = parse_unified_diff(diff);
    // AUDIT-FOLLOWUP (P2 #49): once the fix lands this should be
    // `assert!(hunks.is_empty())`. Today count silently defaults to 1.
    assert_eq!(
        hunks.len(),
        1,
        "current behavior: overflow → 1 fallback → 1 hunk emitted"
    );
    assert_eq!(hunks[0].start, 1, "start parses normally");
    assert_eq!(
        hunks[0].count, 1,
        "AUDIT-FOLLOWUP (P2 #49): overflowing u32 count silently defaults to 1"
    );
}

/// Boundary: `@@ -1 +4294967295,1 @@` — start is exactly u32::MAX, parses cleanly.
/// Pinned as a regression guard around the overflow boundary so a fix that
/// changes `parse::<u32>` to e.g. `parse::<i32>` doesn't silently shift the
/// representable range.
#[test]
fn test_parse_unified_diff_u32_max_start_parses() {
    let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +4294967295,1 @@
+x
";
    let hunks = parse_unified_diff(diff);
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].start, u32::MAX);
    assert_eq!(hunks[0].count, 1);
}

// ============================================================================
// P2 #50 — non-`b/`-prefixed `+++` paths
// ============================================================================

/// Non-`git` diff tools (plain `diff -u`, BSD diff, some patch generators)
/// emit `+++ <path>` without the `b/` prefix. The fallback at
/// `src/diff_parse.rs:55-60` stores the raw path as-is. Asserts the fallback
/// produces a usable `DiffHunk.file`.
#[test]
fn test_parse_unified_diff_no_b_prefix_falls_back_to_raw_path() {
    let diff = "+++ src/main.rs\n@@ -1 +1,3 @@\n+x\n";
    let hunks = parse_unified_diff(diff);
    assert_eq!(
        hunks.len(),
        1,
        "non-`b/` path should still emit a hunk via the raw-path fallback"
    );
    assert_eq!(
        hunks[0].file,
        Path::new("src/main.rs"),
        "raw path should be stored verbatim"
    );
    assert_eq!(hunks[0].start, 1);
    assert_eq!(hunks[0].count, 3);
}

/// Same fallback with a leading `./` and no `b/` — confirms the fallback
/// doesn't try to be clever about path normalization at parse time.
#[test]
fn test_parse_unified_diff_no_b_prefix_relative_dot_path() {
    let diff = "+++ ./src/main.rs\n@@ -1 +5,2 @@\n+x\n";
    let hunks = parse_unified_diff(diff);
    assert_eq!(hunks.len(), 1);
    assert_eq!(
        hunks[0].file,
        Path::new("./src/main.rs"),
        "raw path stored as-is, including the './' prefix"
    );
}

/// Pin sanitization behavior on a malformed path that may flow through to
/// downstream filesystem operations (`impact_diff::cmd_impact_diff` etc.).
///
/// AUDIT-FOLLOWUP (P2 #50): `parse_unified_diff` performs no path
/// canonicalization or validation today — `..` traversal sequences and
/// backslashes survive verbatim. The audit recommends pinning the chosen
/// behavior so a future cleanup is deliberate.
#[test]
fn test_parse_unified_diff_rejects_malformed_path() {
    let diff = "+++ \t..\\..\\etc\\passwd\n@@ -1 +1 @@\n+x\n";
    let hunks = parse_unified_diff(diff);
    // Current behavior: path stored verbatim, no canonicalization, no rejection.
    // AUDIT-FOLLOWUP (P2 #50): once a sanitization layer lands, this assertion
    // should change to either (a) `hunks.is_empty()` on rejection, or
    // (b) the file path being normalized.
    assert_eq!(
        hunks.len(),
        1,
        "AUDIT-FOLLOWUP (P2 #50): malformed paths are accepted verbatim today"
    );
    assert_eq!(
        hunks[0].file,
        Path::new("\t..\\..\\etc\\passwd"),
        "AUDIT-FOLLOWUP (P2 #50): path stored verbatim including leading TAB and backslashes"
    );
}

/// Mixed input: one `b/`-prefixed path and one raw path in the same diff —
/// each branch should be exercised independently.
#[test]
fn test_parse_unified_diff_mixed_b_prefix_and_raw() {
    let diff = "\
diff --git a/src/a.rs b/src/a.rs
+++ b/src/a.rs
@@ -1 +1,2 @@
+x
+++ src/b.rs
@@ -1 +5,3 @@
+y
";
    let hunks = parse_unified_diff(diff);
    assert_eq!(hunks.len(), 2, "both branches should produce a hunk");
    let by_file: Vec<&DiffHunk> = hunks.iter().collect();
    assert_eq!(by_file[0].file, Path::new("src/a.rs"));
    assert_eq!(by_file[0].start, 1);
    assert_eq!(by_file[1].file, Path::new("src/b.rs"));
    assert_eq!(by_file[1].start, 5);
}
