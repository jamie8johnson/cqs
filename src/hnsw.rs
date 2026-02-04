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

use std::mem::ManuallyDrop;
use std::path::Path;

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use thiserror::Error;

use crate::embedder::Embedding;
use crate::index::{IndexResult, VectorIndex};

/// HNSW index parameters
const MAX_NB_CONNECTION: usize = 24; // M parameter - connections per node
const MAX_LAYER: usize = 16; // Maximum layers in the graph
const EF_CONSTRUCTION: usize = 200; // Construction-time search width

/// Embedding dimension (768 from model + 1 sentiment)
const EMBEDDING_DIM: usize = 769;

/// Search width for queries (higher = more accurate but slower)
const EF_SEARCH: usize = 100;

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

/// Search result from HNSW index
#[derive(Debug, Clone)]
pub struct HnswResult {
    /// Chunk ID (matches Store chunk IDs)
    pub id: String,
    /// Cosine similarity score (0.0 to 1.0)
    pub score: f32,
}

/// Valid HNSW file extensions (prevents path traversal via malicious checksum file)
const HNSW_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"];

/// Verify HNSW index file checksums using blake3.
///
/// Returns Ok if checksums match or no checksum file exists (with warning).
fn verify_hnsw_checksums(dir: &Path, basename: &str) -> Result<(), HnswError> {
    let checksum_path = dir.join(format!("{}.hnsw.checksum", basename));

    if !checksum_path.exists() {
        tracing::warn!(
            "No checksum file for HNSW index - run 'cqs index --force' to add checksums"
        );
        return Ok(());
    }

    let checksum_content = std::fs::read_to_string(&checksum_path)?;
    for line in checksum_content.lines() {
        if let Some((ext, expected)) = line.split_once(':') {
            // Only allow known extensions to prevent path traversal
            if !HNSW_EXTENSIONS.contains(&ext) {
                tracing::warn!("Ignoring unknown extension in checksum file: {}", ext);
                continue;
            }
            let path = dir.join(format!("{}.{}", basename, ext));
            if path.exists() {
                let data = std::fs::read(&path)?;
                let actual = blake3::hash(&data).to_hex().to_string();
                if actual != expected {
                    return Err(HnswError::ChecksumMismatch {
                        file: path.display().to_string(),
                        expected: expected.to_string(),
                        actual,
                    });
                }
            }
        }
    }
    tracing::debug!("HNSW checksums verified");
    Ok(())
}

/// Self-referential wrapper for loaded HNSW
///
/// HnswIo owns the data, Hnsw borrows from it. We manage lifetimes manually:
/// - HnswIo is heap-allocated and we hold a raw pointer
/// - Hnsw is in ManuallyDrop so we control drop order
/// - Drop impl: drop Hnsw first, then free HnswIo
struct LoadedHnsw {
    /// Raw pointer to HnswIo - we own this memory
    io_ptr: *mut HnswIo,
    /// Hnsw borrowing from io_ptr (transmuted to 'static, manually dropped)
    hnsw: ManuallyDrop<Hnsw<'static, f32, DistCosine>>,
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
    inner: HnswInner,
    /// Mapping from internal index to chunk ID
    id_map: Vec<String>,
}

/// Internal HNSW state
enum HnswInner {
    /// Built in memory - owns its data with 'static lifetime
    Owned(Hnsw<'static, f32, DistCosine>),
    /// Loaded from disk - self-referential with manual lifetime management
    Loaded(LoadedHnsw),
}

impl HnswIndex {
    /// Build a new HNSW index from embeddings
    ///
    /// # Arguments
    /// * `embeddings` - Vector of (chunk_id, embedding) pairs
    pub fn build(embeddings: Vec<(String, Embedding)>) -> Result<Self, HnswError> {
        if embeddings.is_empty() {
            // Create empty index
            let hnsw = Hnsw::new(MAX_NB_CONNECTION, 1, MAX_LAYER, EF_CONSTRUCTION, DistCosine);
            return Ok(Self {
                inner: HnswInner::Owned(hnsw),
                id_map: Vec::new(),
            });
        }

        // Validate dimensions
        for (id, emb) in &embeddings {
            if emb.len() != EMBEDDING_DIM {
                return Err(HnswError::DimensionMismatch {
                    expected: EMBEDDING_DIM,
                    actual: emb.len(),
                });
            }
            tracing::trace!("Adding {} to HNSW index", id);
        }

        let nb_elem = embeddings.len();
        tracing::info!("Building HNSW index with {} vectors", nb_elem);

        // Create HNSW with cosine distance
        let mut hnsw = Hnsw::new(
            MAX_NB_CONNECTION,
            nb_elem,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistCosine,
        );

        // Build ID map and prepare data for insertion
        let mut id_map = Vec::with_capacity(nb_elem);
        let mut data_for_insert: Vec<(&Vec<f32>, usize)> = Vec::with_capacity(nb_elem);

        for (idx, (chunk_id, embedding)) in embeddings.iter().enumerate() {
            id_map.push(chunk_id.clone());
            data_for_insert.push((embedding.as_vec(), idx));
        }

        // Parallel insert for performance
        hnsw.parallel_insert_data(&data_for_insert);

        tracing::info!("HNSW index built successfully");

        Ok(Self {
            inner: HnswInner::Owned(hnsw),
            id_map,
        })
    }

    /// Search for nearest neighbors
    ///
    /// # Arguments
    /// * `query` - Query embedding
    /// * `k` - Number of results to return
    ///
    /// # Returns
    /// Vector of (chunk_id, score) pairs, sorted by descending score
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<HnswResult> {
        if self.id_map.is_empty() {
            return Vec::new();
        }

        if query.len() != EMBEDDING_DIM {
            tracing::warn!(
                "Query dimension mismatch: expected {}, got {}",
                EMBEDDING_DIM,
                query.len()
            );
            return Vec::new();
        }

        let neighbors = match &self.inner {
            HnswInner::Owned(hnsw) => hnsw.search_neighbours(query.as_slice(), k, EF_SEARCH),
            HnswInner::Loaded(loaded) => {
                loaded
                    .hnsw
                    .search_neighbours(query.as_slice(), k, EF_SEARCH)
            }
        };

        neighbors
            .into_iter()
            .filter_map(|n| {
                let idx = n.d_id;
                if idx < self.id_map.len() {
                    // Convert distance to similarity score
                    // Cosine distance is 1 - cosine_similarity, so we convert back
                    let score = 1.0 - n.distance;
                    Some(HnswResult {
                        id: self.id_map[idx].clone(),
                        score,
                    })
                } else {
                    tracing::warn!("Invalid index {} in HNSW result", idx);
                    None
                }
            })
            .collect()
    }

    /// Save the index to disk
    ///
    /// Creates files in the directory:
    /// - `{basename}.hnsw.data` - Vector data
    /// - `{basename}.hnsw.graph` - HNSW graph structure
    /// - `{basename}.hnsw.ids` - Chunk ID mapping (our addition)
    pub fn save(&self, dir: &Path, basename: &str) -> Result<(), HnswError> {
        tracing::info!("Saving HNSW index to {}/{}", dir.display(), basename);

        // Ensure directory exists
        std::fs::create_dir_all(dir)?;

        // Save the HNSW graph and data using the library's file_dump
        match &self.inner {
            HnswInner::Owned(hnsw) => {
                hnsw.file_dump(dir, basename)
                    .map_err(|e| HnswError::Internal(format!("Failed to dump HNSW: {}", e)))?;
            }
            HnswInner::Loaded(loaded) => {
                loaded
                    .hnsw
                    .file_dump(dir, basename)
                    .map_err(|e| HnswError::Internal(format!("Failed to dump HNSW: {}", e)))?;
            }
        }

        // Save the ID map separately (the library doesn't store our string IDs)
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));
        let id_map_json = serde_json::to_string(&self.id_map)
            .map_err(|e| HnswError::Internal(format!("Failed to serialize ID map: {}", e)))?;
        std::fs::write(&id_map_path, &id_map_json)?;

        // Compute and save checksums for all files (mitigates bincode deserialization risks)
        let mut checksums = Vec::new();
        for ext in &["hnsw.graph", "hnsw.data", "hnsw.ids"] {
            let path = dir.join(format!("{}.{}", basename, ext));
            if path.exists() {
                let data = std::fs::read(&path)?;
                let hash = blake3::hash(&data);
                checksums.push(format!("{}:{}", ext, hash.to_hex()));
            }
        }
        let checksum_path = dir.join(format!("{}.hnsw.checksum", basename));
        std::fs::write(&checksum_path, checksums.join("\n"))?;

        tracing::info!(
            "HNSW index saved: {} vectors (with checksums)",
            self.id_map.len()
        );

        Ok(())
    }

    /// Load an index from disk
    ///
    /// Verifies blake3 checksums before loading to mitigate bincode deserialization risks.
    /// Memory is properly freed when the HnswIndex is dropped.
    pub fn load(dir: &Path, basename: &str) -> Result<Self, HnswError> {
        let graph_path = dir.join(format!("{}.hnsw.graph", basename));
        let data_path = dir.join(format!("{}.hnsw.data", basename));
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));

        if !graph_path.exists() || !data_path.exists() || !id_map_path.exists() {
            return Err(HnswError::NotFound(dir.display().to_string()));
        }

        tracing::info!("Loading HNSW index from {}/{}", dir.display(), basename);
        verify_hnsw_checksums(dir, basename)?;

        // Load ID map
        let id_map_json = std::fs::read_to_string(&id_map_path)?;
        let id_map: Vec<String> = serde_json::from_str(&id_map_json)
            .map_err(|e| HnswError::Internal(format!("Failed to parse ID map: {}", e)))?;

        // Load HNSW graph using LoadedHnsw for proper memory management
        //
        // hnsw_rs returns Hnsw<'a> borrowing from HnswIo. We use LoadedHnsw to:
        // 1. Keep HnswIo alive as long as Hnsw needs it
        // 2. Clean up HnswIo when HnswIndex is dropped
        // 3. Ensure drop order (Hnsw first, then HnswIo)
        let hnsw_io = Box::new(HnswIo::new(dir, basename));
        let io_ptr = Box::into_raw(hnsw_io);

        // SAFETY: io_ptr is valid, we just created it from Box::into_raw above
        let hnsw: Hnsw<'_, f32, DistCosine> = unsafe { &mut *io_ptr }.load_hnsw().map_err(|e| {
            // SAFETY: io_ptr was created from Box::into_raw, safe to reclaim on error path
            unsafe {
                drop(Box::from_raw(io_ptr));
            }
            HnswError::Internal(format!("Failed to load HNSW: {}", e))
        })?;

        // SAFETY: The transmute is sound because:
        // - io_ptr will live as long as LoadedHnsw (cleaned up in Drop)
        // - LoadedHnsw's Drop ensures hnsw is dropped before io_ptr is freed
        // - Hnsw only reads from the data owned by HnswIo
        let hnsw: Hnsw<'static, f32, DistCosine> = unsafe { std::mem::transmute(hnsw) };

        let loaded = LoadedHnsw {
            io_ptr,
            hnsw: ManuallyDrop::new(hnsw),
        };

        tracing::info!("HNSW index loaded: {} vectors", id_map.len());

        Ok(Self {
            inner: HnswInner::Loaded(loaded),
            id_map,
        })
    }

    /// Check if an HNSW index exists at the given path
    pub fn exists(dir: &Path, basename: &str) -> bool {
        let graph_path = dir.join(format!("{}.hnsw.graph", basename));
        let data_path = dir.join(format!("{}.hnsw.data", basename));
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));

        graph_path.exists() && data_path.exists() && id_map_path.exists()
    }

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
            .into_iter()
            .map(|r| IndexResult {
                id: r.id,
                score: r.score,
            })
            .collect()
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_embedding(seed: u32) -> Embedding {
        // Create a simple deterministic embedding for testing
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        for (i, val) in v.iter_mut().enumerate() {
            *val = ((seed as f32 * 0.1) + (i as f32 * 0.001)).sin();
        }
        // L2 normalize
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut v {
                *val /= norm;
            }
        }
        Embedding::new(v)
    }

    #[test]
    fn test_build_and_search() {
        let embeddings = vec![
            ("chunk1".to_string(), make_embedding(1)),
            ("chunk2".to_string(), make_embedding(2)),
            ("chunk3".to_string(), make_embedding(3)),
        ];

        let index = HnswIndex::build(embeddings).unwrap();
        assert_eq!(index.len(), 3);

        // Search for something similar to chunk1
        let query = make_embedding(1);
        let results = index.search(&query, 3);

        assert!(!results.is_empty());
        // The most similar should be chunk1 itself
        assert_eq!(results[0].id, "chunk1");
        assert!(results[0].score > 0.9); // Should be very similar
    }

    #[test]
    fn test_save_and_load() {
        let tmp = TempDir::new().unwrap();

        let embeddings = vec![
            ("chunk1".to_string(), make_embedding(1)),
            ("chunk2".to_string(), make_embedding(2)),
        ];

        let index = HnswIndex::build(embeddings).unwrap();
        index.save(tmp.path(), "index").unwrap();

        assert!(HnswIndex::exists(tmp.path(), "index"));

        let loaded = HnswIndex::load(tmp.path(), "index").unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify search still works
        let query = make_embedding(1);
        let results = loaded.search(&query, 2);
        assert_eq!(results[0].id, "chunk1");
    }

    #[test]
    fn test_empty_index() {
        let index = HnswIndex::build(vec![]).unwrap();
        assert!(index.is_empty());

        let query = make_embedding(1);
        let results = index.search(&query, 5);
        assert!(results.is_empty());
    }
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
