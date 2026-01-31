//! HNSW (Hierarchical Navigable Small World) index for fast vector search
//!
//! Provides O(log n) approximate nearest neighbor search, scaling to >50k chunks.

use std::path::{Path, PathBuf};

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use thiserror::Error;

use crate::embedder::Embedding;

/// HNSW index parameters
const MAX_NB_CONNECTION: usize = 24; // M parameter - connections per node
const MAX_LAYER: usize = 16; // Maximum layers in the graph
const EF_CONSTRUCTION: usize = 200; // Construction-time search width

/// Embedding dimension (nomic-embed-text-v1.5)
const EMBEDDING_DIM: usize = 768;

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
}

/// Search result from HNSW index
#[derive(Debug, Clone)]
pub struct HnswResult {
    /// Chunk ID (matches Store chunk IDs)
    pub id: String,
    /// Cosine similarity score (0.0 to 1.0)
    pub score: f32,
}

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
    /// Built in memory - owns its data
    Owned(Hnsw<'static, f32, DistCosine>),
    /// Loaded from disk - stores path info for reloading
    Loaded { dir: PathBuf, basename: String },
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
            if emb.0.len() != EMBEDDING_DIM {
                return Err(HnswError::DimensionMismatch {
                    expected: EMBEDDING_DIM,
                    actual: emb.0.len(),
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
            data_for_insert.push((&embedding.0, idx));
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

        if query.0.len() != EMBEDDING_DIM {
            tracing::warn!(
                "Query dimension mismatch: expected {}, got {}",
                EMBEDDING_DIM,
                query.0.len()
            );
            return Vec::new();
        }

        let neighbors = match &self.inner {
            HnswInner::Owned(hnsw) => hnsw.search_neighbours(&query.0, k, EF_SEARCH),
            HnswInner::Loaded { dir, basename, .. } => {
                // For loaded indexes, we need to reload and search
                // This is a limitation of the library's lifetime design
                let mut hnsw_io = HnswIo::new(dir, basename);
                let hnsw: Hnsw<f32, DistCosine> = match hnsw_io.load_hnsw() {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::error!("Failed to reload HNSW for search: {}", e);
                        return Vec::new();
                    }
                };
                // Collect results while hnsw is still alive
                hnsw.search_neighbours(&query.0, k, EF_SEARCH)
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
            HnswInner::Loaded {
                dir: src_dir,
                basename: src_basename,
                ..
            } => {
                // Copy existing files to new location if different
                if src_dir != dir || src_basename != basename {
                    for ext in &["hnsw.graph", "hnsw.data"] {
                        let src = src_dir.join(format!("{}.{}", src_basename, ext));
                        let dst = dir.join(format!("{}.{}", basename, ext));
                        if src.exists() {
                            std::fs::copy(&src, &dst)?;
                        }
                    }
                }
            }
        }

        // Save the ID map separately (the library doesn't store our string IDs)
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));
        let id_map_json = serde_json::to_string(&self.id_map)
            .map_err(|e| HnswError::Internal(format!("Failed to serialize ID map: {}", e)))?;
        std::fs::write(&id_map_path, id_map_json)?;

        tracing::info!("HNSW index saved: {} vectors", self.id_map.len());

        Ok(())
    }

    /// Load an index from disk
    pub fn load(dir: &Path, basename: &str) -> Result<Self, HnswError> {
        let graph_path = dir.join(format!("{}.hnsw.graph", basename));
        let data_path = dir.join(format!("{}.hnsw.data", basename));
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));

        if !graph_path.exists() || !data_path.exists() || !id_map_path.exists() {
            return Err(HnswError::NotFound(dir.display().to_string()));
        }

        tracing::info!("Loading HNSW index from {}/{}", dir.display(), basename);

        // Load ID map
        let id_map_json = std::fs::read_to_string(&id_map_path)?;
        let id_map: Vec<String> = serde_json::from_str(&id_map_json)
            .map_err(|e| HnswError::Internal(format!("Failed to parse ID map: {}", e)))?;

        // Verify the HNSW files can be loaded by doing a test load
        let mut hnsw_io = HnswIo::new(dir, basename);
        let _: Hnsw<f32, DistCosine> = hnsw_io
            .load_hnsw()
            .map_err(|e| HnswError::Internal(format!("Failed to load HNSW: {}", e)))?;

        tracing::info!("HNSW index loaded: {} vectors", id_map.len());

        Ok(Self {
            inner: HnswInner::Loaded {
                dir: dir.to_path_buf(),
                basename: basename.to_string(),
            },
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
        Embedding(v)
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
