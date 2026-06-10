//! Test map command — find tests that exercise a function
//!
//! Core BFS logic is in `build_test_map()` so batch mode can reuse it.
//!
//! ## Polymorphic routing
//!
//! `cqs test-map <name>` consults `cqs::kind::classify_hits` against an
//! exact-name lookup before the BFS query. For a name that isn't a
//! function-or-method, the kind-mismatch fallback emits a kind-labeled
//! definition list with a redirect note instead of an empty match list.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};

use cqs::kind::{classify_hits, Kind, KindHit};
use cqs::store::{CallGraph, ChunkSummary};

use crate::cli::commands::resolve::resolve_target;

// ─── Shared data structures ─────────────────────────────────────────────────

/// A test that exercises the target function, found via reverse BFS.
pub(crate) struct TestMatch {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub depth: usize,
    pub chain: Vec<String>,
}

// ─── Output types ───────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct TestMapEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub call_depth: usize,
    pub call_chain: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TestMapOutput {
    pub name: String,
    pub tests: Vec<TestMapEntry>,
    pub count: usize,
}

// ─── Shared core ────────────────────────────────────────────────────────────

/// Default maximum nodes in test-map reverse BFS traversal.
const DEFAULT_TEST_MAP_MAX_NODES: usize = 10_000;

/// Returns the test-map BFS node cap, reading `CQS_TEST_MAP_MAX_NODES` once on first call.
fn test_map_max_nodes() -> usize {
    use std::sync::OnceLock;
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| match std::env::var("CQS_TEST_MAP_MAX_NODES") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(
                    cap = n,
                    "Test-map BFS node cap overridden via CQS_TEST_MAP_MAX_NODES"
                );
                n
            }
            _ => {
                tracing::warn!(
                    val,
                    "CQS_TEST_MAP_MAX_NODES invalid, using default {DEFAULT_TEST_MAP_MAX_NODES}"
                );
                DEFAULT_TEST_MAP_MAX_NODES
            }
        },
        Err(_) => DEFAULT_TEST_MAP_MAX_NODES,
    })
}

/// Reverse BFS through the call graph to find all test chunks that call the
/// target, up to `max_depth`. Returns sorted matches.
///
/// Capped at `CQS_TEST_MAP_MAX_NODES` (default 10,000) visited nodes to prevent
/// OOM on dense graphs.
///
/// Shared between CLI `cmd_test_map` and batch `dispatch_test_map`.
pub(crate) fn build_test_map(
    target_name: &str,
    graph: &CallGraph,
    test_chunks: &[ChunkSummary],
    root: &Path,
    max_depth: usize,
) -> Vec<TestMatch> {
    let _span = tracing::info_span!("build_test_map", target_name, max_depth).entered();
    let max_nodes = test_map_max_nodes();

    // Reverse BFS from target.
    // Keys + parent-pointers are `Arc<str>` so the BFS reuses the
    // already-interned names from `graph.reverse` instead of allocating a
    // fresh `String` per visit (~10k `to_string()` + `clone()` allocations
    // per call on hub functions otherwise). The chain walk clones via
    // `Arc::clone` (RC bump). A `None` parent encodes "this is the target".
    let mut ancestors: HashMap<Arc<str>, (usize, Option<Arc<str>>)> = HashMap::new();
    let mut queue: VecDeque<(Arc<str>, usize)> = VecDeque::new();
    let target_arc: Arc<str> = Arc::from(target_name);
    ancestors.insert(Arc::clone(&target_arc), (0, None));
    queue.push_back((target_arc, 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if ancestors.len() >= max_nodes {
            tracing::warn!(
                target_name,
                max_nodes,
                "test_map reverse BFS hit node cap, returning partial results"
            );
            break;
        }
        if let Some(callers) = graph.reverse.get(current.as_ref()) {
            for caller in callers {
                if ancestors.len() >= max_nodes {
                    break;
                }
                if !ancestors.contains_key(caller.as_ref()) {
                    ancestors.insert(Arc::clone(caller), (depth + 1, Some(Arc::clone(&current))));
                    queue.push_back((Arc::clone(caller), depth + 1));
                }
            }
        }
    }

    // Collect matching tests
    let mut matches: Vec<TestMatch> = Vec::new();
    for test in test_chunks.iter() {
        if let Some((depth, _)) = ancestors.get(test.name.as_str()) {
            if *depth > 0 {
                let mut chain: Vec<String> = Vec::new();
                // The chain walk needs an owned `Arc<str>` cursor to iterate
                // parent pointers. Each step clones via `Arc::clone` (RC
                // bump only); the rendered chain entries are `String` for
                // the public TestMatch API.
                let mut cursor: Arc<str> = Arc::from(test.name.as_str());
                // `saturating_add` keeps `chain_limit` bounded under any
                // caller-supplied `max_depth`. The clap range bound on
                // `TestMapArgs::depth` already caps this in practice
                // (1..=50); the saturating arithmetic is defensive against
                // direct lib callers bypassing clap.
                let chain_limit = max_depth.saturating_add(1);
                while chain.len() < chain_limit {
                    chain.push(cursor.as_ref().to_string());
                    if cursor.as_ref() == target_name {
                        break;
                    }
                    cursor = match ancestors.get(&cursor) {
                        Some((_, Some(p))) => Arc::clone(p),
                        _ => {
                            tracing::debug!(node = %cursor, "Chain walk hit dead end");
                            break;
                        }
                    };
                }
                let rel_file = cqs::rel_display(&test.file, root);
                matches.push(TestMatch {
                    name: test.name.clone(),
                    file: rel_file,
                    line: test.line_start,
                    depth: *depth,
                    chain,
                });
            }
        }
    }

    matches.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));
    matches
}

/// Build typed test map output from matches -- shared between CLI and batch.
pub(crate) fn build_test_map_output(target_name: &str, matches: &[TestMatch]) -> TestMapOutput {
    let _span =
        tracing::info_span!("build_test_map_output", target_name, count = matches.len()).entered();
    TestMapOutput {
        name: target_name.to_string(),
        tests: matches
            .iter()
            .map(|m| TestMapEntry {
                name: m.name.clone(),
                file: m.file.clone(),
                line_start: m.line,
                call_depth: m.depth,
                call_chain: m.chain.clone(),
            })
            .collect(),
        count: matches.len(),
    }
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_test_map(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    max_depth: usize,
    limit: usize,
    cross_project: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_test_map", name, limit, cross_project).entered();
    // Cap on rendered matches. Default is 5 (LimitArg). Truncates the
    // BFS-derived matches AFTER sorting so the "closest" tests rank first.
    let limit = limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let test_chunks = cross_ctx.find_test_chunks_cross()?;

        // Build a merged call graph from all projects
        let graph = cross_ctx.merged_call_graph()?;
        let summaries: Vec<cqs::store::ChunkSummary> =
            test_chunks.iter().map(|tc| tc.chunk.clone()).collect();

        let mut matches = build_test_map(name, &graph, &summaries, &ctx.root, max_depth);
        matches.truncate(limit);

        if json {
            let output = build_test_map_output(name, &matches);
            crate::cli::json_envelope::emit_json(&output)?;
        } else {
            use colored::Colorize;
            println!("{} {} (cross-project)", "Tests for:".cyan(), name.bold());
            if matches.is_empty() {
                println!("  No tests found");
            } else {
                for m in &matches {
                    println!("  {} ({}:{}) [depth {}]", m.name, m.file, m.line, m.depth);
                    if m.chain.len() > 2 {
                        println!("    chain: {}", m.chain.join(" -> "));
                    }
                }
                println!("\n{} tests found", matches.len());
            }
        }
        return Ok(());
    }

    let store = &ctx.store;
    let root = &ctx.root;

    // Polymorphic-routing kind detection. Same dispatch pattern as
    // cmd_impact / cmd_callers / cmd_callees.
    let chunks = store.lookup_by_name(name)?;
    let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
    let kind = classify_hits(&hits);
    match kind {
        Kind::Const => {
            return test_map_kind_fallback(
                name, &chunks, json, "const",
                "consts don't have a call-graph; tests don't 'cover' a const value the way they cover a function. \
                 Use `cqs <name>` to find tests that reference this const by name.",
                &format!("(test-map) `{name}` is a const, not a function — call-graph test-map analysis doesn't apply."),
                "Use `cqs <name>` to find tests that reference this const by name.",
            );
        }
        Kind::Type => {
            return test_map_kind_fallback(
                name, &chunks, json, "type",
                "types don't have a call-graph in the same sense; here are the type's definition sites. \
                 Use `cqs <name>` to find tests that reference this type, or `cqs test-map <Type::method>` for a specific method's coverage.",
                &format!("(test-map) `{name}` is a type, not a function — call-graph test-map analysis doesn't apply."),
                "Use `cqs <name>` to find tests that reference this type or call against a specific method.",
            );
        }
        Kind::Module => {
            return test_map_kind_fallback(
                name, &chunks, json, "module",
                "modules don't have a call-graph; tests cover specific functions inside the module, not the module itself. \
                 Use `cqs <name>` to find tests in this module's files.",
                &format!("(test-map) `{name}` is a module/namespace, not a function — call-graph test-map analysis doesn't apply."),
                "Use `cqs <name>` to find tests in the module's files.",
            );
        }
        Kind::Ambiguous => {
            return test_map_kind_fallback(
                name, &chunks, json, "ambiguous",
                "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
                 Re-run `cqs test-map <name>` against a more specific name (e.g. `Type::method`).",
                &format!("(test-map) `{name}` is ambiguous — matches multiple chunk kinds."),
                "Re-run with a more specific name (e.g. `Type::method`).",
            );
        }
        _ => {}
    }

    let resolved = resolve_target(store, name)?;
    let target_name = resolved.chunk.name.clone();

    let graph = store
        .get_call_graph()
        .context("Failed to load call graph")?;
    let test_chunks = store
        .find_test_chunks()
        .context("Failed to find test chunks")?;

    let mut matches = build_test_map(&target_name, &graph, &test_chunks, root, max_depth);
    matches.truncate(limit);

    if json {
        let output = build_test_map_output(&target_name, &matches);
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        use colored::Colorize;
        println!("{} {}", "Tests for:".cyan(), target_name.bold());
        if matches.is_empty() {
            println!("  No tests found");
        } else {
            for m in &matches {
                println!("  {} ({}:{}) [depth {}]", m.name, m.file, m.line, m.depth);
                if m.chain.len() > 2 {
                    println!("    chain: {}", m.chain.join(" -> "));
                }
            }
            println!("\n{} tests found", matches.len());
        }
    }

    Ok(())
}

/// Polymorphic-routing kind-mismatch fallback for `cqs test-map <name>`.
/// Same shape as the impact / callers / callees fallbacks — emits a
/// `{kind, fallback_from: "test-map", name, definitions, note}` object
/// under JSON, plain-text definitions + redirect under text mode.
fn test_map_kind_fallback(
    name: &str,
    chunks: &[ChunkSummary],
    json: bool,
    kind_label: &str,
    note: &str,
    text_lead: &str,
    text_redirect: &str,
) -> Result<()> {
    debug_assert!(
        !chunks.is_empty(),
        "Kind fallback called with no hits — caller must classify before dispatching"
    );
    if json {
        let definitions = super::chunks_to_definitions(chunks);
        let payload = serde_json::json!({
            "kind": kind_label,
            "fallback_from": "test-map",
            "name": name,
            "definitions": definitions,
            "note": note,
        });
        crate::cli::json_envelope::emit_json(&payload)?;
    } else {
        println!("{text_lead}");
        println!();
        println!("Definitions:");
        for c in chunks {
            println!(
                "  {}:{}-{} ({} {})",
                cqs::normalize_path(&c.file),
                c.line_start,
                c.line_end,
                c.language,
                c.chunk_type
            );
            if !c.signature.is_empty() {
                println!("    {}", c.signature);
            }
        }
        println!();
        println!("{text_redirect}");
    }
    Ok(())
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_test_map_output_field_names() {
        let output = TestMapOutput {
            name: "my_func".into(),
            tests: vec![TestMapEntry {
                name: "test_it".into(),
                file: "tests/foo.rs".into(),
                line_start: 10,
                call_depth: 1,
                call_chain: vec!["my_func".into()],
            }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "my_func");
        assert!(json.get("function").is_none());
        assert_eq!(json["tests"][0]["line_start"], 10);
    }

    #[test]
    fn test_test_map_output_empty() {
        let output = build_test_map_output("no_tests", &[]);
        assert_eq!(output.count, 0);
        assert!(output.tests.is_empty());
    }

    // test-map kind-mismatch fallback shape.
    fn make_chunk(
        chunk_type: cqs::parser::ChunkType,
        name: &str,
        file: &str,
        line: u32,
    ) -> ChunkSummary {
        ChunkSummary {
            id: format!("{}:{}:{}", file, line, "abcd1234"),
            file: std::path::PathBuf::from(file),
            language: cqs::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("test sig for {}", name),
            content: format!("test content for {}", name),
            doc: None,
            line_start: line,
            line_end: line,
            content_hash: "abcd1234".to_string(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }

    #[test]
    fn test_map_fallback_payload_shape() {
        // Pin the {kind, fallback_from: "test-map", name, definitions, note} shape.
        let chunk = make_chunk(cqs::parser::ChunkType::Constant, "X", "src/a.rs", 5);
        let definitions = super::super::chunks_to_definitions(&[chunk]);
        let payload = serde_json::json!({
            "kind": "const",
            "fallback_from": "test-map",
            "name": "X",
            "definitions": definitions,
            "note": "test note",
        });
        assert_eq!(payload["kind"], "const");
        assert_eq!(payload["fallback_from"], "test-map");
        assert_eq!(payload["name"], "X");
        assert_eq!(payload["definitions"].as_array().unwrap().len(), 1);
    }

    // The test-map kind fallback routes through the shared
    // `chunks_to_definitions`, capping entry count and truncating
    // oversized content so a hot name can't emit unbounded JSON.
    #[test]
    fn test_map_fallback_caps_definitions_count() {
        use super::super::{chunks_to_definitions, KIND_FALLBACK_MAX_DEFINITIONS};
        let chunks: Vec<ChunkSummary> = (0..(KIND_FALLBACK_MAX_DEFINITIONS + 50))
            .map(|i| {
                make_chunk(
                    cqs::parser::ChunkType::Constant,
                    &format!("X{i}"),
                    "src/lib.rs",
                    i as u32,
                )
            })
            .collect();
        let defs = chunks_to_definitions(&chunks);
        assert_eq!(defs.len(), KIND_FALLBACK_MAX_DEFINITIONS);
    }

    #[test]
    fn test_map_fallback_truncates_oversized_content() {
        use super::super::{chunks_to_definitions, KIND_FALLBACK_MAX_CONTENT_BYTES};
        let mut big = make_chunk(cqs::parser::ChunkType::Constant, "BIG", "src/lib.rs", 1);
        big.content = "x".repeat(KIND_FALLBACK_MAX_CONTENT_BYTES * 2);
        let defs = chunks_to_definitions(&[big]);
        let content = defs[0]["content"].as_str().unwrap();
        assert!(content.ends_with("... (truncated)"));
        assert_eq!(defs[0]["truncated"], true);
    }
}
