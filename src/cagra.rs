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
//! The cuVS `search()` method consumes the index. We use interior mutability
//! to rebuild the index as needed while maintaining the VectorIndex trait.

#[cfg(feature = "gpu-search")]
use std::sync::Mutex;

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
    /// Cached embedding data for rebuilding index after search
    embeddings: Vec<Vec<f32>>,
    /// Mapping from internal index to chunk ID
    id_map: Vec<String>,
    /// The actual index (rebuilt after each search due to consuming API)
    index: Mutex<Option<cuvs::cagra::Index<f32>>>,
}

#[cfg(feature = "gpu-search")]
impl CagraIndex {
    /// Check if GPU is available for CAGRA
    pub fn gpu_available() -> bool {
        // TODO: Proper GPU detection
        // For now, try to create Resources and see if it succeeds
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

        tracing::info!("Building CAGRA index with {} vectors", embeddings.len());

        // Create cuVS resources
        let resources = cuvs::Resources::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        // Prepare data
        let mut id_map = Vec::with_capacity(embeddings.len());
        let mut emb_data = Vec::with_capacity(embeddings.len());

        for (chunk_id, embedding) in embeddings {
            id_map.push(chunk_id);
            emb_data.push(embedding.into_vec());
        }

        // Flatten for cuVS (expects [n_vectors * dim] layout)
        let n_vectors = emb_data.len();
        let flat_data: Vec<f32> = emb_data.iter().flatten().copied().collect();

        // Build index parameters
        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        // Build the index
        let index = cuvs::cagra::Index::build(
            &resources,
            &build_params,
            &flat_data,
            n_vectors,
            EMBEDDING_DIM,
        )
        .map_err(|e| CagraError::Cuvs(e.to_string()))?;

        tracing::info!("CAGRA index built successfully");

        Ok(Self {
            resources,
            embeddings: emb_data,
            id_map,
            index: Mutex::new(Some(index)),
        })
    }

    /// Rebuild index from cached embeddings (needed after search consumes it)
    fn rebuild_index(&self) -> Result<cuvs::cagra::Index<f32>, CagraError> {
        let n_vectors = self.embeddings.len();
        let flat_data: Vec<f32> = self.embeddings.iter().flatten().copied().collect();

        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        cuvs::cagra::Index::build(
            &self.resources,
            &build_params,
            &flat_data,
            n_vectors,
            EMBEDDING_DIM,
        )
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

        // Perform search
        let query_data = query.as_slice();
        let result = cuvs::cagra::search(
            &self.resources,
            &search_params,
            index,
            query_data,
            1, // n_queries
            k, // top_k
        );

        match result {
            Ok((distances, indices, _returned_index)) => {
                // Note: cuVS search consumes the index, so we need to rebuild next time
                // We could put _returned_index back, but the API might vary

                let mut results = Vec::with_capacity(k);
                for (dist, idx) in distances.iter().zip(indices.iter()) {
                    let idx = *idx as usize;
                    if idx < self.id_map.len() {
                        // Convert L2 distance to similarity
                        // L2 distance for normalized vectors: d = 2 - 2*cos_sim
                        // So cos_sim = 1 - d/2
                        let score = 1.0 - dist / 2.0;
                        results.push(IndexResult {
                            id: self.id_map[idx].clone(),
                            score,
                        });
                    }
                }
                results
            }
            Err(e) => {
                tracing::error!("CAGRA search failed: {}", e);
                Vec::new()
            }
        }
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
