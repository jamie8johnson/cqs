//! Embedding pipeline integration tests
//!
//! Tests for `embed_documents` and `embed_query` that require the ONNX model.
//! Run with: cargo test --features gpu-index --test embedding_test -- --ignored

use cqs::embedder::{Embedder, EmbedderError};
use cqs::EMBEDDING_DIM;

/// Create a CPU embedder (avoids GPU context overhead for these tests)
fn cpu_embedder() -> Embedder {
    Embedder::new_cpu().expect("Failed to create CPU embedder")
}

#[test]
#[ignore] // Requires ONNX model
fn test_embed_single_document() {
    let embedder = cpu_embedder();
    let results = embedder
        .embed_documents(&["fn main() { println!(\"hello\"); }"])
        .expect("embed_documents failed");

    assert_eq!(results.len(), 1);
    // embed_documents returns 768-dim (no sentiment appended)
    assert_eq!(results[0].len(), 768);

    // Should be L2-normalized (magnitude ≈ 1.0)
    let magnitude: f32 = results[0]
        .as_slice()
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt();
    assert!(
        (magnitude - 1.0).abs() < 1e-4,
        "Expected unit vector, got magnitude {}",
        magnitude
    );
}

#[test]
#[ignore]
fn test_embed_batch_documents() {
    let embedder = cpu_embedder();
    let docs = vec![
        "fn add(a: i32, b: i32) -> i32 { a + b }",
        "def multiply(x, y): return x * y",
        "function divide(a, b) { return a / b; }",
        "public static int subtract(int a, int b) { return a - b; }",
        "SELECT * FROM users WHERE id = 1",
    ];
    let results = embedder
        .embed_documents(&docs)
        .expect("embed_documents batch failed");

    assert_eq!(results.len(), 5);
    for (i, emb) in results.iter().enumerate() {
        assert_eq!(emb.len(), 768, "Document {} has wrong dimension", i);
        let magnitude: f32 = emb.as_slice().iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (magnitude - 1.0).abs() < 1e-4,
            "Document {} not normalized: magnitude {}",
            i,
            magnitude
        );
    }
}

#[test]
#[ignore]
fn test_embed_empty_batch() {
    let embedder = cpu_embedder();
    let results = embedder
        .embed_documents(&[])
        .expect("embed_documents empty failed");
    assert!(results.is_empty());
}

#[test]
#[ignore]
fn test_embed_deterministic() {
    let embedder = cpu_embedder();
    let text = "pub fn process(data: &[u8]) -> Vec<u8>";

    let result1 = embedder
        .embed_documents(&[text])
        .expect("first embed failed");
    let result2 = embedder
        .embed_documents(&[text])
        .expect("second embed failed");

    assert_eq!(result1[0].as_slice(), result2[0].as_slice());
}

#[test]
#[ignore]
fn test_query_vs_document_differ() {
    let embedder = cpu_embedder();
    let text = "parse configuration file";

    let doc = embedder
        .embed_documents(&[text])
        .expect("embed_documents failed");
    let query = embedder.embed_query(text).expect("embed_query failed");

    // E5 uses "passage: " prefix for documents and "query: " for queries
    // So the embeddings should differ
    assert_ne!(
        doc[0].as_slice(),
        &query.as_slice()[..768],
        "Query and document embeddings should differ due to E5 prefix"
    );
}

#[test]
#[ignore]
fn test_embed_query_has_sentiment_dim() {
    let embedder = cpu_embedder();
    let query = embedder
        .embed_query("search for functions")
        .expect("embed_query failed");

    // embed_query appends sentiment (0.0) as 769th dim
    assert_eq!(query.len(), EMBEDDING_DIM);
    assert_eq!(query.sentiment(), Some(0.0));
}

#[test]
#[ignore]
fn test_embed_query_empty_rejected() {
    let embedder = cpu_embedder();
    let err = embedder.embed_query("").unwrap_err();
    assert!(matches!(err, EmbedderError::EmptyQuery));
}

#[test]
#[ignore]
fn test_embed_query_whitespace_only_rejected() {
    let embedder = cpu_embedder();
    let err = embedder.embed_query("   \t\n  ").unwrap_err();
    assert!(matches!(err, EmbedderError::EmptyQuery));
}

#[test]
#[ignore]
fn test_embed_query_cached() {
    let embedder = cpu_embedder();
    let text = "test caching behavior";

    // First call — cache miss
    let result1 = embedder.embed_query(text).expect("first query failed");
    // Second call — cache hit (should return identical result)
    let result2 = embedder.embed_query(text).expect("second query failed");

    assert_eq!(result1.as_slice(), result2.as_slice());
}
