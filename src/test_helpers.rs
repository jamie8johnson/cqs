//! Shared test fixtures for cqs unit tests.

use crate::embedder::Embedding;
use crate::store::helpers::ModelInfo;
use crate::Store;
use tempfile::TempDir;

/// Create a temporary Store for testing.
pub fn setup_store() -> (Store, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();
    (store, dir)
}

/// Create a deterministic EMBEDDING_DIM embedding from a seed value, L2-normalized.
/// Fills the vector with `seed` repeated, then normalizes. This makes embeddings
/// distinguishable by seed while keeping consistent magnitude.
pub fn mock_embedding(seed: f32) -> Embedding {
    let mut v = vec![seed; crate::EMBEDDING_DIM];
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding::new(v)
}
