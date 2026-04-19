//! Tests for suggest_placement (TC-6)
//!
//! Integration test that seeds a Store with real chunks and verifies
//! suggest_placement returns meaningful results.

#[allow(unused)]
mod common;

use common::TestStore;
use cqs::embedder::ModelConfig;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::Embedder;
use cqs::PlacementOptions;
use cqs::{suggest_placement, suggest_placement_with_options};
use std::path::PathBuf;

/// Create a chunk with a specific file, name, and content (defaults to Rust)
fn placement_chunk(name: &str, file: &str, content: &str, line_start: u32) -> Chunk {
    placement_chunk_with_lang(name, file, content, line_start, Language::Rust)
}

/// Create a chunk with an explicit language (for cross-language placement tests)
fn placement_chunk_with_lang(
    name: &str,
    file: &str,
    content: &str,
    line_start: u32,
    language: Language,
) -> Chunk {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let signature = match language {
        Language::Python => format!("def {}()", name),
        _ => format!("fn {}()", name),
    };
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature,
        content: content.to_string(),
        doc: None,
        line_start,
        line_end: line_start + 5,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
        parser_version: 0,
    }
}

#[test]
fn test_suggest_placement_returns_results_for_similar_code() {
    let store = TestStore::new();
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).unwrap();

    // Seed store with chunks from multiple files that have known themes
    let chunks = [placement_chunk(
            "parse_config",
            "src/config.rs",
            "fn parse_config(path: &Path) -> Result<Config, Error> { let data = std::fs::read_to_string(path)?; toml::from_str(&data) }",
            1,
        ),
        placement_chunk(
            "validate_config",
            "src/config.rs",
            "fn validate_config(cfg: &Config) -> Result<(), ValidationError> { if cfg.name.is_empty() { return Err(ValidationError::MissingName); } Ok(()) }",
            10,
        ),
        placement_chunk(
            "render_page",
            "src/render.rs",
            "fn render_page(template: &str, data: &Context) -> String { handlebars.render(template, data).unwrap() }",
            1,
        ),
        placement_chunk(
            "handle_request",
            "src/server.rs",
            "fn handle_request(req: Request) -> Response { let body = process(req.body()); Response::ok(body) }",
            1,
        )];

    // Embed each chunk with the real embedder for realistic similarity.
    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_documents(&contents).unwrap();
    let pairs: Vec<_> = chunks.iter().cloned().zip(embeddings).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();

    // Ask for placement of a config-related function
    let result = suggest_placement(&store, &embedder, "load configuration from file", 3).unwrap();

    // PlacementResult should be non-empty — config.rs should rank high
    assert!(
        !result.suggestions.is_empty(),
        "suggest_placement should return at least one suggestion for a seeded store"
    );

    // The top suggestion should reference config.rs (most similar to "load configuration")
    let top_file = result.suggestions[0].file.to_string_lossy().to_string();
    assert!(
        top_file.contains("config"),
        "Top suggestion should be config.rs for config-related query, got: {}",
        top_file
    );
}

#[test]
fn test_suggest_placement_with_options_reuses_embedding() {
    let store = TestStore::new();
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).unwrap();

    let chunks = [placement_chunk(
            "save_data",
            "src/storage.rs",
            "fn save_data(db: &Database, key: &str, value: &[u8]) -> Result<()> { db.put(key, value) }",
            1,
        ),
        placement_chunk(
            "load_data",
            "src/storage.rs",
            "fn load_data(db: &Database, key: &str) -> Result<Vec<u8>> { db.get(key).ok_or(NotFound) }",
            10,
        )];

    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_documents(&contents).unwrap();
    let pairs: Vec<_> = chunks.iter().cloned().zip(embeddings).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();

    // Pre-compute embedding and pass via options (avoids redundant inference)
    let query = "persist data to database";
    let query_embedding = embedder.embed_query(query).unwrap();
    let opts = PlacementOptions {
        query_embedding: Some(query_embedding),
        ..Default::default()
    };

    let result = suggest_placement_with_options(&store, &embedder, query, 3, &opts).unwrap();
    assert!(
        !result.suggestions.is_empty(),
        "suggest_placement_with_options should return results with pre-computed embedding"
    );
    assert!(
        result.suggestions[0]
            .file
            .to_string_lossy()
            .contains("storage"),
        "Should suggest storage.rs for database query"
    );
}

/// Cross-language placement: seed both Python and Rust chunks with similar
/// semantic themes; query with Python-idiomatic content (the `placement_chunk`
/// helper sets the `Language` tag AND file extension per chunk). The
/// placement ranking is semantic; Python chunks written with Python syntax
/// should rank above the Rust chunks for a Python-idiom query.
///
/// Issue #974 acceptance text calls for `PlacementOptions { language: Some(...), .. }`,
/// but `PlacementOptions` does not currently expose a `language` field. This
/// test exercises the de-facto language-correct behavior instead: it catches
/// the same regression class (top-of-list leaking to the wrong language when
/// ranking breaks) without requiring an API change this PR is not scoped to make.
#[test]
fn test_suggest_placement_respects_language_filter() {
    let store = TestStore::new();
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).unwrap();

    let chunks = [
        placement_chunk_with_lang(
            "load_yaml",
            "src/loaders.py",
            "def load_yaml(path):\n    with open(path) as f:\n        return yaml.safe_load(f)\n",
            1,
            Language::Python,
        ),
        placement_chunk_with_lang(
            "parse_toml_file",
            "src/loaders.py",
            "def parse_toml_file(path):\n    with open(path, 'rb') as f:\n        return tomllib.load(f)\n",
            10,
            Language::Python,
        ),
        placement_chunk_with_lang(
            "read_json_config",
            "src/config_loader.py",
            "def read_json_config(path):\n    with open(path) as f:\n        return json.load(f)\n",
            1,
            Language::Python,
        ),
        placement_chunk_with_lang(
            "read_bytes",
            "src/bytes.rs",
            "fn read_bytes(path: &Path) -> Vec<u8> { std::fs::read(path).unwrap() }",
            1,
            Language::Rust,
        ),
        placement_chunk_with_lang(
            "spawn_thread",
            "src/thread_pool.rs",
            "fn spawn_thread(f: impl FnOnce() + Send + 'static) { std::thread::spawn(f); }",
            1,
            Language::Rust,
        ),
    ];

    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_documents(&contents).unwrap();
    let pairs: Vec<_> = chunks.iter().cloned().zip(embeddings).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();

    // Python-idiomatic query: "with open ... yaml safe_load" is distinctly Python.
    let result = suggest_placement(
        &store,
        &embedder,
        "python function that opens a file with 'with open' and loads yaml data",
        5,
    )
    .unwrap();

    assert!(
        !result.suggestions.is_empty(),
        "expected at least one suggestion for Python-idiomatic query"
    );

    // The top suggestion should be a .py file — Python chunks are semantically
    // closer to the query than the Rust chunks seeded alongside.
    let top_file = result.suggestions[0].file.to_string_lossy().to_string();
    assert!(
        top_file.ends_with(".py"),
        "top suggestion for Python-idiomatic query should be a .py file, got: {}",
        top_file
    );
}

/// Empty store must return an empty suggestion list without panicking.
/// Regression guard for the "no chunks indexed yet" boundary condition.
#[test]
fn test_suggest_placement_empty_store_returns_empty() {
    let store = TestStore::new();
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).unwrap();

    let result = suggest_placement(&store, &embedder, "anything at all", 3)
        .expect("empty store should return Ok, not Err");

    assert!(
        result.suggestions.is_empty(),
        "empty store should yield empty suggestions, got: {:?}",
        result
            .suggestions
            .iter()
            .map(|s| s.file.clone())
            .collect::<Vec<_>>()
    );
}

/// A query that is semantically far from every seeded chunk should score low.
/// If the top aggregate score is >= 0.5 for an unrelated query, ranking is
/// returning false positives — exactly the regression class we want to catch.
#[test]
fn test_suggest_placement_dissimilar_query_scores_low() {
    let store = TestStore::new();
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).unwrap();

    // Seed Python data-processing chunks that share no vocabulary with "cryptography".
    let chunks = [
        placement_chunk_with_lang(
            "aggregate_rows",
            "src/aggregator.py",
            "def aggregate_rows(rows):\n    return sum(r['value'] for r in rows)\n",
            1,
            Language::Python,
        ),
        placement_chunk_with_lang(
            "normalize_record",
            "src/normalize.py",
            "def normalize_record(rec):\n    return {k.lower(): v.strip() for k, v in rec.items()}\n",
            1,
            Language::Python,
        ),
        placement_chunk_with_lang(
            "deduplicate",
            "src/dedup.py",
            "def deduplicate(items):\n    return list(dict.fromkeys(items))\n",
            1,
            Language::Python,
        ),
    ];

    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_documents(&contents).unwrap();
    let pairs: Vec<_> = chunks.iter().cloned().zip(embeddings).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();

    // Ask for something completely unrelated.
    let result = suggest_placement(
        &store,
        &embedder,
        "elliptic curve cryptography key exchange",
        3,
    )
    .expect("suggest_placement should not error on a dissimilar query");

    // If the seed threshold filtered everything out, that's fine — the assertion
    // only applies when something came back.
    if let Some(top) = result.suggestions.first() {
        assert!(
            top.score < 0.5,
            "dissimilar query should score < 0.5, got {} for {}",
            top.score,
            top.file.to_string_lossy()
        );
    }
}

/// `limit` must cap the number of returned suggestions. Seed more than `limit`
/// files of semantically-aligned chunks, request limit=3, expect exactly 3.
#[test]
fn test_suggest_placement_limit_honored() {
    let store = TestStore::new();
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).unwrap();

    // Seed 6 distinct files, all about HTTP handling so every chunk is similar
    // to the query and will survive the search threshold.
    let chunks = [
        placement_chunk(
            "handle_get",
            "src/routes/get.rs",
            "fn handle_get(req: Request) -> Response { Response::ok(req.path().to_string()) }",
            1,
        ),
        placement_chunk(
            "handle_post",
            "src/routes/post.rs",
            "fn handle_post(req: Request) -> Response { let body = req.body(); Response::ok(body) }",
            1,
        ),
        placement_chunk(
            "handle_put",
            "src/routes/put.rs",
            "fn handle_put(req: Request) -> Response { let body = req.body(); Response::ok(body) }",
            1,
        ),
        placement_chunk(
            "handle_delete",
            "src/routes/delete.rs",
            "fn handle_delete(req: Request) -> Response { Response::ok(String::new()) }",
            1,
        ),
        placement_chunk(
            "route_request",
            "src/router.rs",
            "fn route_request(req: Request) -> Response { match req.method() { Method::Get => handle_get(req), _ => Response::not_found() } }",
            1,
        ),
        placement_chunk(
            "build_response",
            "src/response_builder.rs",
            "fn build_response(status: u16, body: String) -> Response { Response::new(status, body) }",
            1,
        ),
    ];

    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_documents(&contents).unwrap();
    let pairs: Vec<_> = chunks.iter().cloned().zip(embeddings).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();

    let result = suggest_placement(&store, &embedder, "http request handler function", 3)
        .expect("suggest_placement should succeed");

    assert_eq!(
        result.suggestions.len(),
        3,
        "limit=3 should cap suggestions at 3 files, got {}: {:?}",
        result.suggestions.len(),
        result
            .suggestions
            .iter()
            .map(|s| s.file.to_string_lossy().to_string())
            .collect::<Vec<_>>()
    );
}
