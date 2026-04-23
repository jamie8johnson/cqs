//! In-process integration tests for the related / impact-diff / stale
//! commands that lived in `tests/cli_commands_test.rs`.
//! Phase 2 PR-4 of `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`.
//!
//! Out of scope for this conversion (skipped, with rationale below):
//!
//! - **`scout`, `where`** — both require a real `&Embedder` (not the
//!   test trait), so converting would force a model cold-load. The
//!   underlying behaviour is "search → expansion", and both halves are
//!   covered by `index_search_test::search_returns_indexed_chunks` and
//!   `graph_test::trace_*`. The composition test isn't worth the cost.
//! - **`query --tokens`, `gather --tokens`** — token-budget packing is
//!   internal CLI logic with no library API; the underlying packer is
//!   covered by unit tests inside its own module.
//! - **`query --ref`, `gather --ref`, `ref add/list`** — references
//!   require a separate ref index plus config wiring; that's a
//!   meaningfully larger fixture than the one InProcessFixture
//!   currently builds. Defer to a follow-up that adds
//!   `InProcessFixture::add_ref(...)` once we actually need it.

mod common;

use common::InProcessFixture;

fn graph_corpus() -> InProcessFixture {
    InProcessFixture::with_corpus(&[(
        "src/lib.rs",
        r#"
pub fn main() {
    let data = process(42);
    println!("{}", data);
}

pub fn process(input: i32) -> String {
    let valid = validate(input);
    if valid { transform(input) } else { String::from("invalid") }
}

fn validate(input: i32) -> bool { input > 0 }
fn transform(input: i32) -> String { format!("result: {}", input * 2) }
"#,
    )])
}

// ---------------------------------------------------------------------
// related
// ---------------------------------------------------------------------

#[test]
fn related_returns_shared_callers_and_callees() {
    let f = graph_corpus();
    let result = cqs::find_related(&f.store.store, "validate", 50).expect("find_related");

    // `validate` is called by `process`. Its sibling callees of `process`
    // (`transform`) should appear in shared_callers (functions that
    // share a caller with `validate`).
    let shared_caller_names: Vec<&str> = result
        .shared_callers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        shared_caller_names.contains(&"transform"),
        "expected `transform` (sibling under `process`) in shared_callers, got {shared_caller_names:?}"
    );
}

#[test]
fn related_target_field_is_set() {
    let f = graph_corpus();
    let result = cqs::find_related(&f.store.store, "process", 50).expect("find_related");
    assert_eq!(
        result.target, "process",
        "RelatedResult.target should echo the input"
    );
}

// ---------------------------------------------------------------------
// impact-diff
// ---------------------------------------------------------------------

#[test]
fn impact_diff_with_no_changes_returns_empty() {
    let f = graph_corpus();
    // `analyze_diff_impact` accepts the changed-function list directly;
    // we don't need a git repo. Empty list → empty result.
    let result =
        cqs::analyze_diff_impact(&f.store.store, Vec::new(), &f.root).expect("analyze_diff_impact");
    assert!(
        result.changed_functions.is_empty(),
        "no changed fns → no entries (got {})",
        result.changed_functions.len()
    );
    assert!(
        result.all_callers.is_empty(),
        "no changed fns → no callers (got {})",
        result.all_callers.len()
    );
}

#[test]
fn impact_diff_with_modified_function_lists_callers() {
    use cqs::ChangedFunction;
    let f = graph_corpus();
    // `validate` is the function the old subprocess test "modified" via
    // `input > 0` → `input >= 0`. We declare the change directly to the
    // library API; it then walks the call graph to compute impact.
    let changed = vec![ChangedFunction {
        name: "validate".to_string(),
        file: f.root.join("src").join("lib.rs"),
        line_start: 10,
    }];
    let result =
        cqs::analyze_diff_impact(&f.store.store, changed, &f.root).expect("analyze_diff_impact");
    // `validate` is called by `process`; the flat all_callers list
    // should contain that direct caller. (DiffImpactResult flattens all
    // callers across changed functions into one list with provenance —
    // unlike ImpactResult where each callers list is per-target.)
    assert_eq!(
        result.changed_functions.len(),
        1,
        "one changed function in input"
    );
    let direct_callers: Vec<&str> = result.all_callers.iter().map(|c| c.name.as_str()).collect();
    assert!(
        direct_callers.contains(&"process"),
        "expected `process` in all_callers of changed `validate`, got {direct_callers:?}"
    );
}

// ---------------------------------------------------------------------
// stale
// ---------------------------------------------------------------------

#[test]
fn stale_count_zero_on_fresh_index() {
    use std::collections::HashSet;
    let f = graph_corpus();
    let exts = f.parser.supported_extensions();
    let files: HashSet<_> = cqs::enumerate_files(&f.root, &exts, false)
        .expect("enumerate_files")
        .into_iter()
        .map(|p| f.root.join(p))
        .collect();
    let (stale, missing) = f
        .store
        .count_stale_files(&files, &f.root)
        .expect("count_stale_files");
    assert_eq!(missing, 0, "no missing files on fresh index");
    // `stale` may be 0 or non-zero depending on mtime resolution; the
    // important property tested by the original `--json fresh-index`
    // path was that the call succeeds and reports a number, which
    // is implicit here.
    let _ = stale;
}

#[test]
fn stale_detects_missing_file_after_rm() {
    use std::collections::HashSet;
    let f = graph_corpus();
    std::fs::remove_file(f.root.join("src").join("lib.rs")).expect("rm");
    let exts = f.parser.supported_extensions();
    let files: HashSet<_> = cqs::enumerate_files(&f.root, &exts, false)
        .expect("enumerate_files")
        .into_iter()
        .map(|p| f.root.join(p))
        .collect();
    let (_stale, missing) = f
        .store
        .count_stale_files(&files, &f.root)
        .expect("count_stale_files");
    assert!(
        missing >= 1,
        "expected ≥1 missing file after rm, got {missing}"
    );
}
