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
//! ## Ownership Model (cuVS 26.4+)
//!
//! The cuVS `search()` method takes `&self` (non-consuming). The index is
//! built once and reused for all searches. No rebuild machinery needed.

#[cfg(feature = "gpu-index")]
use std::sync::atomic::{AtomicBool, Ordering};
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

/// SHL-10: Configurable CAGRA CPU memory cap via `CQS_CAGRA_MAX_BYTES` env var.
/// Defaults to 2GB. Cached in OnceLock for single parse.
#[cfg(feature = "gpu-index")]
fn cagra_max_bytes() -> usize {
    static MAX: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("CQS_CAGRA_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2 * 1024 * 1024 * 1024)
    })
}

/// CAGRA GPU index for vector search.
///
/// # Thread Safety
/// `resources` and `index` are protected by a single Mutex to ensure safe
/// concurrent access. CUDA contexts (managed by cuVS Resources) are not
/// inherently thread-safe, so we serialize all GPU operations.
#[cfg(feature = "gpu-index")]
pub struct CagraIndex {
    /// Embedding dimensionality (runtime, from model config)
    dim: usize,
    /// cuVS resources + index, protected by Mutex (CUDA contexts require serialized access)
    gpu: Mutex<GpuState>,
    /// Mapping from internal index to chunk ID
    id_map: Vec<String>,
    /// RM-V1.25-19: Set when a mutex poison is observed. The CUDA stream
    /// may be in an inconsistent posture after a mid-op panic
    /// (cudaMalloc'd buffer unfreed, stream corked, resources leaked),
    /// so subsequent searches against the same `GpuState` could
    /// double-free or CUDA-fault. `BatchContext::vector_index` checks
    /// `is_poisoned()` and forces a rebuild via `build_from_store`
    /// rather than reusing the poisoned state.
    poisoned: AtomicBool,
}

#[cfg(feature = "gpu-index")]
struct GpuState {
    resources: cuvs::Resources,
    index: cuvs::cagra::Index,
}

// Debug impl needed because cuvs types don't implement Debug
#[cfg(feature = "gpu-index")]
impl std::fmt::Debug for CagraIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CagraIndex")
            .field("dim", &self.dim)
            .field("len", &self.id_map.len())
            .finish()
    }
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

        tracing::info!(n_vectors, "Building CAGRA index");

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
            gpu: Mutex::new(GpuState { resources, index }),
            id_map,
            poisoned: AtomicBool::new(false),
        })
    }

    /// Number of vectors in the index
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Checks whether this collection contains any elements.
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }

    /// Search for nearest neighbors
    pub fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        let _span = tracing::debug_span!("cagra_search", k).entered();
        if self.id_map.is_empty() || k == 0 {
            return Vec::new();
        }

        if query.len() != self.dim {
            tracing::warn!(
                expected_dim = self.dim,
                actual_dim = query.len(),
                "Query dimension mismatch"
            );
            return Vec::new();
        }

        let gpu = self.gpu.lock().unwrap_or_else(|poisoned| {
            // RM-V1.25-19: a prior panic left the CUDA stream in an
            // unknown state. Flag the index so the caller forces a
            // rebuild on the next `vector_index()` access; return an
            // empty result here rather than run a new kernel against
            // possibly-corrupted resources.
            self.poisoned.store(true, Ordering::Release);
            tracing::warn!(
                "CAGRA GPU mutex poisoned — results from this call are discarded \
                 and the index will be rebuilt on the next vector_index() access"
            );
            poisoned.into_inner()
        });

        if self.poisoned.load(Ordering::Acquire) {
            // Don't dispatch new kernels; the caller should have already
            // rebuilt us. This is a safety net for racing clients.
            return Vec::new();
        }

        self.search_impl(&gpu, query, k, None)
    }

    /// Core search implementation shared by filtered and unfiltered paths.
    fn search_impl(
        &self,
        gpu: &GpuState,
        query: &Embedding,
        k: usize,
        bitset_device: Option<&cuvs::ManagedTensor>,
    ) -> Vec<IndexResult> {
        let itopk_size = (k * 2).clamp(128, 512);
        if k * 2 > 512 {
            tracing::debug!(k, "CAGRA itopk_size clamped to 512, recall may degrade");
        }

        let search_params = match cuvs::cagra::SearchParams::new() {
            Ok(params) => params.set_itopk_size(itopk_size),
            Err(e) => {
                tracing::error!(error = %e, "Failed to create search params");
                return Vec::new();
            }
        };

        let query_host = match Array2::from_shape_vec((1, self.dim), query.as_slice().to_vec()) {
            Ok(arr) => arr,
            Err(e) => {
                tracing::error!(expected_dim = self.dim, error = %e, "Invalid query shape");
                return Vec::new();
            }
        };

        // IMPORTANT: host arrays must outlive device tensors — ManagedTensor::to_device()
        // copies data to GPU but the DLTensor shape pointer still references the host
        // ndarray's internal shape storage.
        let mut neighbors_host: Array2<u32> = Array2::zeros((1, k));
        // AC-V1.25-7: initialize distances to +∞ so unfilled slots are
        // detectable. When `index.len() < k`, cuVS returns only `index.len()`
        // real pairs and leaves the rest of the buffer untouched — if we
        // zero-filled, those untouched slots look like perfect-match hits
        // (distance 0.0 → score 1.0) pointing at chunk_id 0.
        let mut distances_host: Array2<f32> = Array2::from_elem((1, k), f32::INFINITY);

        let query_device = match cuvs::ManagedTensor::from(&query_host).to_device(&gpu.resources) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "Failed to copy query to device");
                return Vec::new();
            }
        };

        let neighbors_device =
            match cuvs::ManagedTensor::from(&neighbors_host).to_device(&gpu.resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to allocate neighbors on device");
                    return Vec::new();
                }
            };

        let distances_device =
            match cuvs::ManagedTensor::from(&distances_host).to_device(&gpu.resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to allocate distances on device");
                    return Vec::new();
                }
            };

        // Perform search — non-consuming in cuVS 26.4+
        let result = if let Some(bitset) = bitset_device {
            gpu.index.search_with_filter(
                &gpu.resources,
                &search_params,
                &query_device,
                &neighbors_device,
                &distances_device,
                bitset,
            )
        } else {
            gpu.index.search(
                &gpu.resources,
                &search_params,
                &query_device,
                &neighbors_device,
                &distances_device,
            )
        };

        if let Err(e) = result {
            tracing::error!(error = %e, "CAGRA search failed");
            return Vec::new();
        }

        // Copy results back to host
        if let Err(e) = neighbors_device.to_host(&gpu.resources, &mut neighbors_host) {
            tracing::error!(error = %e, "Failed to copy neighbors from device");
            return Vec::new();
        }
        if let Err(e) = distances_device.to_host(&gpu.resources, &mut distances_host) {
            tracing::error!(error = %e, "Failed to copy distances from device");
            return Vec::new();
        }

        // Convert results: CAGRA uses squared L2 distance. For unit-norm vectors:
        // d = 2 - 2*cos_sim, so cos_sim = 1 - d/2.
        let mut results = Vec::with_capacity(k);
        let neighbor_row = neighbors_host.row(0);
        let distance_row = distances_host.row(0);

        for i in 0..k {
            let idx = neighbor_row[i] as usize;
            let dist = distance_row[i];
            // AC-V1.25-7: skip unfilled slots (buffer pre-seeded with +∞) so
            // we don't emit phantom perfect-match results when k > index.len().
            if !dist.is_finite() {
                continue;
            }
            if idx < self.id_map.len() {
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

#[cfg(feature = "gpu-index")]
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

    fn name(&self) -> &'static str {
        "CAGRA"
    }

    /// RM-V1.25-19: expose the poison flag so `BatchContext::vector_index`
    /// can force a full rebuild instead of reusing a possibly-corrupt
    /// CUDA context after a prior panic.
    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    /// GPU-native filtered search: builds a bitset from the predicate and
    /// passes it to CAGRA for traversal-time filtering. No over-fetching needed.
    fn search_with_filter(
        &self,
        query: &Embedding,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        let _span = tracing::debug_span!("cagra_search_filtered", k).entered();
        if self.id_map.is_empty() || k == 0 {
            return Vec::new();
        }

        if query.len() != self.dim {
            tracing::warn!(
                expected_dim = self.dim,
                actual_dim = query.len(),
                "Query dimension mismatch"
            );
            return Vec::new();
        }

        // Build bitset on host: evaluate predicate for each vector
        let n = self.id_map.len();
        let n_words = n.div_ceil(32);
        let mut bitset = vec![0u32; n_words];
        let mut included = 0usize;
        for (i, id) in self.id_map.iter().enumerate() {
            if filter(id) {
                bitset[i / 32] |= 1u32 << (i % 32);
                included += 1;
            }
        }

        // If everything passes the filter, use unfiltered search (faster)
        if included == n {
            return CagraIndex::search(self, query, k);
        }

        // If nothing passes, no results
        if included == 0 {
            return Vec::new();
        }

        tracing::debug!(
            total = n,
            included,
            excluded = n - included,
            "CAGRA bitset filter"
        );

        let gpu = self.gpu.lock().unwrap_or_else(|poisoned| {
            // RM-V1.25-19: same recovery path as `search()`. See that
            // comment for the rationale — CUDA state may be corrupt, we
            // flag for rebuild and drop the work rather than kernel-launch
            // against a bad stream.
            self.poisoned.store(true, Ordering::Release);
            tracing::warn!(
                "CAGRA GPU mutex poisoned (filtered path) — results discarded \
                 and index will be rebuilt on next vector_index()"
            );
            poisoned.into_inner()
        });

        if self.poisoned.load(Ordering::Acquire) {
            return Vec::new();
        }

        // Upload bitset to device
        let bitset_host = ndarray_015::Array1::from_vec(bitset);
        let bitset_device = match cuvs::ManagedTensor::from(&bitset_host).to_device(&gpu.resources)
        {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "Failed to upload bitset to device");
                return Vec::new();
            }
        };

        self.search_impl(&gpu, query, k, Some(&bitset_device))
    }
}

// SAFETY: CagraIndex is thread-safe because:
// - `gpu` (resources + index) is protected by Mutex (CUDA contexts require serialized access)
// - `id_map` is immutable after construction
#[cfg(feature = "gpu-index")]
unsafe impl Send for CagraIndex {}
#[cfg(feature = "gpu-index")]
unsafe impl Sync for CagraIndex {}

#[cfg(feature = "gpu-index")]
impl CagraIndex {
    /// Build CAGRA index from all embeddings in a Store.
    /// Unlike HNSW, CAGRA indexes are not persisted to disk.
    /// Note: CAGRA (cuVS) requires all data upfront for GPU index building,
    /// so we can't stream incrementally like HNSW. However, we stream from
    /// SQLite to avoid double-buffering in memory.
    /// Notes are excluded — they use brute-force search from SQLite.
    pub fn build_from_store(store: &crate::Store, dim: usize) -> Result<Self, CagraError> {
        let _span = tracing::debug_span!("cagra_build_from_store").entered();
        let chunk_count = store
            .chunk_count()
            .map_err(|e| CagraError::Cuvs(format!("Failed to count chunks: {}", e)))?
            as usize;

        if chunk_count == 0 {
            return Err(CagraError::Cuvs("No embeddings in store".into()));
        }

        tracing::info!(chunk_count, "Building CAGRA index from chunk embeddings");

        // Guard against OOM: estimate CPU memory needed for flat data + id map
        let max_bytes = cagra_max_bytes();
        let estimated_bytes = chunk_count.saturating_mul(dim).saturating_mul(4);
        if estimated_bytes > max_bytes {
            return Err(CagraError::Cuvs(format!(
                "Dataset too large for GPU indexing: {}MB estimated (limit {}MB)",
                estimated_bytes / (1024 * 1024),
                max_bytes / (1024 * 1024)
            )));
        }

        let mut id_map = Vec::with_capacity(chunk_count);
        let mut flat_data = Vec::with_capacity(chunk_count * dim);

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

        tracing::info!(n_vectors, "Building CAGRA index");

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
            gpu: Mutex::new(GpuState { resources, index }),
            id_map,
            poisoned: AtomicBool::new(false),
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

    fn require_gpu() -> bool {
        if !CagraIndex::gpu_available() {
            eprintln!("Skipping CAGRA test: no GPU available");
            return false;
        }
        true
    }

    fn build_test_index(n: u32) -> CagraIndex {
        let embeddings: Vec<(String, Embedding)> = (0..n)
            .map(|i| (format!("chunk_{}", i), make_embedding(i)))
            .collect();
        CagraIndex::build(embeddings, EMBEDDING_DIM).expect("Failed to build test index")
    }

    #[test]
    fn test_gpu_available() {
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
        let bad_embedding = Embedding::new(vec![1.0; 100]);
        let result = CagraIndex::build(vec![("bad".into(), bad_embedding)], EMBEDDING_DIM);
        match result {
            Err(CagraError::Build(_)) => {}
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
        let query = make_embedding(3);
        let results = index.search(&query, 5);
        assert!(!results.is_empty(), "Search returned no results");
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
        assert!(results.len() <= 3);
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
        let bad_query = Embedding::new(vec![1.0; 100]);
        let results = index.search(&bad_query, 3);
        assert!(results.is_empty());
    }

    #[test]
    fn test_multiple_searches() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(10);

        // Non-consuming search — no rebuild needed
        let results1 = index.search(&make_embedding(0), 3);
        assert!(!results1.is_empty());

        let results2 = index.search(&make_embedding(5), 3);
        assert!(!results2.is_empty());
        assert_eq!(results2[0].id, "chunk_5");
    }

    #[test]
    fn test_consecutive_searches() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(20);

        for i in 0..10 {
            let query = make_embedding(i);
            let results = index.search(&query, 5);
            assert!(!results.is_empty(), "Search {} should return results", i);
            assert!(results.len() <= 5);
        }
    }

    #[test]
    fn test_search_with_invalid_k() {
        let _guard = GPU_LOCK.lock().unwrap();
        if !require_gpu() {
            return;
        }
        let index = build_test_index(5);

        let results = index.search(&make_embedding(0), 0);
        assert!(results.is_empty());

        let results = index.search(&make_embedding(1), 3);
        assert!(!results.is_empty());
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
    fn test_oom_guard_arithmetic() {
        let max_bytes = super::cagra_max_bytes();
        let max_chunks = max_bytes / (EMBEDDING_DIM * 4);
        let under = max_chunks.saturating_mul(EMBEDDING_DIM).saturating_mul(4);
        assert!(under <= max_bytes);
        let over = (max_chunks + 1)
            .saturating_mul(EMBEDDING_DIM)
            .saturating_mul(4);
        assert!(over > max_bytes);
    }
}
