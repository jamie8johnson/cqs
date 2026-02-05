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
