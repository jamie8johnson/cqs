//! In-process integration tests for `cqs::health` + `cqs::suggest` + the
//! deps store APIs that the `cqs health/suggest/deps` CLI commands wrap.
//!
//! Replaces `tests/cli_health_test.rs`, which was gated behind the
//! `slow-tests` feature because it shelled out to the `cqs` binary and
//! cold-loaded the full model stack per invocation. These tests use
//! [`InProcessFixture`] with the deterministic `MockEmbedder` — same
//! corpus, same assertions on the underlying data, no subprocess, no
//! model load. Phase 2 PR-1 of the slow-tests elimination plan.
//!
//! Coverage notes:
//! - `health_cli_json` was an envelope-shape check; the underlying data is
//!   `HealthReport` and we assert on its fields directly. The JSON envelope
//!   is its own concern (handled by phase 3's `cli_surface_test.rs`).
//! - `health_cli_text` was a "the printer ran" smoke test; pure CLI surface
//!   with no data behind it, dropped here.
//! - `suggest_cli_json` had assertions on envelope `data.suggestions` /
//!   `data.count`; we assert on the `Vec<SuggestedNote>` directly.
//! - `deps_cli_json` (forward) and `deps_cli_reverse_json` map to
//!   `store.get_type_users` and `store.get_types_used_by` respectively.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;

use common::InProcessFixture;

/// Fixture corpus: matches what `setup_graph_project` produced in the
/// old subprocess test (kept identical so behaviour comparisons during
/// the conversion are direct).
fn graph_corpus() -> InProcessFixture {
    InProcessFixture::with_corpus(&[
        (
            "src/lib.rs",
            r#"
pub mod types;

/// Entry point
pub fn main() {
    let data = process(42);
    println!("{}", data);
}

/// Process input through validation and transformation
pub fn process(input: i32) -> String {
    let config = types::Config::default();
    let valid = validate(input, &config);
    if valid {
        transform(input)
    } else {
        String::from("invalid")
    }
}

/// Check if input is positive and within config bounds
fn validate(input: i32, config: &types::Config) -> bool {
    input > 0 && input <= config.max_value
}

/// Double and format the input
fn transform(input: i32) -> String {
    format!("result: {}", input * 2)
}
"#,
        ),
        (
            "src/types.rs",
            r#"
/// Configuration for processing
#[derive(Default)]
pub struct Config {
    pub max_value: i32,
}

impl Config {
    pub fn new(max: i32) -> Self {
        Config { max_value: max }
    }
}
"#,
        ),
        (
            "src/tests.rs",
            r#"
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process() {
        let result = process(5);
        assert_eq!(result, "result: 10");
    }
}
"#,
        ),
    ])
}

/// Helper: gather the on-disk file set for the staleness arm of
/// `health_check`. The `cqs health` CLI does this via
/// `enumerate_files(root, &parser, false)`; library callers do the
/// equivalent themselves.
fn current_files(f: &InProcessFixture) -> HashSet<PathBuf> {
    let exts = f.parser.supported_extensions();
    cqs::enumerate_files(&f.root, &exts, false)
        .expect("enumerate_files for health staleness check")
        .into_iter()
        .map(|rel| f.root.join(rel))
        .collect()
}

#[test]
fn health_check_returns_populated_report() {
    let f = graph_corpus();
    let files = current_files(&f);
    // cqs_dir = tempdir/.cqs; same convention as the CLI
    let cqs_dir = f.root.join(".cqs");

    let report =
        cqs::health::health_check(&f.store.store, &files, &cqs_dir, &f.root).expect("health_check");

    // Stats reflect the indexed corpus.
    assert!(
        report.stats.total_chunks > 0,
        "expected chunks indexed, got {}",
        report.stats.total_chunks
    );
    assert!(
        report.stats.total_files >= 3,
        "expected ≥3 files (lib.rs + types.rs + tests.rs), got {}",
        report.stats.total_files
    );
    assert!(
        !report.stats.model_name.is_empty(),
        "model_name should be set"
    );
    assert!(
        report.stats.schema_version > 0,
        "schema_version should be a positive integer"
    );

    // Hotspots field exists (may be empty on a tiny corpus, but the type
    // is `Vec` not `Option<Vec>`, so the array is always present — this
    // is the property the JSON envelope test was actually checking).
    let _: usize = report.hotspots.len();
    let _: usize = report.untested_hotspots.len();
}

#[test]
fn suggest_notes_returns_vec_of_suggestions() {
    let f = graph_corpus();
    // Library API: `Vec<SuggestedNote>`. The CLI wraps it in a JSON
    // envelope with `suggestions: [...]` + `count: N` — we assert on
    // the underlying type directly.
    let suggestions = cqs::suggest::suggest_notes(&f.store.store, &f.root).expect("suggest_notes");

    // The result might be empty for a 3-file fixture (no obvious dead
    // clusters / risk patterns). The contract is that the call returns
    // a `Vec` rather than failing — that's what we assert.
    let _ = suggestions.len();
}

#[test]
fn deps_forward_reports_type_users() {
    let f = graph_corpus();
    // `cqs deps Config` → who uses the Config type? Type-edge extraction
    // tracks signature-level uses (params, returns, fields), not
    // expression-level uses. So `validate(input: i32, config: &Config)`
    // shows up because of its parameter type, while `process` (which
    // only does `let cfg = Config::default()`) does not. The test
    // assertion mirrors what the data model actually emits.
    let users = f
        .store
        .get_type_users("Config", 100)
        .expect("get_type_users");

    let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
    assert!(
        names.contains(&"validate"),
        "expected `validate` (which takes &Config in its signature) \
         among Config users (saw {:?})",
        names
    );
}

#[test]
fn deps_reverse_reports_used_types() {
    let f = graph_corpus();
    // `cqs deps validate --reverse` → what types does `validate` use?
    // It takes `&types::Config` so `Config` should appear.
    let types = f
        .store
        .get_types_used_by("validate", 100)
        .expect("get_types_used_by");

    let type_names: Vec<&str> = types.iter().map(|t| t.type_name.as_str()).collect();
    assert!(
        type_names.contains(&"Config"),
        "expected `Config` among types used by `validate` (saw {:?})",
        type_names
    );
}
