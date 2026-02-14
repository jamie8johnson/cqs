//! Stress tests for cqs
//!
//! These tests verify behavior under heavy load and are marked `#[ignore]`
//! to avoid running during normal test cycles.
//!
//! Run with: `cargo test --test stress_test -- --ignored`

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use std::sync::Arc;
use std::thread;

/// Test storing and retrieving a large number of chunks
#[test]
#[ignore]
fn test_large_chunk_count() {
    let ts = TestStore::new();
    let chunk_count = 5_000;

    // Generate and insert chunks with unique content
    for i in 0..chunk_count {
        let chunk = test_chunk(
            &format!("func_{}", i),
            &format!("fn func_{}() {{ /* content {} */ }}", i, i),
        );
        let embedding = mock_embedding(i as f32 / chunk_count as f32);
        ts.upsert_chunk(&chunk, &embedding, Some(12345))
            .expect("Failed to upsert chunk");
    }

    // Verify count
    let stats = ts.stats().expect("Failed to get stats");
    assert_eq!(
        stats.total_chunks, chunk_count,
        "Should have all {} chunks",
        chunk_count
    );
}

/// Test concurrent search operations on shared store
#[test]
#[ignore]
fn test_concurrent_searches() {
    let ts = TestStore::new();

    // Setup: insert some test data
    let chunk_count = 500;
    for i in 0..chunk_count {
        let chunk = test_chunk(&format!("func_{}", i), &format!("fn func_{}() {{}}", i));
        let embedding = mock_embedding(i as f32 / chunk_count as f32);
        ts.upsert_chunk(&chunk, &embedding, Some(12345))
            .expect("Failed to upsert");
    }

    // Spawn multiple threads doing searches concurrently
    // Note: TestStore wraps Store which has internal connection pooling
    let store = Arc::new(ts);
    let thread_count = 4;
    let searches_per_thread = 50;

    let handles: Vec<_> = (0..thread_count)
        .map(|t| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                for i in 0..searches_per_thread {
                    let query = mock_embedding((t * searches_per_thread + i) as f32 / 1000.0);
                    let results = store.search_embedding_only(&query, 5, 0.0);
                    assert!(results.is_ok(), "Search should succeed");
                }
            })
        })
        .collect();

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test many small operations (worst case for connection pool)
#[test]
#[ignore]
fn test_many_small_operations() {
    let ts = TestStore::new();

    // Many small upserts
    for i in 0..500 {
        let chunk = test_chunk(&format!("func_{}", i), &format!("fn func_{}() {{}}", i));
        let embedding = mock_embedding(i as f32 / 500.0);
        ts.upsert_chunk(&chunk, &embedding, Some(12345))
            .expect("Failed to upsert");
    }

    // Verify all stored
    let stats = ts.stats().expect("Failed to get stats");
    assert_eq!(stats.total_chunks, 500);

    // Many small searches
    for i in 0..200 {
        let query = mock_embedding(i as f32 / 200.0);
        let results = ts
            .search_embedding_only(&query, 5, 0.0)
            .expect("Search failed");
        assert!(!results.is_empty(), "Should find results");
    }
}

/// Test search performance with varying thresholds
#[test]
#[ignore]
fn test_search_threshold_performance() {
    let ts = TestStore::new();

    // Insert chunks with varied embeddings
    for i in 0..1000 {
        let chunk = test_chunk(&format!("func_{}", i), &format!("fn func_{}() {{}}", i));
        // Use golden ratio for better distribution
        let seed = (i as f32 * 0.618_034) % 1.0;
        let embedding = mock_embedding(seed);
        ts.upsert_chunk(&chunk, &embedding, Some(12345))
            .expect("Failed to upsert");
    }

    // Search with different thresholds
    let query = mock_embedding(0.5);

    // Low threshold - should return more results
    let results_low = ts
        .search_embedding_only(&query, 100, 0.0)
        .expect("Search failed");
    assert!(
        !results_low.is_empty(),
        "Should find results with low threshold"
    );

    // High threshold - should return fewer results
    let results_high = ts
        .search_embedding_only(&query, 100, 0.8)
        .expect("Search failed");
    assert!(
        results_high.len() <= results_low.len(),
        "High threshold should return <= low threshold results"
    );
}

/// Test FTS search under load
#[test]
#[ignore]
fn test_fts_stress() {
    let ts = TestStore::new();

    // Insert chunks with varied names and content
    for i in 0..500 {
        let names = ["calculate", "process", "handle", "validate", "transform"];
        let name = format!("{}_{}", names[i % names.len()], i);
        let content = format!("fn {}() {{ // operation {} }}", name, i);
        let chunk = test_chunk(&name, &content);
        let embedding = mock_embedding(i as f32 / 500.0);
        ts.upsert_chunk(&chunk, &embedding, Some(12345))
            .expect("Failed to upsert");
    }

    // Run multiple FTS searches
    let queries = ["calculate", "process", "handle", "validate", "transform"];
    for query in &queries {
        let results = ts.search_fts(query, 100).expect("FTS search failed");
        assert!(!results.is_empty(), "Should find results for '{}'", query);
    }
}
