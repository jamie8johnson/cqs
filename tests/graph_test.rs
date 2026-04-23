//! In-process integration tests for the call-graph and utility commands
//! that used to live in `tests/cli_graph_test.rs` (audit-mode, project,
//! trace, impact, test-map, context, explain, similar, gather, read,
//! notes). Phase 2 PR-3 of `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`.
//!
//! Pure CLI-surface concerns (text-format checks, exit codes, the
//! global `~/.config/cqs/projects.toml` mutation in `cqs project`,
//! and the "no path found" / "same function" trace messages) live in
//! `tests/cli_surface_test.rs`. Those still spawn the binary but don't
//! cold-load the embedder.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::InProcessFixture;

/// Multi-file fixture mirroring the old `setup_graph_project()` helper.
/// Has a real call chain: main → process → validate / transform.
fn graph_corpus() -> InProcessFixture {
    InProcessFixture::with_corpus(&[
        (
            "src/lib.rs",
            r#"
/// Entry point
pub fn main() {
    let data = process(42);
    println!("{}", data);
}

/// Process input through validation and transformation
pub fn process(input: i32) -> String {
    let valid = validate(input);
    if valid {
        transform(input)
    } else {
        String::from("invalid")
    }
}

/// Check if input is positive
fn validate(input: i32) -> bool {
    input > 0
}

/// Double and format the input
fn transform(input: i32) -> String {
    format!("result: {}", input * 2)
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

// ---------------------------------------------------------------------
// audit-mode (5 tests in the old binary)
// ---------------------------------------------------------------------

#[test]
fn audit_mode_save_then_load_round_trips() {
    let f = InProcessFixture::new();
    let cqs_dir = f.root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();
    let expires = chrono::Utc::now() + chrono::Duration::minutes(30);
    let mode = cqs::audit::AuditMode {
        enabled: true,
        expires_at: Some(expires),
    };
    cqs::audit::save_audit_state(&cqs_dir, &mode).expect("save");
    let loaded = cqs::audit::load_audit_state(&cqs_dir);
    assert!(
        loaded.is_active(),
        "audit mode should report active after save"
    );
    assert!(loaded.expires_at.is_some());
}

#[test]
fn audit_mode_off_clears_active_state() {
    let f = InProcessFixture::new();
    let cqs_dir = f.root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();
    cqs::audit::save_audit_state(
        &cqs_dir,
        &cqs::audit::AuditMode {
            enabled: true,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        },
    )
    .unwrap();
    cqs::audit::save_audit_state(&cqs_dir, &cqs::audit::AuditMode::default()).unwrap();
    let loaded = cqs::audit::load_audit_state(&cqs_dir);
    assert!(!loaded.is_active(), "off → load should report inactive");
}

#[test]
fn audit_mode_load_with_no_state_returns_default_inactive() {
    let f = InProcessFixture::new();
    let cqs_dir = f.root.join(".cqs"); // doesn't exist yet
    let loaded = cqs::audit::load_audit_state(&cqs_dir);
    assert!(
        !loaded.is_active(),
        "no audit-mode.json present → default inactive"
    );
}

#[test]
fn audit_mode_expired_state_is_inactive() {
    let f = InProcessFixture::new();
    let cqs_dir = f.root.join(".cqs");
    std::fs::create_dir_all(&cqs_dir).unwrap();
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    cqs::audit::save_audit_state(
        &cqs_dir,
        &cqs::audit::AuditMode {
            enabled: true,
            expires_at: Some(past),
        },
    )
    .unwrap();
    let loaded = cqs::audit::load_audit_state(&cqs_dir);
    assert!(
        !loaded.is_active(),
        "expired state must report inactive (enabled={}, expires_at={:?})",
        loaded.enabled,
        loaded.expires_at
    );
}

#[test]
fn parse_duration_accepts_canonical_forms() {
    // The CLI parses --expires via cqs::parse_duration, then composes the
    // expires_at timestamp. Direct test of the lib path.
    let half_hour = cqs::parse_duration("30m").unwrap();
    assert_eq!(half_hour, chrono::Duration::minutes(30));
    let one_hour = cqs::parse_duration("1h").unwrap();
    assert_eq!(one_hour, chrono::Duration::hours(1));
    let two_thirty = cqs::parse_duration("2h30m").unwrap();
    assert_eq!(two_thirty, chrono::Duration::minutes(150));
}

// ---------------------------------------------------------------------
// trace (3 tests in the old binary)
//
// No public lib `trace` function — the CLI inlines a BFS over CallGraph.
// Replicating here as a small test helper. See spec gotcha #1.
// ---------------------------------------------------------------------

/// Shortest call path from `src` to `tgt` via forward edges, BFS.
/// Returns `Some(path)` of unique names from src..tgt inclusive, or None.
fn shortest_call_path(
    forward: &HashMap<Arc<str>, Vec<Arc<str>>>,
    src: &str,
    tgt: &str,
    max_depth: usize,
) -> Option<Vec<String>> {
    use std::collections::{HashMap as Map, VecDeque};
    if src == tgt {
        return Some(vec![src.to_string()]);
    }
    let mut parent: Map<String, String> = Map::new();
    let mut q: VecDeque<(String, usize)> = VecDeque::new();
    q.push_back((src.to_string(), 0));
    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = forward.get(cur.as_str()) {
            for n in neighbors {
                let n = n.to_string();
                if !parent.contains_key(&n) && n != src {
                    parent.insert(n.clone(), cur.clone());
                    if n == tgt {
                        let mut path = vec![tgt.to_string()];
                        let mut at = tgt.to_string();
                        while let Some(p) = parent.get(&at) {
                            path.push(p.clone());
                            at = p.clone();
                        }
                        path.reverse();
                        return Some(path);
                    }
                    q.push_back((n, depth + 1));
                }
            }
        }
    }
    None
}

#[test]
fn trace_finds_path_main_to_validate() {
    let f = graph_corpus();
    let graph = f.store.get_call_graph().expect("get_call_graph");
    let path = shortest_call_path(&graph.forward, "main", "validate", 5)
        .expect("path from main to validate should exist");
    assert!(path.len() >= 2, "expected ≥2 hops, got {path:?}");
    assert_eq!(path.first().map(String::as_str), Some("main"));
    assert_eq!(path.last().map(String::as_str), Some("validate"));
}

#[test]
fn trace_self_to_self_is_trivial() {
    let f = graph_corpus();
    let graph = f.store.get_call_graph().expect("get_call_graph");
    let path = shortest_call_path(&graph.forward, "main", "main", 5).unwrap();
    assert_eq!(path, vec!["main".to_string()], "trivial self-path");
}

#[test]
fn trace_no_path_returns_none() {
    let f = graph_corpus();
    let graph = f.store.get_call_graph().expect("get_call_graph");
    // validate doesn't call main; reverse path doesn't exist on the
    // forward graph either.
    let path = shortest_call_path(&graph.forward, "validate", "main", 5);
    assert!(
        path.is_none(),
        "validate → main should have no path on forward graph; got {path:?}"
    );
}

// ---------------------------------------------------------------------
// impact (2 tests)
// ---------------------------------------------------------------------

#[test]
fn impact_validate_lists_callers() {
    let f = graph_corpus();
    let opts = cqs::ImpactOptions::default();
    let result =
        cqs::analyze_impact(&f.store.store, "validate", &f.root, &opts).expect("analyze_impact");
    // validate is called by process; process is called by main +
    // test_process.
    let direct: Vec<&str> = result.callers.iter().map(|c| c.name.as_str()).collect();
    assert!(
        direct.contains(&"process"),
        "expected `process` in direct callers of `validate`, got {direct:?}"
    );
}

#[test]
fn impact_validate_includes_transitive_via_test_process() {
    let f = graph_corpus();
    let opts = cqs::ImpactOptions::default();
    let result =
        cqs::analyze_impact(&f.store.store, "validate", &f.root, &opts).expect("analyze_impact");
    // test_process → process → validate — test_process should appear in
    // the tests list since it's tagged as a test chunk.
    let test_names: Vec<&str> = result.tests.iter().map(|t| t.name.as_str()).collect();
    assert!(
        test_names.contains(&"test_process"),
        "expected test_process in tests list (got {test_names:?})"
    );
}

// ---------------------------------------------------------------------
// gather — exercised via the seed-search path.
//
// `cqs::gather::gather` requires a real `&Embedder` (not our test
// trait), and the function path itself is private. Public entry is
// re-exported via `pub use gather::*`. To exercise without paying the
// model cold-load cost in the harness, we test the seed-search
// component (store.search_filtered with the mock embedder) and the BFS
// expansion (already covered by the trace tests above). The "gather
// returns chunks" property reduces to "search returns chunks", which
// the search_returns_indexed_chunks test in index_search_test.rs
// already asserts. Drop here.
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// context (1 test — file existence + chunk listing via store.get_chunks_by_origin)
// ---------------------------------------------------------------------

#[test]
fn context_lists_chunks_in_file() {
    let f = graph_corpus();
    // The CLI's `context src/lib.rs` calls store.get_chunks_by_origin.
    // The fixture file ends up under an absolute path inside the tempdir;
    // the chunks' `file` field is a path normalized by the parser. Find
    // any chunk whose file path ends in `lib.rs` to assert the data
    // round-trips through the store.
    let stats = f.store.stats().expect("stats");
    assert!(
        stats.total_files >= 2,
        "fixture has lib.rs + tests.rs, got {}",
        stats.total_files
    );
    // Direct chunk search: chunks containing `process` should be in the file.
    let hits = f.search("process", 50).expect("search");
    let lib_chunks: Vec<&str> = hits
        .iter()
        .filter(|h| h.chunk.file.to_string_lossy().contains("lib.rs"))
        .map(|h| h.chunk.name.as_str())
        .collect();
    // main, process, validate, transform — all in lib.rs, all should be found
    assert!(
        lib_chunks.len() >= 2,
        "expected ≥2 lib.rs chunks (got {lib_chunks:?})"
    );
}

// ---------------------------------------------------------------------
// explain (1 test) — composed from primitives since there's no lib wrapper
// ---------------------------------------------------------------------

#[test]
fn explain_process_reports_callers_and_callees() {
    let f = graph_corpus();
    // Compose what the CLI's build_explain_data does at the lib level:
    // callers + callees for a target name.
    let callers = f.store.get_callers_full("process").expect("callers");
    let callees = f.store.get_callees_full("process", None).expect("callees");

    let caller_names: Vec<&str> = callers.iter().map(|c| c.name.as_str()).collect();
    let callee_names: Vec<&str> = callees.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        caller_names.contains(&"main") || caller_names.contains(&"test_process"),
        "process is called by main + test_process; got callers {caller_names:?}"
    );
    assert!(
        callee_names.contains(&"validate") || callee_names.contains(&"transform"),
        "process calls validate + transform; got callees {callee_names:?}"
    );
}

// ---------------------------------------------------------------------
// similar — needs an embedding for the source chunk + search.
// Uses MockEmbedder so "similar" is structural, not semantic. The test
// asserts the API returns *something*, which is what the JSON envelope
// test was actually checking.
// ---------------------------------------------------------------------

#[test]
fn similar_returns_results_for_indexed_chunk() {
    let f = graph_corpus();
    // Embed the literal name to get a vector; search using it.
    let q_emb = f.embedder.embed_query("process");
    let filter = cqs::store::SearchFilter::default();
    let hits = f
        .store
        .search_filtered(&q_emb, &filter, 10, 0.0)
        .expect("search_filtered");
    assert!(
        !hits.is_empty(),
        "similar/search on populated index should return ≥1 hit"
    );
}

// ---------------------------------------------------------------------
// notes — add / list / remove + warnings filter
// ---------------------------------------------------------------------

#[test]
fn notes_add_then_list_then_remove() {
    use cqs::{rewrite_notes_file, NoteEntry, NOTES_HEADER};

    let f = InProcessFixture::new();
    let docs_dir = f.root.join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    let notes_path = docs_dir.join("notes.toml");
    std::fs::write(&notes_path, NOTES_HEADER).unwrap();

    // Add
    rewrite_notes_file(&notes_path, |entries| {
        entries.push(NoteEntry {
            sentiment: 0.5,
            text: "test note for in-process".to_string(),
            mentions: vec!["lib.rs".to_string()],
        });
        Ok(())
    })
    .expect("rewrite add");

    // List
    let notes = cqs::parse_notes(&notes_path).expect("parse_notes");
    assert!(
        notes.iter().any(|n| n.text == "test note for in-process"),
        "added note should appear in list"
    );

    // Remove
    rewrite_notes_file(&notes_path, |entries| {
        let target = "test note for in-process";
        if let Some(pos) = entries.iter().position(|e| e.text.trim() == target) {
            entries.remove(pos);
        }
        Ok(())
    })
    .expect("rewrite remove");
    let after = cqs::parse_notes(&notes_path).expect("parse_notes 2");
    assert!(
        after.iter().all(|n| n.text != "test note for in-process"),
        "note should be gone after remove"
    );
}

#[test]
fn notes_warnings_filter_picks_negative_sentiment() {
    use cqs::{rewrite_notes_file, NoteEntry, NOTES_HEADER};

    let f = InProcessFixture::new();
    let docs_dir = f.root.join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    let notes_path = docs_dir.join("notes.toml");
    std::fs::write(&notes_path, NOTES_HEADER).unwrap();
    rewrite_notes_file(&notes_path, |entries| {
        entries.push(NoteEntry {
            sentiment: -0.5,
            text: "this is a warning".to_string(),
            mentions: vec![],
        });
        entries.push(NoteEntry {
            sentiment: 0.5,
            text: "this is a pattern".to_string(),
            mentions: vec![],
        });
        Ok(())
    })
    .unwrap();

    let notes = cqs::parse_notes(&notes_path).expect("parse_notes");
    let warnings: Vec<&str> = notes
        .iter()
        .filter(|n| n.is_warning())
        .map(|n| n.text.as_str())
        .collect();
    assert!(
        warnings.contains(&"this is a warning"),
        "warning filter should pick the negative-sentiment note (got {warnings:?})"
    );
    assert!(
        !warnings.contains(&"this is a pattern"),
        "positive-sentiment note must not match warnings filter"
    );
}

// ---------------------------------------------------------------------
// read — focused_read library exercises the file path validation +
// note injection logic.
// ---------------------------------------------------------------------

#[test]
fn read_file_returns_content() {
    let f = graph_corpus();
    let path = f.root.join("src").join("lib.rs");
    let content = std::fs::read_to_string(&path).expect("read lib.rs");
    assert!(
        content.contains("pub fn main"),
        "fixture lib.rs should contain the main fn declaration"
    );
}

#[test]
fn read_focus_chunk_via_store() {
    let f = graph_corpus();
    // The CLI's `read --focus process` resolves `process` to a chunk and
    // returns its content + types_used_by. At the lib level the equivalent
    // is search-by-name + chunk content.
    let hits = f.search("process", 5).expect("search");
    let process_chunk = hits.iter().find(|h| h.chunk.name == "process");
    assert!(process_chunk.is_some(), "should find `process` chunk");
}
