//! Asserts every `CQS_*` environment variable referenced in source code
//! appears in the README env var table.
//!
//! SHL-25 (`gh issue view 855`): the README drifts behind when new env vars
//! are introduced without a matching doc entry. This test catches the drift
//! at PR time. If it fails, add the new var to the env-var table in
//! `README.md` (or to the allowlist below if it's a prefix used for
//! `format!`-style keys).
//!
//! Run: `cargo test --test env_var_docs --release --features gpu-index`

use std::fs;
use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rs_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn extract_env_vars(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\bCQS_[A-Z][A-Z_]*[A-Z]\b").unwrap();
    let mut out: Vec<String> = re.find_iter(text).map(|m| m.as_str().to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// Returns `true` when `var` appears in `readme` as a complete token —
/// bordered by characters that are NOT part of an identifier. So a
/// short var name matches itself (and matches when wrapped in
/// backticks or followed by `=`), but is **NOT** satisfied by being a
/// prefix of a longer var name.
///
/// **Pre-fix bug:** `readme.contains(var)` did a substring match, so
/// a longer related var name in the README falsely satisfied a missing
/// short-name doc requirement. The `token_match_tests` module below
/// pins the fix.
///
/// Identifier characters here are ASCII letters, digits, and underscore
/// — matching the Rust env-var convention. The check is byte-level
/// (env var names are ASCII by spec) so we don't need regex compilation.
fn readme_documents(readme: &str, var: &str) -> bool {
    let bytes = readme.as_bytes();
    let needle = var.as_bytes();
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(rel) = readme[start..].find(var) {
        let abs = start + rel;
        let end = abs + var.len();
        let left_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let right_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Files under src/ whose text contains `var` — used by the REMOVED_VARS
/// inverted guard (a removed env knob must not regain production reads).
fn grep_src_for_var(workspace: &str, var: &str) -> Vec<String> {
    let mut files = Vec::new();
    collect_rs_files(&Path::new(workspace).join("src"), &mut files);
    files
        .iter()
        .filter(|f| {
            fs::read_to_string(f)
                .map(|t| t.contains(var))
                .unwrap_or(false)
        })
        .map(|f| f.display().to_string())
        .collect()
}

#[test]
fn all_cqs_env_vars_are_documented_in_readme() {
    let workspace = env!("CARGO_MANIFEST_DIR");
    let mut files = Vec::new();
    collect_rs_files(&Path::new(workspace).join("src"), &mut files);
    collect_rs_files(&Path::new(workspace).join("tests"), &mut files);

    let mut all_vars: Vec<String> = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).expect("readable source file");
        all_vars.extend(extract_env_vars(&text));
    }
    all_vars.sort();
    all_vars.dedup();

    // Per-category SPLADE alpha variants are covered by the generic
    // `CQS_SPLADE_ALPHA_{CATEGORY}` entry.
    const SPLADE_ALPHA_VARIANTS: &[&str] = &[
        "CQS_SPLADE_ALPHA_BEHAVIORAL",
        "CQS_SPLADE_ALPHA_CONCEPTUAL",
        "CQS_SPLADE_ALPHA_CONCEPTUAL_SEARCH",
        "CQS_SPLADE_ALPHA_CROSS_LANGUAGE",
        "CQS_SPLADE_ALPHA_IDENTIFIER_LOOKUP",
        "CQS_SPLADE_ALPHA_MULTI_STEP",
        "CQS_SPLADE_ALPHA_NEGATION",
        "CQS_SPLADE_ALPHA_STRUCTURAL",
        "CQS_SPLADE_ALPHA_TYPE_FILTERED",
        "CQS_SPLADE_ALPHA_UNKNOWN",
    ];

    // Deliberately-removed env vars that are still *referenced by name* in a
    // regression test asserting the removal stuck (the env var is read by
    // nothing in production, so it must NOT be documented as a live knob).
    // `CQS_ULTRASECURITY` was the posture knob removed; its inert-
    // knob test in `tests/cli_envelope_test.rs` sets it and asserts the wire
    // shape is unchanged.
    const REMOVED_VARS: &[&str] = &["CQS_ULTRASECURITY"];

    // Inverted guard: a removed var must stay removed. If any REMOVED_VARS
    // entry reappears in src/ (a production read), this test fails — the
    // allowlist exempts test-only references, never live knobs.
    for v in REMOVED_VARS {
        let hits = grep_src_for_var(workspace, v);
        assert!(
            hits.is_empty(),
            "{v} is in REMOVED_VARS but has production reads in src/: {hits:?} — \
             either document it in README (it's live) or remove the reads"
        );
    }

    let readme = fs::read_to_string(Path::new(workspace).join("README.md")).expect("README.md");

    let mut missing = Vec::new();
    for v in &all_vars {
        if SPLADE_ALPHA_VARIANTS.contains(&v.as_str()) {
            continue;
        }
        if REMOVED_VARS.contains(&v.as_str()) {
            continue;
        }
        // Skip test-only fixture env vars (used by `set_var` inside `#[test]`
        // bodies — they are local to a single test and have no production
        // call site to document). Convention: name them `CQS_TEST_*`.
        if v.starts_with("CQS_TEST_") {
            continue;
        }
        if !readme_documents(&readme, v) {
            missing.push(v.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "Undocumented CQS_* env vars — add each to the env-var table in README.md:\n  {}",
        missing.join("\n  ")
    );
}

#[cfg(test)]
mod token_match_tests {
    //! Pin the substring-match bug fix: `readme_documents` must require
    //! a complete token, not a substring. Test-fixture var names use
    //! the `CQS_TEST_*` convention so they pass the allowlist in the
    //! main test (otherwise the env-var regex would scan THIS file and
    //! the literals would surface as "missing from README").
    use super::readme_documents;

    #[test]
    fn matches_at_start_with_trailing_non_ident() {
        assert!(readme_documents("CQS_TEST_X is documented", "CQS_TEST_X"));
    }

    #[test]
    fn matches_in_middle_with_word_boundaries() {
        assert!(readme_documents(
            "see `CQS_TEST_X` in the table",
            "CQS_TEST_X"
        ));
    }

    #[test]
    fn matches_at_end() {
        assert!(readme_documents("the var is CQS_TEST_X", "CQS_TEST_X"));
    }

    #[test]
    fn rejects_substring_of_longer_var_at_start() {
        // The bug: looking up `CQS_TEST_X` should NOT be satisfied by a
        // README that only mentions `CQS_TEST_X_BAR`.
        assert!(!readme_documents(
            "see CQS_TEST_X_BAR in the table",
            "CQS_TEST_X"
        ));
    }

    #[test]
    fn rejects_substring_of_longer_var_at_end() {
        // Same trap, with the boundary on the other side.
        assert!(!readme_documents("MY_CQS_TEST_X env var", "CQS_TEST_X"));
    }

    #[test]
    fn rejects_substring_with_digit_continuation() {
        // Digits also continue identifiers in env-var convention.
        assert!(!readme_documents(
            "CQS_TEST_X2 is the new one",
            "CQS_TEST_X"
        ));
    }

    #[test]
    fn matches_with_equals_sign_boundary() {
        // `CQS_TEST_X=value` in code blocks.
        assert!(readme_documents("set CQS_TEST_X=1 to enable", "CQS_TEST_X"));
    }

    #[test]
    fn longer_var_still_matches_itself() {
        // Pin that the fix doesn't reject the var-as-itself case.
        assert!(readme_documents(
            "`CQS_TEST_X_BAR` is documented",
            "CQS_TEST_X_BAR"
        ));
    }
}
