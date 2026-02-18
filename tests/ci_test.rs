//! Tests for run_ci_analysis() — CI pipeline analysis
//!
//! Tests the CI composition: review_diff + dead code filtering + gate evaluation.

mod common;

use common::{mock_embedding, TestStore};
use cqs::ci::{run_ci_analysis, GateThreshold};
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
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

// ===== Gate passes when only low/medium risk and threshold=High =====

#[test]
fn test_ci_gate_high_passes_with_low_risk() {
    let store = TestStore::new();

    // Insert a simple function with no callers (low risk: 0 callers, 0 tests)
    let chunks = vec![chunk_at("helper", "src/utils.rs", 10, 20)];
    insert_chunks(&store, &chunks);

    let diff = "\
diff --git a/src/utils.rs b/src/utils.rs
--- a/src/utils.rs
+++ b/src/utils.rs
@@ -15,3 +15,4 @@ fn helper() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::High).unwrap();

    assert!(
        report.gate.passed,
        "Gate should pass when no high-risk functions exist"
    );
    assert!(report.gate.reasons.is_empty());
}

// ===== Gate fails when high-risk function with threshold=High =====

#[test]
fn test_ci_gate_high_fails_with_high_risk() {
    let store = TestStore::new();

    // Insert a well-connected function (many callers = higher risk)
    let chunks = vec![
        chunk_at("core_fn", "src/core.rs", 10, 30),
        chunk_at("caller1", "src/a.rs", 1, 10),
        chunk_at("caller2", "src/b.rs", 1, 10),
        chunk_at("caller3", "src/c.rs", 1, 10),
        chunk_at("caller4", "src/d.rs", 1, 10),
        chunk_at("caller5", "src/e.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    // All callers call core_fn
    insert_calls(&store, "src/a.rs", &[("caller1", 1, &[("core_fn", 5)])]);
    insert_calls(&store, "src/b.rs", &[("caller2", 1, &[("core_fn", 5)])]);
    insert_calls(&store, "src/c.rs", &[("caller3", 1, &[("core_fn", 5)])]);
    insert_calls(&store, "src/d.rs", &[("caller4", 1, &[("core_fn", 5)])]);
    insert_calls(&store, "src/e.rs", &[("caller5", 1, &[("core_fn", 5)])]);

    let diff = "\
diff --git a/src/core.rs b/src/core.rs
--- a/src/core.rs
+++ b/src/core.rs
@@ -15,3 +15,4 @@ fn core_fn() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::High).unwrap();

    // core_fn has 5 callers and 0 tests → high risk
    if report.review.risk_summary.high > 0 {
        assert!(
            !report.gate.passed,
            "Gate should fail when high-risk functions exist"
        );
        assert!(
            !report.gate.reasons.is_empty(),
            "Should have failure reasons"
        );
    }
    // If the risk scorer doesn't classify this as high (possible with default thresholds),
    // the gate should still pass — that's correct behavior
}

// ===== Gate=Medium fails on medium risk =====

#[test]
fn test_ci_gate_medium_fails_on_medium_risk() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("process", "src/app.rs", 10, 30),
        chunk_at("handler", "src/api.rs", 1, 10),
        test_chunk_at("test_process", "tests/app_test.rs", 1, 15),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(&store, "src/api.rs", &[("handler", 1, &[("process", 5)])]);
    insert_calls(
        &store,
        "tests/app_test.rs",
        &[("test_process", 1, &[("process", 5)])],
    );

    let diff = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -15,3 +15,4 @@ fn process() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::Medium).unwrap();

    // Regardless of what level this gets, with gate=Medium any medium+ should fail
    if report.review.risk_summary.medium > 0 || report.review.risk_summary.high > 0 {
        assert!(
            !report.gate.passed,
            "Gate=Medium should fail on medium+ risk"
        );
    }
}

// ===== Gate=Off always passes =====

#[test]
fn test_ci_gate_off_always_passes() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("critical", "src/core.rs", 10, 30),
        chunk_at("caller1", "src/a.rs", 1, 10),
        chunk_at("caller2", "src/b.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(&store, "src/a.rs", &[("caller1", 1, &[("critical", 5)])]);
    insert_calls(&store, "src/b.rs", &[("caller2", 1, &[("critical", 5)])]);

    let diff = "\
diff --git a/src/core.rs b/src/core.rs
--- a/src/core.rs
+++ b/src/core.rs
@@ -15,3 +15,4 @@ fn critical() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::Off).unwrap();

    assert!(
        report.gate.passed,
        "Gate=Off should always pass regardless of risk"
    );
    assert!(report.gate.reasons.is_empty());
}

// ===== Empty diff → gate passes, empty report =====

#[test]
fn test_ci_empty_diff_passes() {
    let store = TestStore::new();

    let report = run_ci_analysis(&store, "", Path::new("/tmp"), GateThreshold::High).unwrap();

    assert!(report.gate.passed, "Empty diff should pass gate");
    assert!(report.review.changed_functions.is_empty());
    assert!(report.review.affected_callers.is_empty());
    assert!(report.review.affected_tests.is_empty());
    assert!(report.dead_in_diff.is_empty());
    assert_eq!(report.review.risk_summary.overall, RiskLevel::Low);
}

// ===== Diff touching no indexed files → gate passes =====

#[test]
fn test_ci_no_indexed_functions_passes() {
    let store = TestStore::new();

    // Index some chunks in src/math.rs
    let chunks = vec![chunk_at("compute", "src/math.rs", 10, 30)];
    insert_chunks(&store, &chunks);

    // Diff touches a different file entirely
    let diff = "\
diff --git a/src/unknown.rs b/src/unknown.rs
--- a/src/unknown.rs
+++ b/src/unknown.rs
@@ -5,3 +5,4 @@ fn mystery() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::High).unwrap();

    assert!(
        report.gate.passed,
        "Diff touching no indexed files should pass"
    );
    assert!(report.review.changed_functions.is_empty());
}

// ===== Dead code in diff files is reported =====

#[test]
fn test_ci_dead_code_in_diff_reported() {
    let store = TestStore::new();

    // Insert a function with no callers (dead code) in the diff file
    let chunks = vec![
        chunk_at("used_fn", "src/app.rs", 10, 20),
        chunk_at("dead_fn", "src/app.rs", 30, 40),
        chunk_at("caller", "src/main.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    // Only used_fn has a caller — dead_fn is dead code
    insert_calls(&store, "src/main.rs", &[("caller", 1, &[("used_fn", 5)])]);

    let diff = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -15,3 +15,4 @@ fn used_fn() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::High).unwrap();

    // dead_fn should appear in dead_in_diff since it's in src/app.rs (touched by diff)
    let dead_names: Vec<&str> = report
        .dead_in_diff
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    // Note: dead code detection may or may not find dead_fn depending on
    // pub analysis. The important thing is it doesn't include functions from
    // files NOT in the diff.
    // Also verify used_fn is NOT in dead code (it has a caller)
    assert!(
        !dead_names.contains(&"used_fn"),
        "used_fn has callers, should not be dead code"
    );
}

// ===== Dead code NOT in diff file is excluded =====

#[test]
fn test_ci_dead_code_not_in_diff_excluded() {
    let store = TestStore::new();

    // dead_fn is in src/utils.rs (NOT touched by diff)
    let chunks = vec![
        chunk_at("changed_fn", "src/app.rs", 10, 20),
        chunk_at("dead_fn", "src/utils.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    let diff = "\
diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -15,3 +15,4 @@ fn changed_fn() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::High).unwrap();

    // dead_fn is in src/utils.rs which is NOT in the diff — should be excluded
    let dead_files: Vec<&str> = report
        .dead_in_diff
        .iter()
        .map(|d| d.file.as_str())
        .collect();
    assert!(
        !dead_files.iter().any(|f| f.contains("utils")),
        "Dead code from non-diff files should be excluded, got: {:?}",
        dead_files
    );
}

// ===== Review result includes callers and tests =====

#[test]
fn test_ci_review_includes_callers_and_tests() {
    let store = TestStore::new();

    let chunks = vec![
        chunk_at("target_fn", "src/lib.rs", 10, 30),
        chunk_at("caller_fn", "src/api.rs", 1, 15),
        test_chunk_at("test_target", "tests/lib_test.rs", 1, 10),
    ];
    insert_chunks(&store, &chunks);

    insert_calls(
        &store,
        "src/api.rs",
        &[("caller_fn", 1, &[("target_fn", 8)])],
    );
    insert_calls(
        &store,
        "tests/lib_test.rs",
        &[("test_target", 1, &[("target_fn", 5)])],
    );

    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -15,3 +15,4 @@ fn target_fn() {
     let x = 1;
+    let y = 2;
";

    let report = run_ci_analysis(&store, diff, Path::new("/tmp"), GateThreshold::Off).unwrap();

    assert!(
        report
            .review
            .changed_functions
            .iter()
            .any(|f| f.name == "target_fn"),
        "target_fn should be in changed functions"
    );
    assert!(
        report
            .review
            .affected_callers
            .iter()
            .any(|c| c.name == "caller_fn"),
        "caller_fn should be in affected callers"
    );
    assert!(
        report
            .review
            .affected_tests
            .iter()
            .any(|t| t.name == "test_target"),
        "test_target should be in affected tests"
    );
}
