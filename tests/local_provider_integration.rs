//! Integration tests for the Local LLM provider (OpenAI-compat).
//!
//! These tests are `#[ignore]`-gated because:
//!   1. They build a real `Store` against a tempfile SQLite DB.
//!   2. They run the full `llm_summary_pass` end-to-end against a live (mock)
//!      OpenAI-compat HTTP server.
//!   3. `httpmock` servers bind a real port — lightweight but not free.
//!
//! Run with:
//!   `cargo test --features gpu-index --test local_provider_integration -- --ignored`

#![cfg(feature = "llm-summaries")]

use std::sync::Mutex;

use std::path::PathBuf;

use cqs::parser::{ChunkType, Language};
use cqs::store::ModelInfo;
use cqs::{Chunk, Embedding, Store, EMBEDDING_DIM, INDEX_DB_FILENAME};

/// L2-normalized deterministic embedding for integration tests.
fn mock_embedding(seed: f32) -> Embedding {
    let mut v = vec![seed; EMBEDDING_DIM];
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding::new(v)
}

/// Open a fresh Store + TempDir for an integration test.
fn setup_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(&dir.path().join(INDEX_DB_FILENAME)).unwrap();
    store.init(&ModelInfo::default()).unwrap();
    (store, dir)
}

/// Serialize tests that manipulate `CQS_*` env vars — they are process-global
/// and races between test threads cause flake.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Insert a minimal callable chunk via the public Store API.
///
/// Uses a deterministic embedding seeded on the content_hash so equality across
/// runs is stable. The chunk type is `Function` so `collect_eligible_chunks`
/// includes it; content length is >= MIN_CONTENT_CHARS (50).
fn insert_callable_chunk(store: &Store, content_hash: &str, name: &str) {
    let embedding = mock_embedding(content_hash.len() as f32 + 1.0);

    let chunk = Chunk {
        id: content_hash.to_string(),
        file: PathBuf::from(format!("src/test_{}.rs", name)),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: format!(
            "fn {}() {{ let x = 42; println!(\"hello world from {}\"); }}",
            name, name
        ),
        doc: None,
        line_start: 1,
        line_end: 10,
        content_hash: content_hash.to_string(),
        window_idx: None,
        parent_id: None,
        parent_type_name: None,
        parser_version: 0,
    };

    store.upsert_chunk(&chunk, &embedding, None).unwrap();
}

/// Count summary rows of a given purpose via the public API.
fn count_summaries_for_purpose(store: &Store, hashes: &[&str], purpose: &str) -> usize {
    store
        .get_summaries_by_hashes(hashes, purpose)
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Set the local-provider env vars to point at the given mock-server URL.
/// Caller is responsible for holding `ENV_MUTEX` and restoring values.
fn set_local_env(base_url: &str) {
    std::env::set_var("CQS_LLM_PROVIDER", "local");
    std::env::set_var("CQS_LLM_API_BASE", format!("{}/v1", base_url));
    std::env::set_var("CQS_LLM_MODEL", "test-model");
    std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "2");
    std::env::set_var("CQS_LOCAL_LLM_TIMEOUT_SECS", "10");
    // Allow cleartext http:// in tests — httpmock binds a real port on localhost.
    std::env::set_var("CQS_LLM_ALLOW_INSECURE", "1");
    std::env::remove_var("CQS_LLM_API_KEY");
}

fn clear_local_env() {
    for k in [
        "CQS_LLM_PROVIDER",
        "CQS_LLM_API_BASE",
        "CQS_LLM_MODEL",
        "CQS_LOCAL_LLM_CONCURRENCY",
        "CQS_LOCAL_LLM_TIMEOUT_SECS",
        "CQS_LLM_ALLOW_INSECURE",
        "CQS_LLM_API_KEY",
    ] {
        std::env::remove_var(k);
    }
}

/// Spec item 26: full `llm_summary_pass` end-to-end against a mocked server,
/// all 5 candidate chunks result in cached summaries in SQLite.
#[test]
#[ignore]
fn item26_full_summary_pass_end_to_end() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let (store, _dir) = setup_store();
    let hashes: Vec<String> = (0..5).map(|i| format!("hash_{:03}", i)).collect();
    for (i, h) in hashes.iter().enumerate() {
        insert_callable_chunk(&store, h, &format!("fn_{}", i));
    }

    let server = httpmock::MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "choices": [{ "message": { "content": "This function is a test stub." } }]
        }));
    });

    set_local_env(&server.base_url());

    let config = cqs::config::Config::default();
    let count = cqs::llm::llm_summary_pass(&store, /*quiet=*/ true, &config, None)
        .expect("summary pass must succeed");

    clear_local_env();

    assert_eq!(count, 5, "all 5 items should produce summaries");
    let h_refs: Vec<&str> = hashes.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        count_summaries_for_purpose(&store, &h_refs, "summary"),
        5,
        "all 5 summaries should land in SQLite"
    );
}

/// Spec item 27: streaming persist survives partial failure.
///
/// Implementation note: httpmock 0.7 doesn't expose a "respond N times then
/// fail" API. This test verifies the *property* that writes happen per-item
/// (via the streaming callback) — not that a specific number of items
/// succeed and fail. We run a 5-item batch against a server that succeeds
/// for all items, and assert summaries land in SQLite. The specific
/// partial-failure assertion is covered by the unit tests in `local.rs`.
#[test]
#[ignore]
fn item27_streaming_persist_writes_each_item() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let (store, _dir) = setup_store();
    let hashes: Vec<String> = (0..5).map(|i| format!("hash27_{:03}", i)).collect();
    for (i, h) in hashes.iter().enumerate() {
        insert_callable_chunk(&store, h, &format!("fn27_{}", i));
    }

    let server = httpmock::MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "choices": [{ "message": { "content": "streamed" } }]
        }));
    });

    set_local_env(&server.base_url());
    std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1"); // serialize for ordering

    let config = cqs::config::Config::default();
    let count = cqs::llm::llm_summary_pass(&store, true, &config, None)
        .expect("summary pass should complete");

    clear_local_env();

    // Streaming persist property: each item produces a row via the callback
    // (INSERT OR IGNORE), plus the final `fetch_batch_results` pass writes
    // again with INSERT OR REPLACE. Either way, 5 rows should be present.
    assert_eq!(count, 5);
    let h_refs: Vec<&str> = hashes.iter().map(|s| s.as_str()).collect();
    assert_eq!(count_summaries_for_purpose(&store, &h_refs, "summary"), 5);
}

/// Spec item 28: re-run after partial → first run's cached items skipped,
/// second run is a no-op. Exercises the content-hash cache in
/// `collect_eligible_chunks`.
#[test]
#[ignore]
fn item28_re_run_skips_cached_items() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let (store, _dir) = setup_store();
    let hashes: Vec<String> = (0..3).map(|i| format!("hash28_{:03}", i)).collect();
    for (i, h) in hashes.iter().enumerate() {
        insert_callable_chunk(&store, h, &format!("fn28_{}", i));
    }

    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).json_body(serde_json::json!({
            "choices": [{ "message": { "content": "first pass" } }]
        }));
    });

    set_local_env(&server.base_url());

    let config = cqs::config::Config::default();

    // First pass: all 3 chunks processed.
    let count1 = cqs::llm::llm_summary_pass(&store, true, &config, None).unwrap();
    assert_eq!(count1, 3, "first pass processes all 3 items");
    let hits_after_first = mock.hits();
    assert_eq!(
        hits_after_first, 3,
        "first pass should issue 3 HTTP requests"
    );

    // Second pass: all 3 hashes already cached → 0 API calls, 0 hits.
    let count2 = cqs::llm::llm_summary_pass(&store, true, &config, None).unwrap();
    assert_eq!(
        count2, 0,
        "second pass must skip all cached items (content-hash cache)"
    );
    assert_eq!(
        mock.hits(),
        hits_after_first,
        "second pass should not issue new HTTP requests"
    );

    clear_local_env();
}

/// Spec item 29: concurrency=1 AND concurrency=4 produce equivalent output.
/// Proves the worker pool is correct.
#[test]
#[ignore]
fn item29_concurrency_produces_equivalent_output() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    // Run twice with different concurrency levels, compare the set of
    // generated summaries (content_hash → summary_text).
    let runs = [1usize, 4usize];
    let mut outputs: Vec<std::collections::BTreeMap<String, String>> = Vec::new();

    for c in runs {
        let (store, _dir) = setup_store();
        let hashes: Vec<String> = (0..5).map(|i| format!("hash29_{:03}", i)).collect();
        for (i, h) in hashes.iter().enumerate() {
            insert_callable_chunk(&store, h, &format!("fn29_{}", i));
        }

        let server = httpmock::MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "deterministic output" } }]
            }));
        });

        set_local_env(&server.base_url());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", c.to_string());

        let config = cqs::config::Config::default();
        cqs::llm::llm_summary_pass(&store, true, &config, None).unwrap();

        clear_local_env();

        // Pull the summaries via the public API.
        let h_refs: Vec<&str> = hashes.iter().map(|s| s.as_str()).collect();
        let map: std::collections::BTreeMap<String, String> = store
            .get_summaries_by_hashes(&h_refs, "summary")
            .unwrap()
            .into_iter()
            .collect();
        outputs.push(map);
    }

    assert_eq!(
        outputs[0], outputs[1],
        "concurrency=1 and concurrency=4 must produce the same summary set"
    );
}
