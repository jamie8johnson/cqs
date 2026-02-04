//! Common test fixtures and helpers
//!
//! Usage in test files:
//! ```ignore
//! mod common;
//! use common::TestStore;
//! ```

use cqs::embedder::Embedding;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::store::{ModelInfo, Store};
use std::path::PathBuf;
use tempfile::TempDir;

/// Test store with automatic cleanup
///
/// Wraps a `Store` with its backing `TempDir`, ensuring the directory
/// lives as long as the store is in use.
pub struct TestStore {
    /// The store instance
    pub store: Store,
    /// Temp directory (kept alive to prevent cleanup)
    _dir: TempDir,
}

impl TestStore {
    /// Create an initialized test store in a temporary directory
    pub fn new() -> Self {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).expect("Failed to open store");
        store.init(&ModelInfo::default()).expect("Failed to init store");
        Self { store, _dir: dir }
    }

    /// Create a test store with custom model info
    pub fn with_model(model: &ModelInfo) -> Self {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).expect("Failed to open store");
        store.init(model).expect("Failed to init store");
        Self { store, _dir: dir }
    }
}

impl std::ops::Deref for TestStore {
    type Target = Store;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

/// Create a test chunk with sensible defaults
pub fn test_chunk(name: &str, content: &str) -> Chunk {
    let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    Chunk {
        id: format!("test.rs:1:{}", &hash[..8]),
        file: PathBuf::from("test.rs"),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: content.to_string(),
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash: hash,
        parent_id: None,
        window_idx: None,
    }
}

/// Create a mock 769-dim embedding (768 model + 1 sentiment)
///
/// The seed value determines the direction of the embedding vector.
/// Same seed = same direction = high similarity.
/// Different seeds = different directions = lower similarity.
pub fn mock_embedding(seed: f32) -> Embedding {
    let mut v = vec![seed; 768];
    // Normalize to unit length
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    // Add sentiment dimension (769th)
    v.push(0.0);
    Embedding::new(v)
}
