//! HNSW (Hierarchical Navigable Small World) index for fast vector search
//!
//! Provides O(log n) approximate nearest neighbor search, scaling to >50k chunks.
//!
//! ## Security
//!
//! The underlying hnsw_rs library uses bincode for serialization, which is
//! unmaintained (RUSTSEC-2025-0141). To mitigate deserialization risks, we
//! compute and verify blake3 checksums on save/load.
//!
//! ## Memory Management
//!
//! When loading an index from disk, hnsw_rs returns `Hnsw<'a>` borrowing from
//! `HnswIo`. We use `LoadedHnsw` to manage this self-referential pattern:
//! - HnswIo is heap-allocated, we hold a raw pointer
//! - Hnsw lifetime is transmuted to 'static (safe because HnswIo outlives it)
//! - Custom Drop ensures HnswIo is freed after Hnsw is dropped
//!
//! This avoids memory leaks while keeping the loaded index usable.
//!
//! ## CRITICAL: hnsw_rs Version Dependency
//!
//! The `LoadedHnsw` struct uses `std::mem::transmute` to extend a borrowed
//! lifetime. This is sound ONLY because:
//!
//! 1. `HnswIo::load_hnsw()` returns `Hnsw<'a>` borrowing from `&'a mut HnswIo`
//! 2. The `Hnsw` only reads data owned by `HnswIo` (no interior mutation)
//! 3. We control drop order via `ManuallyDrop` (Hnsw dropped before HnswIo)
//!
//! **If upgrading hnsw_rs**: Run `cargo test safety_tests` and verify behavior.
//! Breaking changes to `HnswIo::load_hnsw()` or `Hnsw`'s borrowing could cause UB.
//! Current tested version: hnsw_rs 0.3.x

mod build;
mod persist;
mod safety;
mod search;

use std::mem::ManuallyDrop;

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use thiserror::Error;

use crate::embedder::Embedding;
use crate::index::{IndexResult, VectorIndex};

// HNSW tuning parameters
//
// These values are optimized for code search workloads (10k-100k chunks):
// - M=24: Higher connectivity for better recall on semantic similarity
// - ef_construction=200: Thorough graph construction (one-time cost)
// - ef_search=100: Good accuracy/speed tradeoff for interactive search
//
// For different workloads, consider:
// - Smaller codebases (<5k): M=16, ef_construction=100, ef_search=50
// - Larger codebases (>100k): M=32, ef_construction=400, ef_search=200
// - Batch processing: Lower ef_search for speed
// - Maximum accuracy: Higher ef_search (up to ef_construction)
pub(crate) const MAX_NB_CONNECTION: usize = 24; // M parameter - connections per node
pub(crate) const MAX_LAYER: usize = 16; // Maximum layers in the graph
pub(crate) const EF_CONSTRUCTION: usize = 200; // Construction-time search width

/// Search width for queries (higher = more accurate but slower)
pub(crate) const EF_SEARCH: usize = 100;

#[derive(Error, Debug)]
pub enum HnswError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HNSW index not found at {0}")]
    NotFound(String),
    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("HNSW error: {0}")]
    Internal(String),
    #[error(
        "Checksum mismatch for {file}: expected {expected}, got {actual}. Index may be corrupted."
    )]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
}

// Note: Uses crate::index::IndexResult instead of a separate HnswResult type
// since they have identical structure (id: String, score: f32)

/// Self-referential wrapper for loaded HNSW
///
/// HnswIo owns the data, Hnsw borrows from it. We manage lifetimes manually:
/// - HnswIo is heap-allocated and we hold a raw pointer
/// - Hnsw is in ManuallyDrop so we control drop order
/// - Drop impl: drop Hnsw first, then free HnswIo
pub(crate) struct LoadedHnsw {
    /// Raw pointer to HnswIo - we own this memory
    pub(crate) io_ptr: *mut HnswIo,
    /// Hnsw borrowing from io_ptr (transmuted to 'static, manually dropped)
    pub(crate) hnsw: ManuallyDrop<Hnsw<'static, f32, DistCosine>>,
}

impl Drop for LoadedHnsw {
    fn drop(&mut self) {
        // SAFETY: We control drop order - Hnsw first, then HnswIo
        // 1. Drop Hnsw while HnswIo data is still valid
        // 2. Then free HnswIo
        unsafe {
            ManuallyDrop::drop(&mut self.hnsw);
            drop(Box::from_raw(self.io_ptr));
        }
    }
}

// SAFETY: LoadedHnsw is Send+Sync because:
// - io_ptr points to HnswIo which only contains file paths and data buffers
// - Hnsw<f32, DistCosine> contains data structures that are inherently thread-safe
// - All mutable access is protected by external synchronization (RwLock in HnswIndex)
unsafe impl Send for LoadedHnsw {}
unsafe impl Sync for LoadedHnsw {}

/// HNSW index wrapper for semantic code search
///
/// This wraps the hnsw_rs library, handling:
/// - Building indexes from embeddings
/// - Searching for nearest neighbors
/// - Saving/loading to disk
/// - Mapping between internal IDs and chunk IDs
pub struct HnswIndex {
    /// Internal state - either built in memory or loaded from disk
    pub(crate) inner: HnswInner,
    /// Mapping from internal index to chunk ID
    pub(crate) id_map: Vec<String>,
}

/// Internal HNSW state
pub(crate) enum HnswInner {
    /// Built in memory - owns its data with 'static lifetime
    Owned(Hnsw<'static, f32, DistCosine>),
    /// Loaded from disk - self-referential with manual lifetime management
    Loaded(LoadedHnsw),
}

impl HnswInner {
    /// Get a reference to the underlying HNSW graph regardless of variant.
    pub(crate) fn hnsw(&self) -> &Hnsw<'static, f32, DistCosine> {
        match self {
            HnswInner::Owned(hnsw) => hnsw,
            HnswInner::Loaded(loaded) => &loaded.hnsw,
        }
    }
}

impl HnswIndex {
    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }
}

impl VectorIndex for HnswIndex {
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        self.search(query, k)
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn name(&self) -> &'static str {
        "HNSW"
    }
}

/// Shared test helper: create a deterministic normalized embedding from a seed.
/// Uses sin-based values for reproducible but varied vectors.
#[cfg(test)]
pub(crate) fn make_test_embedding(seed: u32) -> Embedding {
    let mut v = vec![0.0f32; crate::EMBEDDING_DIM];
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

#[cfg(test)]
mod send_sync_tests {
    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    #[test]
    fn test_hnsw_index_is_send_sync() {
        assert_send::<HnswIndex>();
        assert_sync::<HnswIndex>();
    }

    #[test]
    fn test_loaded_hnsw_is_send_sync() {
        assert_send::<LoadedHnsw>();
        assert_sync::<LoadedHnsw>();
    }
}
