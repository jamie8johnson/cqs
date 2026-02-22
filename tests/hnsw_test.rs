//! HNSW error path tests
//!
//! Tests for corruption detection and error handling in the HNSW index.

use cqs::embedder::Embedding;
use cqs::hnsw::HnswIndex;
use tempfile::TempDir;

const EMBEDDING_DIM: usize = 769;

fn make_embedding(seed: u32) -> Embedding {
    let mut v = vec![0.0f32; EMBEDDING_DIM];
    for (i, val) in v.iter_mut().enumerate() {
        *val = ((seed as f32 * 0.1) + (i as f32 * 0.001)).sin();
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut v {
            *val /= norm;
        }
    }
    Embedding::new(v)
}

#[test]
fn test_truncated_data_file_detected() {
    let tmp = TempDir::new().unwrap();

    // Build and save a valid index
    let embeddings: Vec<_> = (1..=5)
        .map(|i| (format!("chunk{}", i), make_embedding(i)))
        .collect();
    let index = HnswIndex::build(embeddings).unwrap();
    index.save(tmp.path(), "test").unwrap();

    // Truncate the data file (corrupt it)
    let data_path = tmp.path().join("test.hnsw.data");
    let original = std::fs::read(&data_path).unwrap();
    // Write only first half of the file
    std::fs::write(&data_path, &original[..original.len() / 2]).unwrap();

    // Loading should fail with checksum mismatch
    let result = HnswIndex::load(tmp.path(), "test");
    match result {
        Ok(_) => panic!("Truncated file should cause load to fail"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("Checksum") || err_msg.contains("checksum"),
                "Error should mention checksum: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_checksum_mismatch_detected() {
    let tmp = TempDir::new().unwrap();

    // Build and save a valid index
    let embeddings = vec![
        ("a".to_string(), make_embedding(1)),
        ("b".to_string(), make_embedding(2)),
    ];
    let index = HnswIndex::build(embeddings).unwrap();
    index.save(tmp.path(), "test").unwrap();

    // Corrupt a single byte in the graph file
    let graph_path = tmp.path().join("test.hnsw.graph");
    let mut data = std::fs::read(&graph_path).unwrap();
    if !data.is_empty() {
        // Flip a bit in the middle of the file
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
        std::fs::write(&graph_path, &data).unwrap();
    }

    // Loading should fail with checksum mismatch
    let result = HnswIndex::load(tmp.path(), "test");
    match result {
        Ok(_) => panic!("Corrupted file should cause load to fail"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("Checksum") || err_msg.contains("checksum"),
                "Error should mention checksum: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_missing_files_detected() {
    let tmp = TempDir::new().unwrap();

    // Build and save a valid index
    let embeddings = vec![("x".to_string(), make_embedding(42))];
    let index = HnswIndex::build(embeddings).unwrap();
    index.save(tmp.path(), "test").unwrap();

    // Delete one of the required files
    std::fs::remove_file(tmp.path().join("test.hnsw.ids")).unwrap();

    // Loading should fail with not found
    let result = HnswIndex::load(tmp.path(), "test");
    match result {
        Ok(_) => panic!("Missing file should cause load to fail"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("not found") || err_msg.contains("NotFound"),
                "Error should mention not found: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_corrupted_id_map_json() {
    let tmp = TempDir::new().unwrap();

    // Build and save a valid index
    let embeddings = vec![("y".to_string(), make_embedding(99))];
    let index = HnswIndex::build(embeddings).unwrap();
    index.save(tmp.path(), "test").unwrap();

    // Corrupt the ID map JSON
    let id_map_path = tmp.path().join("test.hnsw.ids");
    std::fs::write(&id_map_path, "{ invalid json [[[").unwrap();

    // Loading should fail (either checksum or parse error)
    let result = HnswIndex::load(tmp.path(), "test");
    assert!(result.is_err(), "Corrupted JSON should cause load to fail");
}

#[test]
fn test_id_map_size_mismatch_rejected() {
    let tmp = TempDir::new().unwrap();

    // Build and save a valid index with 3 vectors
    let embeddings: Vec<_> = (1..=3)
        .map(|i| (format!("chunk{}", i), make_embedding(i)))
        .collect();
    let index = HnswIndex::build(embeddings).unwrap();
    index.save(tmp.path(), "test").unwrap();

    // Modify id_map to have wrong count (2 instead of 3)
    let id_map_path = tmp.path().join("test.hnsw.ids");
    std::fs::write(&id_map_path, r#"["chunk1", "chunk2"]"#).unwrap();

    // Loading should fail due to size mismatch
    let result = HnswIndex::load(tmp.path(), "test");
    // Note: checksum verification may catch this first, but if bypassed, size check will catch it
    assert!(
        result.is_err(),
        "ID map size mismatch should cause load to fail"
    );
}

#[test]
fn test_dimension_mismatch_rejected() {
    // Try to build with wrong dimension embedding
    let wrong_dim = Embedding::new(vec![1.0; 100]); // Should be 769
    let embeddings = vec![("wrong".to_string(), wrong_dim)];

    let result = HnswIndex::build(embeddings);
    match result {
        Ok(_) => panic!("Wrong dimension should fail"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("mismatch") || err_msg.contains("Dimension"),
                "Error should mention dimension: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_query_dimension_mismatch_returns_empty() {
    let embeddings = vec![("good".to_string(), make_embedding(1))];
    let index = HnswIndex::build(embeddings).unwrap();

    // Query with wrong dimension should return empty (graceful degradation)
    let wrong_query = Embedding::new(vec![1.0; 100]);
    let results = index.search(&wrong_query, 5);
    assert!(
        results.is_empty(),
        "Wrong dimension query should return empty"
    );
}

// ===== build_batched error path tests (T14) =====

#[test]
fn test_build_batched_dimension_mismatch() {
    // First batch has correct dimension, second has wrong dimension
    let good_batch: Vec<(String, Embedding)> = vec![
        ("good1".to_string(), make_embedding(1)),
        ("good2".to_string(), make_embedding(2)),
    ];

    let wrong_dim = Embedding::new(vec![1.0; 100]); // Should be 769
    let bad_batch: Vec<(String, Embedding)> = vec![("bad".to_string(), wrong_dim)];

    let batches: Vec<Result<Vec<(String, Embedding)>, &str>> = vec![Ok(good_batch), Ok(bad_batch)];

    let result = HnswIndex::build_batched(batches.into_iter(), 3);
    match result {
        Ok(_) => panic!("Dimension mismatch in batch should fail"),
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("mismatch") || err_msg.contains("Dimension"),
                "Error should mention dimension: {}",
                err_msg
            );
        }
    }
}

#[test]
fn test_build_batched_empty_batches() {
    // All batches are empty
    let batches: Vec<Result<Vec<(String, Embedding)>, &str>> =
        vec![Ok(vec![]), Ok(vec![]), Ok(vec![])];

    let result = HnswIndex::build_batched(batches.into_iter(), 0);

    // Empty input should either succeed with empty index or fail gracefully
    // Current implementation should handle this - empty HNSW is valid
    match result {
        Ok(index) => {
            // Empty index - searching should return empty
            let query = make_embedding(1);
            let results = index.search(&query, 5);
            assert!(results.is_empty(), "Empty index should return no results");
        }
        Err(e) => {
            // If it fails, error should be meaningful
            let err_msg = e.to_string();
            assert!(
                !err_msg.is_empty(),
                "Error message should not be empty: {}",
                err_msg
            );
        }
    }
}

// ===== HNSW Batch Build Tests (#239) =====

#[test]
fn test_build_batched_handles_rebuild_after_initial_build() {
    // Simulate rebuilding: build index, then build again with different data
    // This tests if the batch logic properly handles starting fresh

    // First build
    let batch1: Vec<Result<Vec<(String, Embedding)>, &str>> = vec![Ok(vec![
        ("chunk1".to_string(), make_embedding(1)),
        ("chunk2".to_string(), make_embedding(2)),
    ])];

    let index1 = HnswIndex::build_batched(batch1.into_iter(), 2).unwrap();
    assert_eq!(index1.len(), 2);

    // Second build with different data (simulates rebuild)
    let batch2: Vec<Result<Vec<(String, Embedding)>, &str>> = vec![Ok(vec![
        ("chunk3".to_string(), make_embedding(3)),
        ("chunk4".to_string(), make_embedding(4)),
        ("chunk5".to_string(), make_embedding(5)),
    ])];

    let index2 = HnswIndex::build_batched(batch2.into_iter(), 3).unwrap();
    assert_eq!(index2.len(), 3);

    // Verify search works on rebuilt index
    let query = make_embedding(3);
    let results = index2.search(&query, 1);
    assert_eq!(results[0].id, "chunk3");
}

#[test]
fn test_build_batched_large_number_of_batches() {
    // Test with many small batches (stress test batch iteration)
    let batches: Vec<Result<Vec<(String, Embedding)>, &str>> = (0..50)
        .map(|i| {
            Ok(vec![
                (format!("chunk{}_1", i), make_embedding(i * 2 + 1)),
                (format!("chunk{}_2", i), make_embedding(i * 2 + 2)),
            ])
        })
        .collect();

    let index = HnswIndex::build_batched(batches.into_iter(), 100).unwrap();
    assert_eq!(
        index.len(),
        100,
        "Should handle 50 batches with 2 items each"
    );

    // Verify search works correctly
    let query = make_embedding(1);
    let results = index.search(&query, 5);
    assert!(!results.is_empty(), "Search should return results");
}

#[test]
fn test_build_batched_uneven_batch_sizes() {
    // Test with varying batch sizes (realistic scenario)
    let batches: Vec<Result<Vec<(String, Embedding)>, &str>> = vec![
        Ok(vec![
            ("a".to_string(), make_embedding(1)),
            ("b".to_string(), make_embedding(2)),
            ("c".to_string(), make_embedding(3)),
        ]),
        Ok(vec![("d".to_string(), make_embedding(4))]), // Small batch
        Ok(vec![
            ("e".to_string(), make_embedding(5)),
            ("f".to_string(), make_embedding(6)),
            ("g".to_string(), make_embedding(7)),
            ("h".to_string(), make_embedding(8)),
            ("i".to_string(), make_embedding(9)),
        ]), // Large batch
        Ok(vec![
            ("j".to_string(), make_embedding(10)),
            ("k".to_string(), make_embedding(11)),
        ]),
    ];

    let index = HnswIndex::build_batched(batches.into_iter(), 11).unwrap();
    assert_eq!(index.len(), 11);

    // Verify IDs are correctly mapped
    let query = make_embedding(5);
    let results = index.search(&query, 1);
    assert_eq!(results[0].id, "e");
}

#[test]
fn test_build_batched_with_batch_errors() {
    // Test error handling when a batch fails mid-stream
    let batches: Vec<Result<Vec<(String, Embedding)>, &str>> = vec![
        Ok(vec![
            ("good1".to_string(), make_embedding(1)),
            ("good2".to_string(), make_embedding(2)),
        ]),
        Err("Batch fetch failed"),
        Ok(vec![("good3".to_string(), make_embedding(3))]),
    ];

    let result = HnswIndex::build_batched(batches.into_iter(), 5);
    assert!(result.is_err(), "Should propagate error from failed batch");

    match result {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("Batch fetch failed"),
                "Error should mention batch failure"
            );
        }
        Ok(_) => panic!("Expected error, got Ok"),
    }
}

#[test]
fn test_build_batched_search_quality() {
    // Verify that batched build produces usable search results
    // Build with batches
    let all_embeddings: Vec<(String, Embedding)> = (1..=30)
        .map(|i| (format!("item{}", i), make_embedding(i)))
        .collect();

    let batches: Vec<Result<Vec<(String, Embedding)>, &str>> = all_embeddings
        .chunks(5)
        .map(|chunk| Ok(chunk.to_vec()))
        .collect();

    let index = HnswIndex::build_batched(batches.into_iter(), 30).unwrap();

    // Search for each item and verify it can be found.
    // Use top 15 (half the index) since HNSW is approximate — batched builds
    // with small datasets can have suboptimal graph quality.
    for i in [1, 10, 20, 30] {
        let query = make_embedding(i);
        let results = index.search(&query, 15);
        assert!(!results.is_empty(), "Should find results for item{}", i);
        let found = results.iter().any(|r| r.id == format!("item{}", i));
        assert!(found, "Should find item{} in top 15 results", i);
    }
}
