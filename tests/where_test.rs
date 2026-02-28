//! Tests for suggest_placement (TC-6)
//!
//! Integration test that seeds a Store with real chunks and verifies
//! suggest_placement returns meaningful results.

#[allow(unused)]
mod common;

use common::TestStore;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::suggest_placement;
use cqs::Embedder;
use std::path::PathBuf;

/// Create a chunk with a specific file, name, and content
fn placement_chunk(name: &str, file: &str, content: &str, line_start: u32) -> Chunk {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("{}:{}:{}", file, line_start, &hash[..8]),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: content.to_string(),
        doc: None,
        line_start,
        line_end: line_start + 5,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
    }
}

#[test]
fn test_suggest_placement_returns_results_for_similar_code() {
    let store = TestStore::new();
    let embedder = Embedder::new().unwrap();

    // Seed store with chunks from multiple files that have known themes
    let chunks = vec![
        placement_chunk(
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
        ),
    ];

    // Embed each chunk with the real embedder for realistic similarity.
    // embed_documents returns 768-dim; add neutral sentiment for 769-dim.
    let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_documents(&contents).unwrap();
    let pairs: Vec<_> = chunks
        .iter()
        .cloned()
        .zip(embeddings.into_iter().map(|e| e.with_sentiment(0.0)))
        .collect();
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
