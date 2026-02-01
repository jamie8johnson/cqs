//! CAGRA GPU-accelerated vector search
//!
//! Uses NVIDIA cuVS for GPU-accelerated nearest neighbor search.
//! Only available when compiled with the `gpu-search` feature.
//!
//! ## Usage
//!
//! CAGRA indexes are built from embeddings at runtime (not persisted to disk).
//! When GPU is available and this feature is enabled, CAGRA provides
//! faster search than CPU-based HNSW for large indexes.
//!
//! ## Ownership Model
//!
//! The cuVS `search()` method consumes the index. We cache the embeddings
//! and rebuild the index as needed.

#[cfg(feature = "gpu-search")]
use std::sync::Mutex;

#[cfg(feature = "gpu-search")]
use ndarray_015::Array2;

#[cfg(feature = "gpu-search")]
use thiserror::Error;

#[cfg(feature = "gpu-search")]
use crate::embedder::Embedding;
#[cfg(feature = "gpu-search")]
use crate::index::{IndexResult, VectorIndex};

/// Embedding dimension (768 from model + 1 sentiment)
#[cfg(feature = "gpu-search")]
const EMBEDDING_DIM: usize = 769;

#[cfg(feature = "gpu-search")]
#[derive(Error, Debug)]
pub enum CagraError {
    #[error("cuVS error: {0}")]
    Cuvs(String),
    #[error("No GPU available")]
    NoGpu,
    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("Index not built")]
    NotBuilt,
}

/// CAGRA GPU index for vector search
///
/// Wraps cuVS CAGRA with interior mutability to handle the consuming `search()` API.
/// The index is rebuilt from cached data when needed.
#[cfg(feature = "gpu-search")]
pub struct CagraIndex {
    /// cuVS resources (CUDA context, streams, etc.)
    resources: cuvs::Resources,
    /// Cached embedding data as ndarray for rebuilding index after search
    dataset: Array2<f32>,
    /// Mapping from internal index to chunk ID
    id_map: Vec<String>,
    /// The actual index (rebuilt after each search due to consuming API)
    index: Mutex<Option<cuvs::cagra::Index>>,
}

#[cfg(feature = "gpu-search")]
impl CagraIndex {
    /// Check if GPU is available for CAGRA
    pub fn gpu_available() -> bool {
        cuvs::Resources::new().is_ok()
    }

    /// Build a CAGRA index from embeddings
    pub fn build(embeddings: Vec<(String, Embedding)>) -> Result<Self, CagraError> {
        if embeddings.is_empty() {
            return Err(CagraError::Cuvs("Cannot build empty index".into()));
        }

        // Validate dimensions
        for (id, emb) in &embeddings {
            if emb.len() != EMBEDDING_DIM {
                return Err(CagraError::DimensionMismatch {
                    expected: EMBEDDING_DIM,
                    actual: emb.len(),
                });
            }
            tracing::trace!("Adding {} to CAGRA index", id);
        }

        let n_vectors = embeddings.len();
        tracing::info!("Building CAGRA index with {} vectors", n_vectors);

        // Create cuVS resources
        let resources = cuvs::Resources::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        // Prepare data as ndarray (row-major: [n_vectors, EMBEDDING_DIM])
        let mut id_map = Vec::with_capacity(n_vectors);
        let mut flat_data = Vec::with_capacity(n_vectors * EMBEDDING_DIM);

        for (chunk_id, embedding) in embeddings {
            id_map.push(chunk_id);
            flat_data.extend(embedding.into_inner());
        }

        let dataset = Array2::from_shape_vec((n_vectors, EMBEDDING_DIM), flat_data)
            .map_err(|e| CagraError::Cuvs(format!("Failed to create array: {}", e)))?;

        // Build index parameters
        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        // Build the index
        let index = cuvs::cagra::Index::build(&resources, &build_params, &dataset)
            .map_err(|e| CagraError::Cuvs(e.to_string()))?;

        tracing::info!("CAGRA index built successfully");

        Ok(Self {
            resources,
            dataset,
            id_map,
            index: Mutex::new(Some(index)),
        })
    }

    /// Rebuild index from cached embeddings (needed after search consumes it)
    fn rebuild_index(&self) -> Result<cuvs::cagra::Index, CagraError> {
        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        cuvs::cagra::Index::build(&self.resources, &build_params, &self.dataset)
            .map_err(|e| CagraError::Cuvs(e.to_string()))
    }

    /// Number of vectors in the index
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Check if index is empty
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }

    /// Search for nearest neighbors
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
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

        // Take the index (cuVS search consumes it)
        let index = {
            let mut guard = self.index.lock().unwrap();
            guard.take()
        };

        let index = match index {
            Some(idx) => idx,
            None => {
                // Rebuild if it was consumed
                match self.rebuild_index() {
                    Ok(idx) => idx,
                    Err(e) => {
                        tracing::error!("Failed to rebuild CAGRA index: {}", e);
                        return Vec::new();
                    }
                }
            }
        };

        // Search parameters
        let search_params = match cuvs::cagra::SearchParams::new() {
            Ok(params) => params,
            Err(e) => {
                tracing::error!("Failed to create search params: {}", e);
                // Put index back
                let mut guard = self.index.lock().unwrap();
                *guard = Some(index);
                return Vec::new();
            }
        };

        // Prepare query as 2D array (1 query x EMBEDDING_DIM)
        let query_host = Array2::from_shape_vec((1, EMBEDDING_DIM), query.as_slice().to_vec())
            .expect("query shape");

        // Copy query to device
        let query_device = match cuvs::ManagedTensor::from(&query_host).to_device(&self.resources) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Failed to copy query to device: {}", e);
                let mut guard = self.index.lock().unwrap();
                *guard = Some(index);
                return Vec::new();
            }
        };

        // Prepare output buffers on host, then copy to device
        let neighbors_host: Array2<u32> = Array2::zeros((1, k));
        let distances_host: Array2<f32> = Array2::zeros((1, k));

        let neighbors_device =
            match cuvs::ManagedTensor::from(&neighbors_host).to_device(&self.resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("Failed to allocate neighbors on device: {}", e);
                    let mut guard = self.index.lock().unwrap();
                    *guard = Some(index);
                    return Vec::new();
                }
            };

        let distances_device =
            match cuvs::ManagedTensor::from(&distances_host).to_device(&self.resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("Failed to allocate distances on device: {}", e);
                    let mut guard = self.index.lock().unwrap();
                    *guard = Some(index);
                    return Vec::new();
                }
            };

        // Perform search (consumes index)
        if let Err(e) = index.search(
            &self.resources,
            &search_params,
            &query_device,
            &neighbors_device,
            &distances_device,
        ) {
            tracing::error!("CAGRA search failed: {}", e);
            return Vec::new();
        }

        // Copy results back to host
        let mut neighbors_result: Array2<u32> = Array2::zeros((1, k));
        let mut distances_result: Array2<f32> = Array2::zeros((1, k));

        if let Err(e) = neighbors_device.to_host(&self.resources, &mut neighbors_result) {
            tracing::error!("Failed to copy neighbors from device: {}", e);
            return Vec::new();
        }
        if let Err(e) = distances_device.to_host(&self.resources, &mut distances_result) {
            tracing::error!("Failed to copy distances from device: {}", e);
            return Vec::new();
        }

        // Note: index was consumed by search, will be rebuilt on next search

        // Convert results
        let mut results = Vec::with_capacity(k);
        let neighbor_row = neighbors_result.row(0);
        let distance_row = distances_result.row(0);

        for i in 0..k {
            let idx = neighbor_row[i] as usize;
            if idx < self.id_map.len() {
                // CAGRA returns L2 distance for normalized vectors
                // L2 distance for normalized: d = 2 - 2*cos_sim, so cos_sim = 1 - d/2
                let dist = distance_row[i];
                let score = 1.0 - dist / 2.0;
                results.push(IndexResult {
                    id: self.id_map[idx].clone(),
                    score,
                });
            }
        }

        results
    }
}

#[cfg(feature = "gpu-search")]
impl VectorIndex for CagraIndex {
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        CagraIndex::search(self, query, k)
    }

    fn len(&self) -> usize {
        CagraIndex::len(self)
    }

    fn is_empty(&self) -> bool {
        CagraIndex::is_empty(self)
    }
}

// SAFETY: Resources and Index are thread-safe when accessed through Mutex
#[cfg(feature = "gpu-search")]
unsafe impl Send for CagraIndex {}
#[cfg(feature = "gpu-search")]
unsafe impl Sync for CagraIndex {}

#[cfg(feature = "gpu-search")]
impl CagraIndex {
    /// Build CAGRA index from all embeddings in a Store
    ///
    /// This is the typical way to create a CAGRA index at runtime.
    /// Unlike HNSW, CAGRA indexes are not persisted to disk.
    pub fn build_from_store(store: &crate::Store) -> Result<Self, CagraError> {
        let embeddings = store
            .all_embeddings()
            .map_err(|e| CagraError::Cuvs(format!("Failed to load embeddings: {}", e)))?;

        if embeddings.is_empty() {
            return Err(CagraError::Cuvs("No embeddings in store".into()));
        }

        tracing::info!(
            "Building CAGRA index from {} stored embeddings",
            embeddings.len()
        );
        Self::build(embeddings)
    }
}
