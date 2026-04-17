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

    let readme = fs::read_to_string(Path::new(workspace).join("README.md")).expect("README.md");

    let mut missing = Vec::new();
    for v in &all_vars {
        if SPLADE_ALPHA_VARIANTS.contains(&v.as_str()) {
            continue;
        }
        // Skip test-only fixture env vars (used by `set_var` inside `#[test]`
        // bodies — they are local to a single test and have no production
        // call site to document). Convention: name them `CQS_TEST_*`.
        if v.starts_with("CQS_TEST_") {
            continue;
        }
        if !readme.contains(v) {
            missing.push(v.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "Undocumented CQS_* env vars — add each to the env-var table in README.md:\n  {}",
        missing.join("\n  ")
    );
}
