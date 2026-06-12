//! TC-HAP-1.29-2: smoke tests for batch dispatch handlers.
//!
//! The 16 handlers named in the audit spec are:
//!   gather, scout, task, where, onboard,
//!   callers, callees, impact, test-map, trace,
//!   similar, explain, context, deps, related, impact-diff.
//!
//! Five of those (`gather`, `scout`, `task`, `where`, `onboard`) call
//! `ctx.embedder()` which cold-loads a ~500 MB ONNX session. The audit
//! requires `tests should NOT require model load` — so those five are
//! deliberately SKIPPED here and noted below. The remaining eleven are
//! covered by smoke tests that dispatch one line through
//! `BatchContext::dispatch_line` and assert the daemon envelope shape.
//!
//! Why the tests live in the crate and not in `tests/batch_handlers_test.rs`:
//! `BatchContext::dispatch_line` and `create_test_context` are crate-private
//! (`pub(crate)` / `pub(in crate::cli)`), and integration tests in `tests/`
//! link against the *library* only — the `cli` module lives under
//! `src/main.rs` and isn't reachable from there. Co-locating with
//! `handlers/search.rs::tests` matches the precedent for content-asserting
//! dispatch tests.
//!
//! ## Envelope shape assertions
//!
//! `BatchContext::dispatch_line` wraps a handler's `serde_json::Value`
//! in `{"data": <payload>, "error": null, "version": 1}` via
//! `write_json_line`. Every test here parses that line back and asserts:
//! 1. `error` is null (no handler failure).
//! 2. `data` is non-null and contains the handler's expected top-level keys.
//!
//! The tests do not pin deep payload content — that's handler-specific and
//! already covered by their dedicated test suites (see `search.rs::tests`
//! for the template). This file's job is "the dispatch wiring is connected"
//! smoke coverage.

#![cfg(test)]

use super::super::{create_test_context, BatchContext};
use cqs::embedder::Embedding;
use cqs::parser::{CallSite, Chunk, ChunkType, FunctionCalls, Language};
use cqs::store::{ModelInfo, Store};
use std::path::PathBuf;
use tempfile::TempDir;

/// Construct a chunk with defaults sufficient for dispatch smoke tests.
fn make_chunk(
    id: &str,
    file: &str,
    name: &str,
    signature: &str,
    content: &str,
    line_start: u32,
    line_end: u32,
) -> Chunk {
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: id.to_string(),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: signature.to_string(),
        content: content.to_string(),
        doc: None,
        line_start,
        line_end,
        content_hash,
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    }
}

/// Seed a BatchContext with a small corpus + call graph.
///
/// Layout:
/// - `foo()` calls `bar()` and `baz()`
/// - `bar()` is called by `foo()`
/// - `baz()` is called by `foo()`, calls `helper()`
/// - `test_foo_smoke()` in `tests/foo.rs` calls `foo()` — exercises test-map
/// - `helper()` is a leaf
///
/// The `file` column on function_calls rows is the file where the *caller*
/// lives — that matters for dispatch_test_map which joins on the callee's
/// origin file and the caller's origin file.
fn seed_ctx() -> (TempDir, BatchContext) {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let index_path = cqs_dir.join("index.db");

    // Unit embedding: handlers we test here don't look at embedding content;
    // `upsert_chunks_batch` only validates dimension against ModelInfo::default().
    let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
    emb_vec[0] = 1.0;
    let embedding = Embedding::new(emb_vec);

    let chunks = [
        make_chunk(
            "src/lib.rs:1:foo00001",
            "src/lib.rs",
            "foo",
            "fn foo()",
            "fn foo() { bar(); baz(); }",
            1,
            4,
        ),
        make_chunk(
            "src/lib.rs:6:bar00002",
            "src/lib.rs",
            "bar",
            "fn bar()",
            "fn bar() {}",
            6,
            8,
        ),
        make_chunk(
            "src/lib.rs:10:baz0003",
            "src/lib.rs",
            "baz",
            "fn baz()",
            "fn baz() { helper(); }",
            10,
            12,
        ),
        make_chunk(
            "src/lib.rs:14:hlp0004",
            "src/lib.rs",
            "helper",
            "fn helper()",
            "fn helper() {}",
            14,
            16,
        ),
        make_chunk(
            "tests/foo.rs:1:tst00005",
            "tests/foo.rs",
            "test_foo_smoke",
            "fn test_foo_smoke()",
            "#[test]\nfn test_foo_smoke() { foo(); }",
            1,
            4,
        ),
    ];

    {
        let store = Store::open(&index_path).expect("open store");
        store.init(&ModelInfo::default()).expect("init store");
        let pairs: Vec<(Chunk, Embedding)> = chunks
            .iter()
            .map(|c| (c.clone(), embedding.clone()))
            .collect();
        store
            .upsert_chunks_batch(&pairs, Some(0))
            .expect("upsert chunks");

        // Seed the call graph. `upsert_function_calls_for_files` keys on the
        // caller's file, so group by file.
        let function_calls: Vec<(PathBuf, Vec<FunctionCalls>)> = vec![
            (
                PathBuf::from("src/lib.rs"),
                vec![
                    FunctionCalls {
                        name: "foo".to_string(),
                        line_start: 1,
                        calls: vec![
                            CallSite {
                                callee_name: "bar".to_string(),
                                line_number: 2,
                            },
                            CallSite {
                                callee_name: "baz".to_string(),
                                line_number: 3,
                            },
                        ],
                    },
                    FunctionCalls {
                        name: "baz".to_string(),
                        line_start: 10,
                        calls: vec![CallSite {
                            callee_name: "helper".to_string(),
                            line_number: 11,
                        }],
                    },
                ],
            ),
            (
                PathBuf::from("tests/foo.rs"),
                vec![FunctionCalls {
                    name: "test_foo_smoke".to_string(),
                    line_start: 1,
                    calls: vec![CallSite {
                        callee_name: "foo".to_string(),
                        line_number: 3,
                    }],
                }],
            ),
        ];
        store
            .upsert_function_calls_for_files(&function_calls)
            .expect("upsert calls");
    }

    let ctx = create_test_context(&cqs_dir).expect("create test ctx");
    (dir, ctx)
}

/// Dispatch one line through `BatchContext::dispatch_line` and return the
/// parsed envelope (as serde_json::Value). Panics if the handler didn't
/// write valid JSON — those are the failure modes these tests are catching.
fn dispatch(ctx: &BatchContext, line: &str) -> serde_json::Value {
    let mut sink: Vec<u8> = Vec::new();
    ctx.dispatch_line(line, &mut sink);
    let text = std::str::from_utf8(&sink)
        .unwrap_or_else(|_| panic!("dispatch_line wrote non-UTF-8 for {line:?}"));
    let trimmed = text.trim();
    assert!(
        !trimmed.is_empty(),
        "dispatch_line wrote empty output for {line:?} — handler may have panicked silently"
    );
    serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!("dispatch_line output is not valid JSON for {line:?}: {e}\nraw: {trimmed}")
    })
}

/// Assert the envelope succeeded and return the `data` field.
///
/// **Wire shape:** the batch / daemon JSONL path emits the slim envelope,
/// which drops `error: null` and `version` — the success contract is
/// "no `error` key present". The full `{data, error, version, _meta}`
/// envelope is only produced by `Envelope::ok` on the CLI direct path
/// under `CQS_OUTPUT_FORMAT=v1`.
fn assert_ok_envelope<'a>(env: &'a serde_json::Value, ctx_label: &str) -> &'a serde_json::Value {
    assert!(
        env.get("error").is_none(),
        "{ctx_label} returned error envelope (slim shape skips error key only when null): {env}"
    );
    let data = env
        .get("data")
        .unwrap_or_else(|| panic!("{ctx_label}: envelope missing `data` field: {env}"));
    assert!(
        !data.is_null(),
        "{ctx_label}: `data` is null (expected handler payload): {env}"
    );
    data
}

// ───────────────────────────────────────────────────────────────────────────
// Graph handlers (no embedder required)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn dispatch_callers_returns_envelope_with_callers() {
    let (_dir, ctx) = seed_ctx();
    let env = dispatch(&ctx, "callers bar");
    let data = assert_ok_envelope(&env, "callers bar");
    // `callers_core` emits `{name, callers, count}` — the same object
    // topology as callees, keyed by `callers` rather than `calls`.
    assert_eq!(data["name"], "bar", "callers payload name: {data}");
    let callers = data
        .get("callers")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected `callers` array in callers payload: {data}"));
    assert!(
        !callers.is_empty(),
        "foo() calls bar() in the seeded graph, so callers must be non-empty: {data}"
    );
    let names: Vec<&str> = callers
        .iter()
        .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(
        names.contains(&"foo"),
        "expected 'foo' among bar's callers, got {names:?}"
    );
}

#[test]
fn dispatch_callees_returns_envelope_with_calls() {
    let (_dir, ctx) = seed_ctx();
    let env = dispatch(&ctx, "callees foo");
    let data = assert_ok_envelope(&env, "callees foo");
    // build_callees emits `{function, calls, count}`.
    let calls = data
        .get("calls")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected `calls` array in callees payload: {data}"));
    assert!(
        !calls.is_empty(),
        "foo() calls bar() and baz() — callees must be non-empty: {data}"
    );
    let callee_names: Vec<&str> = calls
        .iter()
        .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(
        callee_names.contains(&"bar") && callee_names.contains(&"baz"),
        "expected 'bar' and 'baz' among foo's callees, got {callee_names:?}"
    );
}

#[test]
fn dispatch_impact_returns_envelope_with_callers_list() {
    let (_dir, ctx) = seed_ctx();
    // `helper` has a single caller (`baz`), a transitive caller (`foo`), and
    // a transitive test caller (`test_foo_smoke` via foo). depth=3 reaches them.
    let env = dispatch(&ctx, "impact helper --depth 3");
    let data = assert_ok_envelope(&env, "impact helper");
    // `impact_to_json` emits a map with at minimum `callers` / `target` keys
    // (see `cqs::impact_to_json`); the exact shape is pinned by dedicated
    // tests elsewhere. Smoke-assert the top-level keys exist.
    assert!(
        data.get("callers").is_some() || data.get("target").is_some(),
        "impact payload should carry `callers` or `target`: {data}"
    );
}

#[test]
fn dispatch_test_map_returns_envelope_with_matches() {
    let (_dir, ctx) = seed_ctx();
    // `test_foo_smoke` is in tests/foo.rs and calls foo(); test-map on foo
    // should surface it at depth 1.
    let env = dispatch(&ctx, "test-map foo --depth 2");
    let data = assert_ok_envelope(&env, "test-map foo");
    // `build_test_map_output` returns `{name, tests, count}` — `name` echoes
    // the resolved target, `tests` lists matches with call_chain / depth.
    assert_eq!(
        data.get("name").and_then(|v| v.as_str()),
        Some("foo"),
        "test-map payload must echo resolved name: {data}"
    );
    let tests = data
        .get("tests")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("test-map payload must carry `tests` array: {data}"));
    assert!(
        tests
            .iter()
            .any(|t| { t.get("name").and_then(|n| n.as_str()) == Some("test_foo_smoke") }),
        "test-map(foo) should surface test_foo_smoke from the seeded graph: {data}"
    );
}

#[test]
fn dispatch_trace_returns_envelope_with_source_target() {
    let (_dir, ctx) = seed_ctx();
    // foo → baz → helper is a length-2 path.
    let env = dispatch(&ctx, "trace foo helper --max-depth 3");
    let data = assert_ok_envelope(&env, "trace foo helper");
    // `build_trace_output` returns `{source, target, path, ...}`.
    assert_eq!(
        data.get("source").and_then(|v| v.as_str()),
        Some("foo"),
        "trace source should be foo: {data}"
    );
    assert_eq!(
        data.get("target").and_then(|v| v.as_str()),
        Some("helper"),
        "trace target should be helper: {data}"
    );
}

#[test]
fn dispatch_deps_returns_envelope() {
    let (_dir, ctx) = seed_ctx();
    // deps in forward mode queries `get_type_users` — against our corpus
    // (no type edges seeded), this returns an empty list. The smoke test is
    // that the envelope comes back OK and carries a list.
    let env = dispatch(&ctx, "deps SomeType");
    let data = assert_ok_envelope(&env, "deps SomeType");
    // `build_deps_forward` returns an array of `{name, file, line, chunk_type}`.
    assert!(
        data.is_array() || data.is_object(),
        "deps payload should be a JSON array or object: {data}"
    );
}

#[test]
fn dispatch_related_returns_envelope_with_target() {
    let (_dir, ctx) = seed_ctx();
    let env = dispatch(&ctx, "related foo --limit 5");
    let data = assert_ok_envelope(&env, "related foo");
    // `build_related_output` emits `{target, shared_callers, shared_callees, shared_types}`.
    assert_eq!(
        data.get("target").and_then(|v| v.as_str()),
        Some("foo"),
        "related payload must echo target: {data}"
    );
    assert!(
        data.get("shared_callers").is_some(),
        "related payload should carry shared_callers: {data}"
    );
}

#[test]
fn dispatch_impact_diff_returns_envelope_even_without_git() {
    let (_dir, ctx) = seed_ctx();
    // Without a git repo, `impact-diff` takes the empty-hunks path and
    // returns the empty summary envelope — that's still a valid OK envelope
    // and is the graceful-fail contract. If the shell-out to `git diff`
    // somehow errors here, the handler surfaces an IO error through the
    // envelope; we accept either shape for this smoke test.
    let mut sink: Vec<u8> = Vec::new();
    ctx.dispatch_line("impact-diff", &mut sink);
    let text = std::str::from_utf8(&sink).expect("utf8 output");
    let trimmed = text.trim();
    assert!(
        !trimmed.is_empty(),
        "impact-diff wrote no output — handler likely panicked"
    );
    let env: serde_json::Value =
        serde_json::from_str(trimmed).expect("impact-diff emits valid JSON envelope");
    assert!(
        env.get("data").is_some() || env.get("error").is_some(),
        "impact-diff envelope must carry data or error: {env}"
    );
    // If it's a happy path, the payload has the documented shape.
    if env.get("error") == Some(&serde_json::Value::Null) {
        let data = env.get("data").expect("data present on ok envelope");
        assert!(
            data.get("changed_functions").is_some() && data.get("summary").is_some(),
            "impact-diff ok payload must carry `changed_functions` and `summary`: {data}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Info handlers (similar uses vector index but no embedder; context/explain
// use embedder only when `--tokens` is set — we omit that flag here to
// keep the handlers off the ONNX path).
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn dispatch_similar_returns_envelope_with_results() {
    let (_dir, ctx) = seed_ctx();
    // `similar` loads the target chunk's stored embedding and searches the
    // store's vector index. No ONNX call — the model is only needed to
    // *produce* embeddings, not to compare them.
    let env = dispatch(&ctx, "similar foo --limit 3 --threshold 0.0");
    let data = assert_ok_envelope(&env, "similar foo");
    assert_eq!(
        data.get("target").and_then(|v| v.as_str()),
        Some("foo"),
        "similar payload must echo target: {data}"
    );
    assert!(
        data.get("results").and_then(|v| v.as_array()).is_some(),
        "similar payload must carry `results` array: {data}"
    );
}

#[test]
fn parity_similar_daemon_matches_core() {
    // The structural payoff of the command-core split: the daemon
    // `dispatch_similar` adapter and a direct `similar_core` +
    // `build_similar_output` produce a byte-identical `serde_json::Value` for
    // the same input. Parity-BY-CONSTRUCTION — the handler is a thin wrapper
    // over the very core compared against — plus a fixture-grounded value
    // assert so a both-sides-empty regression still fails.
    use super::info::dispatch_similar;
    use crate::cli::commands::search::similar::{build_similar_output, similar_core, SimilarArgs};

    let (_dir, ctx) = seed_ctx();
    let view = ctx.build_view(None);

    // The seed corpus stores a unit embedding on every chunk, so cosine
    // similarity is 1.0 across the board; threshold 0.0 keeps them all.
    let wire = crate::cli::args::SimilarArgs {
        name: "foo".into(),
        limit_arg: crate::cli::args::LimitArg { limit: 3 },
        threshold: 0.0,
        lang: None,
        path: None,
    };
    let daemon = dispatch_similar(&view, &wire).expect("dispatch_similar");

    let store = view.store();
    let index = view.vector_index().expect("vector_index");
    let filter = cqs::SearchFilter::default();
    let core_args = SimilarArgs {
        name: "foo".into(),
        limit: 3,
        threshold: 0.0,
    };
    let matches =
        similar_core(&store, index.as_deref(), &filter, &core_args).expect("similar_core");
    let core = serde_json::to_value(build_similar_output(&matches)).expect("serialize output");

    // Fixture-grounded: `foo` resolves and its peers (bar/baz/helper) are all
    // unit-embedding matches, so the core output must be non-empty before the
    // byte-equality check below can mean anything.
    assert_eq!(core["target"], "foo");
    let results = core["results"]
        .as_array()
        .unwrap_or_else(|| panic!("similar core must carry a results array: {core}"));
    assert!(
        !results.is_empty(),
        "seeded corpus must yield at least one similar result: {core}"
    );
    assert_eq!(daemon, core, "similar parity mismatch");
}

/// Construct a chunk with an explicit language + file, all else defaulted.
/// `seed_ctx`/`make_chunk` hardcode Rust in `src/lib.rs`; the scope-parity
/// tests below need a corpus that spans languages and paths so a `--lang` /
/// `--path` filter actually drops rows.
fn make_chunk_lang(id: &str, file: &str, name: &str, lang: Language) -> Chunk {
    let content = format!("fn {name}() {{ }}");
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: id.to_string(),
        file: PathBuf::from(file),
        language: lang,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {name}()"),
        content,
        doc: None,
        line_start: 1,
        line_end: 3,
        content_hash,
        canonical_hash: String::new(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    }
}

/// Seed a context spanning two languages and two directories so a scope
/// filter bites: `target` (Rust, `src/`) plus a Rust peer in `src/`, a Rust
/// peer in `vendor/`, and a Python peer in `lib/`. All chunks share the unit
/// embedding so cosine is 1.0 across the board — the *only* thing that can
/// shrink the result set is the `--lang` / `--path` filter.
fn seed_mixed_ctx() -> (TempDir, BatchContext) {
    let dir = TempDir::new().expect("tempdir");
    let cqs_dir = dir.path().join(".cqs");
    std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
    let index_path = cqs_dir.join("index.db");

    let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
    emb_vec[0] = 1.0;
    let embedding = Embedding::new(emb_vec);

    let chunks = [
        make_chunk_lang("src/a.rs:1:target0", "src/a.rs", "target", Language::Rust),
        make_chunk_lang("src/b.rs:1:rspeer0", "src/b.rs", "rs_peer", Language::Rust),
        make_chunk_lang(
            "vendor/v.rs:1:vendrs0",
            "vendor/v.rs",
            "vendored_rs_peer",
            Language::Rust,
        ),
        make_chunk_lang(
            "lib/p.py:1:pypeer0",
            "lib/p.py",
            "py_peer",
            Language::Python,
        ),
    ];

    {
        let store = Store::open(&index_path).expect("open store");
        store.init(&ModelInfo::default()).expect("init store");
        let pairs: Vec<(Chunk, Embedding)> = chunks
            .iter()
            .map(|c| (c.clone(), embedding.clone()))
            .collect();
        store
            .upsert_chunks_batch(&pairs, Some(0))
            .expect("upsert chunks");
    }

    let ctx = create_test_context(&cqs_dir).expect("create test ctx");
    (dir, ctx)
}

/// Drive the daemon `dispatch_similar` for `target` with the given scope flags
/// and return the parsed payload (envelope unwrapped to `data`).
fn similar_filter_names(daemon: &serde_json::Value) -> std::collections::BTreeSet<String> {
    daemon["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `--lang` / `--path` scope flags reach the daemon `similar` path and produce
/// the same payload as the CLI-shaped `similar_core` driven with the matching
/// `build_similar_filter`. Two filtered cases (lang + path) each assert the
/// filter actually *bites* — fewer results than the unfiltered baseline and
/// the dropped rows are exactly the off-scope peers.
#[test]
fn parity_similar_scoped_daemon_matches_core() {
    use super::info::dispatch_similar;
    use crate::cli::commands::search::similar::{
        build_similar_filter, build_similar_output, similar_core, SimilarArgs,
    };

    let (_dir, ctx) = seed_mixed_ctx();
    let view = ctx.build_view(None);
    let store = view.store();
    let index = view.vector_index().expect("vector_index");

    // Baseline: no scope. `target` (Rust/src) plus the three peers are all
    // unit-embedding matches; self-match excluded, so 3 peers remain.
    let unfiltered = dispatch_similar(
        &view,
        &crate::cli::args::SimilarArgs {
            name: "target".into(),
            limit_arg: crate::cli::args::LimitArg { limit: 10 },
            threshold: 0.0,
            lang: None,
            path: None,
        },
    )
    .expect("dispatch_similar baseline");
    let baseline_names = similar_filter_names(&unfiltered);
    assert_eq!(
        baseline_names.len(),
        3,
        "unfiltered must surface all three peers: {unfiltered}"
    );
    assert!(
        baseline_names.contains("py_peer")
            && baseline_names.contains("vendored_rs_peer")
            && baseline_names.contains("rs_peer"),
        "baseline peer set unexpected: {baseline_names:?}"
    );

    // ── Case 1: `--lang rust` drops the Python peer. ──
    let wire_lang = crate::cli::args::SimilarArgs {
        name: "target".into(),
        limit_arg: crate::cli::args::LimitArg { limit: 10 },
        threshold: 0.0,
        lang: Some("rust".into()),
        path: None,
    };
    let daemon_lang = dispatch_similar(&view, &wire_lang).expect("dispatch_similar lang");

    let filter_lang = build_similar_filter(Some("rust"), None).expect("build_similar_filter lang");
    let core_lang = similar_core(
        &store,
        index.as_deref(),
        &filter_lang,
        &SimilarArgs {
            name: "target".into(),
            limit: 10,
            threshold: 0.0,
        },
    )
    .expect("similar_core lang");
    let core_lang_val =
        serde_json::to_value(build_similar_output(&core_lang)).expect("serialize core lang");

    assert_eq!(
        daemon_lang, core_lang_val,
        "lang-scoped similar parity mismatch"
    );
    let lang_names = similar_filter_names(&daemon_lang);
    assert!(
        lang_names.len() < baseline_names.len(),
        "--lang rust must bite (fewer than unfiltered): {lang_names:?} vs {baseline_names:?}"
    );
    assert!(
        !lang_names.contains("py_peer"),
        "--lang rust must drop the Python peer: {lang_names:?}"
    );
    assert!(
        lang_names.contains("rs_peer") && lang_names.contains("vendored_rs_peer"),
        "--lang rust must keep the Rust peers: {lang_names:?}"
    );

    // ── Case 2: `--path src/*` drops the vendor + lib peers. ──
    let wire_path = crate::cli::args::SimilarArgs {
        name: "target".into(),
        limit_arg: crate::cli::args::LimitArg { limit: 10 },
        threshold: 0.0,
        lang: None,
        path: Some("src/*".into()),
    };
    let daemon_path = dispatch_similar(&view, &wire_path).expect("dispatch_similar path");

    let filter_path = build_similar_filter(None, Some("src/*")).expect("build_similar_filter path");
    let core_path = similar_core(
        &store,
        index.as_deref(),
        &filter_path,
        &SimilarArgs {
            name: "target".into(),
            limit: 10,
            threshold: 0.0,
        },
    )
    .expect("similar_core path");
    let core_path_val =
        serde_json::to_value(build_similar_output(&core_path)).expect("serialize core path");

    assert_eq!(
        daemon_path, core_path_val,
        "path-scoped similar parity mismatch"
    );
    let path_names = similar_filter_names(&daemon_path);
    assert!(
        path_names.len() < baseline_names.len(),
        "--path src/* must bite: {path_names:?} vs {baseline_names:?}"
    );
    assert!(
        path_names.contains("rs_peer"),
        "--path src/* must keep the src peer: {path_names:?}"
    );
    assert!(
        !path_names.contains("vendored_rs_peer") && !path_names.contains("py_peer"),
        "--path src/* must drop off-path peers: {path_names:?}"
    );
}

#[test]
fn dispatch_explain_returns_envelope_without_tokens() {
    let (_dir, ctx) = seed_ctx();
    // Without --tokens, `dispatch_explain` skips the `ctx.embedder()?` path.
    let env = dispatch(&ctx, "explain foo");
    let data = assert_ok_envelope(&env, "explain foo");
    // `build_explain_output` returns a map with `name`, `signature`, etc.
    assert!(
        data.get("name").and_then(|v| v.as_str()) == Some("foo")
            || data.get("target").and_then(|v| v.as_str()) == Some("foo"),
        "explain payload must reference target 'foo': {data}"
    );
}

#[test]
fn dispatch_context_returns_envelope_without_tokens() {
    let (_dir, ctx) = seed_ctx();
    // Without --tokens, `dispatch_context` never reaches `ctx.embedder()?`.
    // The full-context path expects the file to have indexed chunks.
    let env = dispatch(&ctx, "context src/lib.rs");
    let data = assert_ok_envelope(&env, "context src/lib.rs");
    assert!(
        data.get("file").and_then(|v| v.as_str()).is_some()
            || data.get("chunks").and_then(|v| v.as_array()).is_some(),
        "context payload must carry `file` or `chunks`: {data}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Skipped handlers (model-loading).
//
// The five below call `ctx.embedder()?` on every invocation, which
// cold-loads a ~500 MB ONNX session. Including them here would violate the
// audit requirement that tests not require model load. They are covered
// by the real-embedder eval suite and their constituent library functions
// (cqs::gather, cqs::scout, cqs::task, cqs::suggest_placement, cqs::onboard)
// have their own tests exercising the semantic path.
//
//   - dispatch_gather   — GatherArgs → cqs::gather (embeds query)
//   - dispatch_scout    — cqs::scout (embeds query)
//   - dispatch_task     — cqs::task_with_resources (embeds query)
//   - dispatch_where    — cqs::suggest_placement (embeds description)
//   - dispatch_onboard  — cqs::onboard (embeds query)
//
// ───────────────────────────────────────────────────────────────────────────
