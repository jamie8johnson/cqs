//! Tests for review_diff() (TC-1)
//!
//! Tests the comprehensive diff review pipeline: parse diff -> changed functions
//! -> impact analysis -> risk scoring -> review result.

mod common;

use common::{mock_embedding, TestStore};
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
use cqs::review::review_diff;
use cqs::RiskLevel;
use std::path::{Path, PathBuf};

/// Create a test chunk with custom file and line range
fn chunk_at(name: &str, file: &str, line_start: u32, line_end: u32) -> Chunk {
    let content = format!("fn {}() {{ }}", name);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content,
        doc: None,
        line_start,
        line_end,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
    }
}

/// Create a test chunk (name starts with "test_") to be recognized as a test
fn test_chunk_at(name: &str, file: &str, line_start: u32, line_end: u32) -> Chunk {
    let content = format!("#[test] fn {}() {{ }}", name);
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content,
        doc: None,
        line_start,
        line_end,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
    }
}

/// Insert chunks into the store
fn insert_chunks(store: &TestStore, chunks: &[Chunk]) {
    let emb = mock_embedding(1.0);
    let pairs: Vec<_> = chunks.iter().map(|c| (c.clone(), emb.clone())).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();
}

/// Insert function call graph entries
fn insert_calls(store: &TestStore, file: &str, calls: &[(&str, u32, &[(&str, u32)])]) {
    let fc: Vec<FunctionCalls> = calls
        .iter()
        .map(|(name, line, callees)| FunctionCalls {
            name: name.to_string(),
            line_start: *line,
            calls: callees
                .iter()
                .map(|(callee, cline)| CallSite {
                    callee_name: callee.to_string(),
                    line_number: *cline,
                })
                .collect(),
        })
        .collect();
    store.upsert_function_calls(Path::new(file), &fc).unwrap();
}

// ===== review_diff with synthetic diff + seeded store =====

#[test]
fn test_review_diff_with_changed_functions() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("compute", "src/math.rs", 10, 30),
        chunk_at("validate", "src/math.rs", 40, 60),
        chunk_at("process", "src/app.rs", 1, 20),
        test_chunk_at("test_compute", "tests/math_test.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    // process calls compute, test_compute calls compute
    insert_calls(&store, "src/app.rs", &[("process", 1, &[("compute", 10)])]);
    insert_calls(
        &store,
        "tests/math_test.rs",
        &[("test_compute", 1, &[("compute", 5)])],
    );

    let diff = "\
diff --git a/src/math.rs b/src/math.rs
--- a/src/math.rs
+++ b/src/math.rs
@@ -15,3 +15,4 @@ fn compute() {
     let x = 1;
+    let y = 2;
";

    let root = Path::new("/tmp");
    let result = review_diff(&store, diff, root).unwrap();

    // Should produce Some result (not None)
    let review = result.expect("review_diff should produce a result for a valid diff");

    // changed_functions should contain "compute"
    assert!(
        review.changed_functions.iter().any(|f| f.name == "compute"),
        "compute should be in changed_functions, got: {:?}",
        review
            .changed_functions
            .iter()
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );

    // affected_callers should contain "process" (it calls compute)
    assert!(
        review.affected_callers.iter().any(|c| c.name == "process"),
        "process should be an affected caller, got: {:?}",
        review
            .affected_callers
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );

    // affected_tests should contain test_compute
    assert!(
        review
            .affected_tests
            .iter()
            .any(|t| t.name == "test_compute"),
        "test_compute should be an affected test, got: {:?}",
        review
            .affected_tests
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );

    // risk_summary should exist and have valid counts
    let total_risk =
        review.risk_summary.high + review.risk_summary.medium + review.risk_summary.low;
    assert_eq!(
        total_risk,
        review.changed_functions.len(),
        "Risk summary counts should sum to changed_functions count"
    );

    // Each changed function should have a risk score
    for func in &review.changed_functions {
        // Just verify the risk field is accessible and has valid values
        assert!(func.risk.score >= 0.0, "Risk score should be non-negative");
        assert!(
            func.risk.coverage >= 0.0 && func.risk.coverage <= 1.0,
            "Coverage should be between 0 and 1"
        );
    }
}

// ===== Empty diff → None result =====

#[test]
fn test_review_diff_empty_diff() {
    let store = TestStore::new();

    let result = review_diff(&store, "", Path::new("/tmp")).unwrap();
    assert!(
        result.is_none(),
        "Empty diff should produce None (no hunks to review)"
    );
}

#[test]
fn test_review_diff_diff_with_no_hunks() {
    let store = TestStore::new();

    // A diff header with no actual hunks
    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
";

    let result = review_diff(&store, diff, Path::new("/tmp")).unwrap();
    assert!(result.is_none(), "Diff with no hunks should produce None");
}

// ===== Diff with no indexed functions → graceful None =====

#[test]
fn test_review_diff_no_indexed_functions() {
    let store = TestStore::new();
    // Store is empty — no chunks indexed

    let diff = "\
diff --git a/src/unknown.rs b/src/unknown.rs
--- a/src/unknown.rs
+++ b/src/unknown.rs
@@ -5,3 +5,4 @@ fn mystery() {
     let x = 1;
+    let y = 2;
";

    let result = review_diff(&store, diff, Path::new("/tmp")).unwrap();
    assert!(
        result.is_none(),
        "Diff touching non-indexed files should produce None (no changed functions found)"
    );
}

// ===== Multiple changed functions =====

#[test]
fn test_review_diff_multiple_changed_functions() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("fn_alpha", "src/lib.rs", 10, 20),
        chunk_at("fn_beta", "src/lib.rs", 30, 40),
        chunk_at("caller_of_both", "src/app.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/app.rs",
        &[("caller_of_both", 1, &[("fn_alpha", 5), ("fn_beta", 10)])],
    );

    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -15,3 +15,4 @@ fn fn_alpha() {
     let x = 1;
+    let y = 2;
@@ -35,3 +36,4 @@ fn fn_beta() {
     let a = 1;
+    let b = 2;
";

    let root = Path::new("/tmp");
    let result = review_diff(&store, diff, root).unwrap();
    let review = result.expect("Should produce a review result");

    assert_eq!(
        review.changed_functions.len(),
        2,
        "Should find both fn_alpha and fn_beta as changed"
    );

    let names: Vec<&str> = review
        .changed_functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert!(names.contains(&"fn_alpha"));
    assert!(names.contains(&"fn_beta"));

    // caller_of_both should be deduplicated (appears once even though it calls both)
    let caller_names: Vec<&str> = review
        .affected_callers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        caller_names.contains(&"caller_of_both"),
        "caller_of_both should be an affected caller"
    );
    assert_eq!(
        caller_names
            .iter()
            .filter(|&&n| n == "caller_of_both")
            .count(),
        1,
        "caller_of_both should appear only once (deduplicated)"
    );

    // risk_summary.overall should be defined
    assert!(
        matches!(
            review.risk_summary.overall,
            RiskLevel::Low | RiskLevel::Medium | RiskLevel::High
        ),
        "Overall risk level should be a valid variant"
    );
}

// ===== review_diff with actual notes matching changed files (TC-8) =====

#[test]
fn test_review_diff_with_relevant_notes() {
    let store = TestStore::new();

    // Set up chunks and call graph (same pattern as existing tests)
    let chunks = vec![
        chunk_at("compute", "src/math.rs", 10, 30),
        chunk_at("validate", "src/math.rs", 40, 60),
        test_chunk_at("test_compute", "tests/math_test.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "tests/math_test.rs",
        &[("test_compute", 1, &[("compute", 5)])],
    );

    // Insert a note that mentions "math.rs" — should be matched by review_diff
    let note = cqs::note::Note {
        id: "note:0".to_string(),
        text: "math.rs compute function has known precision issues".to_string(),
        sentiment: -0.5,
        mentions: vec!["math.rs".to_string()],
    };
    let note_embedding = mock_embedding(0.5);
    store
        .upsert_notes_batch(
            &[(note, note_embedding)],
            std::path::Path::new("notes.toml"),
            12345,
        )
        .unwrap();

    // Verify the note was stored
    let summaries = store.list_notes_summaries().unwrap();
    assert_eq!(summaries.len(), 1, "Should have 1 note stored");
    assert!(
        summaries[0].mentions.contains(&"math.rs".to_string()),
        "Note should mention math.rs"
    );

    // Run review_diff with a diff that touches src/math.rs
    let diff = "\
diff --git a/src/math.rs b/src/math.rs
--- a/src/math.rs
+++ b/src/math.rs
@@ -15,3 +15,4 @@ fn compute() {
     let x = 1;
+    let y = 2;
";

    let root = Path::new("/tmp");
    let result = review_diff(&store, diff, root).unwrap();
    let review = result.expect("review_diff should produce a result");

    // relevant_notes should contain our note (it mentions math.rs, which is changed)
    assert!(
        !review.relevant_notes.is_empty(),
        "relevant_notes should be non-empty when notes mention changed files"
    );

    assert!(
        review
            .relevant_notes
            .iter()
            .any(|n| n.text.contains("precision issues")),
        "Should find the note about precision issues, got: {:?}",
        review
            .relevant_notes
            .iter()
            .map(|n| &n.text)
            .collect::<Vec<_>>()
    );

    // Verify the matching_files field includes the changed file
    let matching_note = review
        .relevant_notes
        .iter()
        .find(|n| n.text.contains("precision issues"))
        .unwrap();
    assert!(
        matching_note
            .matching_files
            .iter()
            .any(|f| f.contains("math.rs")),
        "matching_files should include math.rs, got: {:?}",
        matching_note.matching_files
    );
}
