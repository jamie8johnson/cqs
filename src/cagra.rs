//! CAGRA GPU-accelerated vector search
//!
//! Uses NVIDIA cuVS for GPU-accelerated nearest neighbor search.
//! Only available when compiled with the `gpu-index` feature.
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

#[cfg(feature = "gpu-index")]
use std::sync::Mutex;

#[cfg(feature = "gpu-index")]
use ndarray_015::Array2;

#[cfg(feature = "gpu-index")]
use thiserror::Error;

#[cfg(feature = "gpu-index")]
use crate::embedder::Embedding;
#[cfg(feature = "gpu-index")]
use crate::index::{IndexResult, VectorIndex};

#[cfg(feature = "gpu-index")]
#[derive(Error, Debug)]
pub enum CagraError {
    #[error("cuVS error: {0}")]
    Cuvs(String),
    #[error("No GPU available")]
    NoGpu,
    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("Build error: {0}")]
    Build(String),
    #[error("Index not built")]
    NotBuilt,
}

/// CAGRA GPU index for vector search
///
/// Wraps cuVS CAGRA with interior mutability to handle the consuming `search()` API.
/// The index is rebuilt from cached data when needed.
///
/// # Thread Safety
///
/// Both `resources` and `index` are protected by Mutex to ensure safe concurrent access.
/// CUDA contexts (managed by cuVS Resources) are not inherently thread-safe, so we
/// serialize all GPU operations.
#[cfg(feature = "gpu-index")]
pub struct CagraIndex {
    /// Embedding dimensionality (runtime, from model config)
    dim: usize,
    /// cuVS resources (CUDA context, streams, etc.) - protected by Mutex for thread safety
    resources: Mutex<cuvs::Resources>,
    /// Cached embedding data as ndarray for rebuilding index after search
    dataset: Array2<f32>,
    /// Mapping from internal index to chunk ID
    id_map: Vec<String>,
    /// The actual index (rebuilt after each search due to consuming API)
    index: Mutex<Option<cuvs::cagra::Index>>,
}

#[cfg(feature = "gpu-index")]
impl CagraIndex {
    /// Check if GPU is available for CAGRA
    pub fn gpu_available() -> bool {
        cuvs::Resources::new().is_ok()
    }

    /// Build a CAGRA index from embeddings
    pub fn build(embeddings: Vec<(String, Embedding)>, dim: usize) -> Result<Self, CagraError> {
        let _span = tracing::debug_span!("cagra_build").entered();
        let (id_map, flat_data, n_vectors) = crate::hnsw::prepare_index_data(embeddings, dim)
            .map_err(|e| CagraError::Build(e.to_string()))?;

        tracing::info!("Building CAGRA index with {} vectors", n_vectors);

        // Create cuVS resources
        let resources = cuvs::Resources::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        let dataset = Array2::from_shape_vec((n_vectors, dim), flat_data)
            .map_err(|e| CagraError::Cuvs(format!("Failed to create array: {}", e)))?;

        // Build index parameters
        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        // Build the index
        let index = cuvs::cagra::Index::build(&resources, &build_params, &dataset)
            .map_err(|e| CagraError::Cuvs(e.to_string()))?;

        tracing::info!("CAGRA index built successfully");

        Ok(Self {
            dim,
            resources: Mutex::new(resources),
            dataset,
            id_map,
            index: Mutex::new(Some(index)),
        })
    }

    /// Rebuild index from cached embeddings (needed after search consumes it)
    ///
    /// Caller must hold the resources lock.
    fn rebuild_index_with_resources(
        &self,
        resources: &cuvs::Resources,
    ) -> Result<cuvs::cagra::Index, CagraError> {
        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        cuvs::cagra::Index::build(resources, &build_params, &self.dataset)
            .map_err(|e| CagraError::Cuvs(e.to_string()))
    }

    /// Ensure index is rebuilt and stored back in the Mutex.
    /// Called by IndexRebuilder on drop to guarantee index restoration.
    fn ensure_index_rebuilt(&self, resources: &cuvs::Resources) {
        match self.rebuild_index_with_resources(resources) {
            Ok(idx) => {
                let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                    tracing::debug!("CAGRA index mutex poisoned during rebuild, recovering");
                    poisoned.into_inner()
                });
                *guard = Some(idx);
                tracing::debug!("CAGRA index rebuilt successfully");
            }
            Err(e) => {
                tracing::error!("Failed to rebuild CAGRA index: {}", e);
            }
        }
    }

    /// Number of vectors in the index
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Checks whether this collection contains any elements.
    ///
    /// # Returns
    ///
    /// Returns `true` if the collection is empty, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }

    /// Search for nearest neighbors
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        let _span = tracing::debug_span!("cagra_search", k).entered();
        if self.id_map.is_empty() {
            return Vec::new();
        }

        if k == 0 {
            return Vec::new();
        }

        if query.len() != self.dim {
            tracing::warn!(
                "Query dimension mismatch: expected {}, got {}",
                self.dim,
                query.len()
            );
            return Vec::new();
        }

        // Lock resources for the entire search operation (CUDA contexts aren't thread-safe)
        let resources = self.resources.lock().unwrap_or_else(|poisoned| {
            tracing::debug!("CAGRA resources mutex poisoned, recovering");
            poisoned.into_inner()
        });

        // Take the index (cuVS search consumes it)
        let index = {
            let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                tracing::debug!("CAGRA index mutex poisoned, recovering");
                poisoned.into_inner()
            });
            guard.take()
        };

        let index = match index {
            Some(idx) => idx,
            None => {
                // Rebuild if it was consumed
                match self.rebuild_index_with_resources(&resources) {
                    Ok(idx) => idx,
                    Err(e) => {
                        tracing::error!("Failed to rebuild CAGRA index: {}", e);
                        return Vec::new();
                    }
                }
            }
        };

        // Search parameters - set itopk_size large enough for our k
        // CAGRA requires itopk_size > k; default library value is 64.
        // We use max(k*2, 128) for better recall at small k:
        //   - k*2 gives headroom for filtering duplicates/invalids
        //   - 128 minimum ensures enough candidates for the graph search
        // Trade-off: larger itopk_size = better recall, more GPU memory/compute
        let itopk_size = (k * 2).max(128);

        let search_params = match cuvs::cagra::SearchParams::new() {
            Ok(params) => params.set_itopk_size(itopk_size),
            Err(e) => {
                tracing::error!("Failed to create search params: {}", e);
                let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                    tracing::debug!("CAGRA index mutex poisoned, recovering");
                    poisoned.into_inner()
                });
                *guard = Some(index);
                return Vec::new();
            }
        };

        // Prepare query as 2D array (1 query x dim)
        let query_host = match Array2::from_shape_vec((1, self.dim), query.as_slice().to_vec()) {
            Ok(arr) => arr,
            Err(e) => {
                tracing::error!("Invalid query shape (expected {} dims): {}", self.dim, e);
                let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                    tracing::debug!("CAGRA index mutex poisoned, recovering");
                    poisoned.into_inner()
                });
                *guard = Some(index);
                return Vec::new();
            }
        };

        // IMPORTANT: host arrays must outlive device tensors — ManagedTensor::to_device()
        // copies data to GPU but the DLTensor shape pointer still references the host
        // ndarray's internal shape storage. Dropping the host array = dangling shape pointer.
        // RM-12: Allocate once and reuse for both to_device() and to_host().
        let mut neighbors_host: Array2<u32> = Array2::zeros((1, k));
        let mut distances_host: Array2<f32> = Array2::zeros((1, k));

        // Copy to device (shape pointers reference host arrays above)
        let query_device = match cuvs::ManagedTensor::from(&query_host).to_device(&resources) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Failed to copy query to device: {}", e);
                let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                    tracing::debug!("CAGRA index mutex poisoned, recovering");
                    poisoned.into_inner()
                });
                *guard = Some(index);
                return Vec::new();
            }
        };

        let neighbors_device =
            match cuvs::ManagedTensor::from(&neighbors_host).to_device(&resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("Failed to allocate neighbors on device: {}", e);
                    let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                        tracing::debug!("CAGRA index mutex poisoned, recovering");
                        poisoned.into_inner()
                    });
                    *guard = Some(index);
                    return Vec::new();
                }
            };

        let distances_device =
            match cuvs::ManagedTensor::from(&distances_host).to_device(&resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("Failed to allocate distances on device: {}", e);
                    let mut guard = self.index.lock().unwrap_or_else(|poisoned| {
                        tracing::debug!("CAGRA index mutex poisoned, recovering");
                        poisoned.into_inner()
                    });
                    *guard = Some(index);
                    return Vec::new();
                }
            };

        // Install RAII guard to rebuild index on all exit paths (including panics/early returns)
        let _rebuilder = IndexRebuilder {
            cagra: self,
            resources: &resources,
        };

        // Perform search (consumes index)
        if let Err(e) = index.search(
            &resources,
            &search_params,
            &query_device,
            &neighbors_device,
            &distances_device,
        ) {
            tracing::error!("CAGRA search failed: {}", e);
            return Vec::new();
        }

        // Copy results back to host — reuse the same arrays allocated for to_device() (RM-12)
        if let Err(e) = neighbors_device.to_host(&resources, &mut neighbors_host) {
            tracing::error!("Failed to copy neighbors from device: {}", e);
            return Vec::new();
        }
        if let Err(e) = distances_device.to_host(&resources, &mut distances_host) {
            tracing::error!("Failed to copy distances from device: {}", e);
            return Vec::new();
        }

        // Note: index will be automatically rebuilt by IndexRebuilder when this function returns
        // (including on early return or panic)

        // Convert results
        let mut results = Vec::with_capacity(k);
        let neighbor_row = neighbors_host.row(0);
        let distance_row = distances_host.row(0);

        for i in 0..k {
            let idx = neighbor_row[i] as usize;
            if idx < self.id_map.len() {
                // CAGRA uses squared L2 distance. For unit-norm vectors: d = 2 - 2*cos_sim,
                // so cos_sim = 1 - d/2. Vectors are unit-norm embeddings,
                // so all three backends (CAGRA, HNSW, brute-force) agree on scoring.
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

/// RAII guard that ensures the CAGRA index is rebuilt on drop.
/// This guarantees index restoration even on early returns or panics.
#[cfg(feature = "gpu-index")]
struct IndexRebuilder<'a> {
    cagra: &'a CagraIndex,
    resources: &'a cuvs::Resources,
}

#[cfg(feature = "gpu-index")]
impl<'a> Drop for IndexRebuilder<'a> {
    /// Performs cleanup when this object is dropped.
    ///
    /// Ensures that the CAGRA index is rebuilt with the current resources before the object is destroyed.
    ///
    /// # Arguments
    /// * `&mut self` - A mutable reference to self
    ///
    /// # Panics
    /// Panics if index rebuilding fails or if resources are in an invalid state.
    fn drop(&mut self) {
        self.cagra.ensure_index_rebuilt(self.resources);
    }
}

#[cfg(feature = "gpu-index")]
impl VectorIndex for CagraIndex {
    /// Searches the index for the k nearest neighbors to the given query embedding.
    ///
    /// # Arguments
    ///
    /// * `query` - The embedding vector to search for
    /// * `k` - The number of nearest neighbors to return
    ///
    /// # Returns
    ///
    /// A vector of IndexResult entries representing the k nearest neighbors found in the index, ordered by similarity/distance.
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        CagraIndex::search(self, query, k)
    }

    /// Returns the number of vectors in the index.
    ///
    /// # Returns
    ///
    /// The total count of vectors currently stored in the index.
    fn len(&self) -> usize {
        CagraIndex::len(self)
    }

    /// Checks whether the index is empty.
    ///
    /// # Returns
    ///
    /// Returns `true` if the index contains no elements, `false` otherwise.
    fn is_empty(&self) -> bool {
        CagraIndex::is_empty(self)
    }

    /// Returns the name identifier for the CAGRA index.
    ///
    /// # Returns
    /// A static string slice containing "CAGRA", the name of this index type.
    fn name(&self) -> &'static str {
        "CAGRA"
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

// SAFETY: CagraIndex is thread-safe because:
// - `resources` is protected by Mutex (CUDA contexts require serialized access)
// - `index` is protected by Mutex
// - `dataset` and `id_map` are immutable after construction
#[cfg(feature = "gpu-index")]
unsafe impl Send for CagraIndex {}
#[cfg(feature = "gpu-index")]
unsafe impl Sync for CagraIndex {}

#[cfg(feature = "gpu-index")]
impl CagraIndex {
    /// Build CAGRA index from all embeddings in a Store
    ///
    /// This is the typical way to create a CAGRA index at runtime.
    /// Unlike HNSW, CAGRA indexes are not persisted to disk.
    ///
    /// Note: CAGRA (cuVS) requires all data upfront for GPU index building,
    /// so we can't stream incrementally like HNSW. However, we stream from
    /// SQLite to avoid double-buffering in memory.
    ///
    /// Notes are excluded — they use brute-force search from SQLite so that
    /// notes are immediately searchable without rebuild.
    pub fn build_from_store(store: &crate::Store, dim: usize) -> Result<Self, CagraError> {
        let _span = tracing::debug_span!("cagra_build_from_store").entered();
        let chunk_count = store
            .chunk_count()
            .map_err(|e| CagraError::Cuvs(format!("Failed to count chunks: {}", e)))?
            as usize;

        if chunk_count == 0 {
            return Err(CagraError::Cuvs("No embeddings in store".into()));
        }

        tracing::info!("Building CAGRA index from {} chunk embeddings", chunk_count,);

        // Guard against OOM: estimate CPU memory needed for flat data + id map
        const MAX_CAGRA_CPU_BYTES: usize = 2 * 1024 * 1024 * 1024; // 2GB
        let estimated_bytes = chunk_count.saturating_mul(dim).saturating_mul(4); // f32 = 4 bytes
        if estimated_bytes > MAX_CAGRA_CPU_BYTES {
            return Err(CagraError::Cuvs(format!(
                "Dataset too large for GPU indexing: {}MB estimated (limit {}MB)",
                estimated_bytes / (1024 * 1024),
                MAX_CAGRA_CPU_BYTES / (1024 * 1024)
            )));
        }

        let mut id_map = Vec::with_capacity(chunk_count);
        let mut flat_data = Vec::with_capacity(chunk_count * dim);

        // Stream chunk embeddings in batches
        const BATCH_SIZE: usize = 10_000;
        let mut loaded_chunks = 0usize;
        for batch_result in store.embedding_batches(BATCH_SIZE) {
            let batch = batch_result
                .map_err(|e| CagraError::Cuvs(format!("Failed to fetch batch: {}", e)))?;

            let batch_len = batch.len();
            for (chunk_id, embedding) in batch {
                if embedding.len() != dim {
                    return Err(CagraError::DimensionMismatch {
                        expected: dim,
                        actual: embedding.len(),
                    });
                }
                id_map.push(chunk_id);
                flat_data.extend(embedding.into_inner());
            }

            loaded_chunks += batch_len;
            let progress_pct = if chunk_count > 0 {
                (loaded_chunks * 100) / chunk_count
            } else {
                100
            };
            tracing::info!(
                "CAGRA loading progress: {} / {} chunks ({}%)",
                loaded_chunks,
                chunk_count,
                progress_pct
            );
        }

        // Build from pre-collected data
        Self::build_from_flat(id_map, flat_data, dim)
    }

    /// Build CAGRA index from pre-collected flat data (also used by tests)
    pub(crate) fn build_from_flat(
        id_map: Vec<String>,
        flat_data: Vec<f32>,
        dim: usize,
    ) -> Result<Self, CagraError> {
        let n_vectors = id_map.len();
        if n_vectors == 0 {
            return Err(CagraError::Cuvs("Cannot build empty index".into()));
        }

        tracing::info!("Building CAGRA index with {} vectors", n_vectors);

        let resources = cuvs::Resources::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        let dataset = Array2::from_shape_vec((n_vectors, dim), flat_data)
            .map_err(|e| CagraError::Cuvs(format!("Failed to create array: {}", e)))?;

        let build_params =
            cuvs::cagra::IndexParams::new().map_err(|e| CagraError::Cuvs(e.to_string()))?;

        let index = cuvs::cagra::Index::build(&resources, &build_params, &dataset)
            .map_err(|e| CagraError::Cuvs(e.to_string()))?;

        tracing::info!("CAGRA index built successfully");

        Ok(Self {
            dim,
            resources: Mutex::new(resources),
            dataset,
            id_map,
            index: Mutex::new(Some(index)),
        })
    }
}

#[cfg(all(test, feature = "gpu-index"))]
mod tests {
    use super::*;
    use crate::index::VectorIndex;
    use crate::EMBEDDING_DIM;
    use std::sync::Mutex;

    /// Serialize GPU tests — concurrent CUDA contexts cause SIGSEGV
    static GPU_LOCK: Mutex<()> = Mutex::new(());

    /// Generates a normalized embedding vector from a seed value.
    ///
    /// Creates a deterministic embedding by computing sine-based values for each dimension using the provided seed, then normalizes the resulting vector to unit length.
    ///
    /// # Arguments
    ///
    /// * `seed` - A 32-bit unsigned integer used to generate deterministic embedding values
    ///
    /// # Returns
    ///
    /// An `Embedding` containing a normalized vector of dimension `EMBEDDING_DIM` with values derived from the seed
    fn make_embedding(seed: u32) -> Embedding {
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        for (i, val) in v.iter_mut().enumerate() {
            *val = ((seed as f32 * 10.0) + (i as f32 * 0.001)).sin();
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        Embedding::new(v)
    }

    /// Checks if a GPU is available for CAGRA operations.
    ///
    /// # Returns
    ///
    /// Returns `true` if a GPU is available, `false` otherwise. When a GPU is not available, prints a message to stderr and returns `false`.
    fn require_gpu() -> bool {
        if !CagraIndex::gpu_available() {
            eprintln!("Skipping CAGRA test: no GPU available");
            return false;
        }
        true
    }

    /// Builds a test CAGRA search index with synthetic embeddings.
    ///
    /// # Arguments
    ///
    /// * `n` - The number of embeddings to generate and index
    ///
    /// # Returns
    ///
    /// A `CagraIndex` containing `n` synthetic embeddings with keys formatted as "chunk_0", "chunk_1", etc.
    ///
    /// # Panics
    ///
    /// Panics if the index build operation fails.
    fn build_test_index(n: u32) -> CagraIndex {
        let embeddings: Vec<(String, Embedding)> = (0..n)
            .map(|i| (format!("chunk_{}", i), make_embedding(i)))
            .collect();
        CagraIndex::build(embeddings, EMBEDDING_DIM).expect("Failed to build test index")
    }

    #[test]
    fn test_gpu_available() {
        // Should return a bool without panicking
        let _ = CagraIndex::gpu_available();
    }

    #[test]
    fn test_build_simple() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(5);
        assert_eq!(index.len(), 5);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_build_empty() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let result = CagraIndex::build(vec![], EMBEDDING_DIM);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_dimension_mismatch() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let bad_embedding = Embedding::new(vec![1.0; 100]); // wrong dims
        let result = CagraIndex::build(vec![("bad".into(), bad_embedding)], EMBEDDING_DIM);
        match result {
            Err(CagraError::Build(_)) => {} // Now returns Build error via prepare_index_data
            Err(e) => panic!("Expected Build error, got: {:?}", e),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[test]
    fn test_search_self_match() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(10);
        let query = make_embedding(3); // same as chunk_3
        let results = index.search(&query, 5);
        assert!(!results.is_empty(), "Search returned no results");
        // chunk_3 should be the top result (exact match)
        assert_eq!(results[0].id, "chunk_3", "Top result should be chunk_3");
        assert!(
            results[0].score > 0.9,
            "Self-match score should be high, got {}",
            results[0].score
        );
    }

    #[test]
    fn test_search_k_limiting() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(10);
        let query = make_embedding(0);
        let results = index.search(&query, 3);
        assert!(
            results.len() <= 3,
            "Expected at most 3 results, got {}",
            results.len()
        );
    }

    #[test]
    fn test_search_ordering() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(10);
        let query = make_embedding(0);
        let results = index.search(&query, 5);
        for window in results.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "Results not sorted: {} < {}",
                window[0].score,
                window[1].score
            );
        }
    }

    #[test]
    fn test_search_dimension_mismatch_query() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(5);
        let bad_query = Embedding::new(vec![1.0; 100]); // wrong dims
        let results = index.search(&bad_query, 3);
        assert!(
            results.is_empty(),
            "Mismatched query should return empty results"
        );
    }

    #[test]
    fn test_multiple_searches() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(10);

        // First search consumes the index internally
        let results1 = index.search(&make_embedding(0), 3);
        assert!(!results1.is_empty(), "First search returned no results");

        // Second search triggers rebuild
        let results2 = index.search(&make_embedding(5), 3);
        assert!(!results2.is_empty(), "Second search returned no results");
        assert_eq!(
            results2[0].id, "chunk_5",
            "Second search should find chunk_5"
        );
    }

    #[test]
    fn test_name_returns_cagra() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(5);
        let vi: &dyn VectorIndex = &index;
        assert_eq!(vi.name(), "CAGRA");
    }

    #[test]
    fn test_is_empty() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(3);
        assert!(!index.is_empty());
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_search_rebuilds_after_use() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(10);

        // First search consumes the index
        let results1 = index.search(&make_embedding(0), 3);
        assert!(!results1.is_empty(), "First search should return results");

        // Verify index was rebuilt by performing another search
        let results2 = index.search(&make_embedding(5), 3);
        assert!(
            !results2.is_empty(),
            "Second search should return results (index was rebuilt)"
        );

        // Third search to confirm continued functionality
        let results3 = index.search(&make_embedding(8), 3);
        assert!(!results3.is_empty(), "Third search should return results");
    }

    #[test]
    fn test_consecutive_searches() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(20);

        // Run multiple searches back-to-back
        for i in 0..10 {
            let query = make_embedding(i);
            let results = index.search(&query, 5);
            assert!(
                !results.is_empty(),
                "Search {} should return results (index should be rebuilt each time)",
                i
            );
            assert!(
                results.len() <= 5,
                "Search {} returned too many results: {}",
                i,
                results.len()
            );
        }
    }

    #[test]
    fn test_search_with_invalid_k() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(5);

        // k=0 should return empty (valid behavior)
        let results = index.search(&make_embedding(0), 0);
        assert!(results.is_empty(), "k=0 should return no results");

        // After returning early, next search should still work (index wasn't consumed)
        let results = index.search(&make_embedding(1), 3);
        assert!(!results.is_empty(), "Search after k=0 should work");
    }

    #[test]
    fn test_oom_guard_arithmetic() {
        // Verify the OOM guard threshold: 2GB limit / (768 dims * 4 bytes) ≈ 699K chunks
        const MAX_CAGRA_CPU_BYTES: usize = 2 * 1024 * 1024 * 1024;
        let max_chunks = MAX_CAGRA_CPU_BYTES / (EMBEDDING_DIM * 4);

        // Just under the limit should pass
        let under = max_chunks.saturating_mul(EMBEDDING_DIM).saturating_mul(4);
        assert!(under <= MAX_CAGRA_CPU_BYTES);

        // One more chunk should exceed
        let over = (max_chunks + 1)
            .saturating_mul(EMBEDDING_DIM)
            .saturating_mul(4);
        assert!(over > MAX_CAGRA_CPU_BYTES);

        // Extreme value shouldn't overflow (saturating_mul)
        let extreme = usize::MAX.saturating_mul(EMBEDDING_DIM).saturating_mul(4);
        assert!(extreme > MAX_CAGRA_CPU_BYTES);
    }

    #[test]
    fn test_search_on_empty_index_then_valid() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }

        // This test verifies that early returns (before index consumption) work correctly
        let index = build_test_index(5);

        // Query with wrong dimension (returns early before consuming index)
        let bad_query = Embedding::new(vec![0.5; 100]);
        let results = index.search(&bad_query, 3);
        assert!(results.is_empty(), "Bad query should return empty");

        // Now a valid search should work (index wasn't consumed by early return)
        let good_query = make_embedding(2);
        let results = index.search(&good_query, 3);
        assert!(
            !results.is_empty(),
            "Good query after bad query should work"
        );
    }
}
