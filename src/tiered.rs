//! Tiered ANN index backend (cuVS tiered index, opt-in behind the
//! `tiered-index` feature + a fork pin).
//!
//! # What the tiered index is
//!
//! The cuVS tiered index couples a **brute-force tier** that absorbs
//! incremental inserts with an **ANN tier** (CAGRA by default). Vectors added
//! via [`TieredIndex::extend`] land in the brute-force tier and are
//! *immediately* searchable — before the ANN tier is (re)built. Once the
//! incremental tier exceeds `min_ann_rows`, cuVS (re)builds the ANN tier
//! internally on the next extend (when `create_ann_index_on_extend` is set).
//!
//! This is the whole point for cqs: the watch loop's periodic full HNSW
//! rebuild (to clean orphaned vectors and absorb deltas) becomes unnecessary —
//! incremental adds flow into the brute-force tier via `extend`, and the ANN
//! tier compacts *inside* cuVS. The periodic-rebuild path collapses to a no-op
//! when the tiered backend is active.
//!
//! # Feature gating & the fork pin (HARD RAIL)
//!
//! `cuvs::tiered_index` exists only on our fork branch (`cqs-tiered-26.6`,
//! pinned via `[patch.crates-io]` in the workspace `Cargo.toml`). The official
//! crates.io `cuvs = 26.6` does **not** expose it. Therefore this entire
//! module is gated behind the `tiered-index` cargo feature, which is NOT
//! enabled by default and is NOT implied by `cuda-index`:
//!
//! - `cuda-index` alone → compiles against whatever `cuvs` is resolved, but
//!   never references `cuvs::tiered_index` (this module is absent). A published
//!   crates.io build (where `[patch]` is stripped) therefore compiles against
//!   official 26.6 with no tiered symbols required.
//! - `tiered-index` → adds this module; only buildable with the fork pin in
//!   place (the patch supplies `cuvs::tiered_index`).
//!
//! # Persistence
//!
//! The cuVS C API offers **no** serialize/deserialize for the tiered index
//! (the brute-force tier's incremental layout is in-memory only). So a tiered
//! index is **never persisted**: the daemon rebuilds it from the store on
//! restart. This still kills the *periodic* rebuild (the steady-state cost the
//! roadmap item targets); only the cold-start build remains, identical in cost
//! to the CAGRA build it replaces.

#![cfg(feature = "tiered-index")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

// cuVS's ManagedTensor `From` impls are for ndarray 0.15 (the version cuVS
// itself depends on); the main `ndarray` dep is 0.17. Use the `ndarray_015`
// alias for every array that crosses the cuVS boundary, exactly as `cagra.rs`
// does.
use ndarray_015::Array2;

use crate::embedder::Embedding;
use crate::index::{DistanceMetric, IndexResult, VectorIndex};

/// cuVS sentinel: an untouched distance slot. cuVS does not write past the
/// number of real neighbors found, so we seed every distance slot with `+∞`
/// and drop any slot still holding it after copy-back (its paired neighbor id
/// is garbage). Mirrors `cagra::INVALID_DISTANCE`.
const INVALID_DISTANCE: f32 = f32::INFINITY;

/// Default `min_ann_rows`: below this many incremental rows, the tiered index
/// answers purely from the brute-force tier (no ANN tier built yet). Chosen to
/// match the CAGRA-eligibility floor so the ANN tier kicks in around the same
/// corpus size CAGRA itself would. Overridable via `CQS_TIERED_MIN_ANN_ROWS`.
const TIERED_MIN_ANN_ROWS_DEFAULT: i64 = 5000;

/// Errors building or operating a tiered index.
#[derive(Debug, thiserror::Error)]
pub enum TieredError {
    /// Underlying cuVS error (build / extend / search).
    #[error("cuVS tiered error: {0}")]
    Cuvs(String),
    /// Store-level error while streaming embeddings.
    #[error("store error during tiered build: {0}")]
    Store(String),
    /// A chunk embedding had a dimension other than the index's.
    #[error("embedding dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    /// The store held no embeddings to build from.
    #[error("no embeddings in store")]
    Empty,
}

/// cuVS resources + tiered index, behind a Mutex.
///
/// CUDA contexts (cuVS `Resources`) are not thread-safe, so every GPU op is
/// serialized. Drop order is declaration order: `index` drops before
/// `resources` (the index holds handles into the resources' CUDA context).
struct GpuState {
    index: cuvs::tiered_index::Index,
    resources: cuvs::Resources,
}

impl Drop for GpuState {
    fn drop(&mut self) {
        // Block until pending CUDA work drains before Resources drops, or a
        // late kernel can fault the next cuvsResourcesCreate. Same hazard the
        // CAGRA backend guards against in its `GpuState::drop`.
        if let Err(e) = self.resources.sync_stream() {
            tracing::warn!(error = ?e, "cuvsStreamSync failed during tiered GpuState drop");
        }
    }
}

/// Tiered GPU index implementing [`VectorIndex`].
///
/// The id map is append-only and mirrors cuVS's internal row numbering:
/// build-time rows are `0..n`, and each [`extend`](Self::extend) appends new
/// rows at the end — exactly the id ordering the cuVS tiered index assigns, so
/// the `i64` neighbor ids it returns index straight into `id_map`.
pub struct TieredIndex {
    dim: usize,
    metric: DistanceMetric,
    /// The ANN backend the tiered index is built with. cqs always uses CAGRA
    /// (the tiered default and the only backend cqs builds), so search params
    /// are always `SearchParams::Cagra`.
    gpu: Mutex<GpuState>,
    /// internal row index → chunk id. Append-only; `extend` pushes new ids.
    id_map: Mutex<Vec<Box<str>>>,
    poisoned: AtomicBool,
}

impl std::fmt::Debug for TieredIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TieredIndex")
            .field("dim", &self.dim)
            .field("metric", &self.metric)
            .field("len", &self.len())
            .finish()
    }
}

// SAFETY: TieredIndex is thread-safe because:
// - `gpu` (cuVS resources + tiered index, which hold non-Send raw `*mut`
//   pointers) is protected by a Mutex — CUDA contexts require serialized
//   access, and every GPU op takes the lock.
// - `id_map` is also behind a Mutex (it grows on `extend`), so concurrent
//   reads/writes are serialized.
// The raw cuVS pointers never escape the locks. Mirrors `cagra::CagraIndex`.
unsafe impl Send for TieredIndex {}
unsafe impl Sync for TieredIndex {}

/// `min_ann_rows`, env-overridable via `CQS_TIERED_MIN_ANN_ROWS`.
fn tiered_min_ann_rows() -> i64 {
    std::env::var("CQS_TIERED_MIN_ANN_ROWS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(TIERED_MIN_ANN_ROWS_DEFAULT)
}

/// Build the tiered `IndexParams` for a CAGRA-backed ANN tier with the given
/// metric. Mirrors `cagra::cagra_build_params` for the metric mapping:
/// `Cosine` keeps cuVS's default `L2Expanded` (rank-equivalent to cosine on
/// unit-norm embeddings); `DotProduct` sets `InnerProduct`.
fn tiered_build_params(
    metric: DistanceMetric,
) -> Result<cuvs::tiered_index::IndexParams, TieredError> {
    use cuvs::tiered_index::AnnAlgo;

    let mut cagra =
        cuvs::cagra::IndexParams::new().map_err(|e| TieredError::Cuvs(e.to_string()))?;
    // Match the CAGRA backend's graph-degree defaults so the ANN tier has the
    // same recall characteristics as the standalone CAGRA backend it replaces.
    let graph_degree =
        crate::limits::parse_env_usize_clamped("CQS_CAGRA_GRAPH_DEGREE", 64, 1, 4096);
    let intermediate_graph_degree =
        crate::limits::parse_env_usize_clamped("CQS_CAGRA_INTERMEDIATE_GRAPH_DEGREE", 128, 1, 4096);
    cagra = cagra
        .set_graph_degree(graph_degree)
        .set_intermediate_graph_degree(intermediate_graph_degree);

    let params = cuvs::tiered_index::IndexParams::new()
        .map_err(|e| TieredError::Cuvs(e.to_string()))?
        .set_algo(AnnAlgo::CUVS_TIERED_INDEX_ALGO_CAGRA)
        .set_min_ann_rows(tiered_min_ann_rows())
        // Rebuild the ANN tier on extend once the incremental tier exceeds
        // min_ann_rows. This is what makes the periodic rebuild a no-op: cuVS
        // compacts the tiers internally on each extend.
        .set_create_ann_index_on_extend(true);

    let params = match metric {
        // Default L2Expanded — leave untouched (rank-equivalent to cosine).
        DistanceMetric::Cosine => params,
        DistanceMetric::DotProduct => {
            // The cuvs 26.6 wrapper exposes no `set_metric`; the inner
            // `cuvsCagraIndexParams_t` is the pub raw field. SAFETY: identical
            // pattern to the wrapper's own setters; pointer valid for the
            // lifetime of `cagra` and we hold exclusive access.
            unsafe {
                (*cagra.0).metric = cuvs::distance_type::DistanceType::InnerProduct;
            }
            params
        }
    };
    // set_cagra_params moves `cagra` into the tiered params so its C struct
    // outlives the raw pointer the tiered params store.
    let params = params.set_cagra_params(cagra);

    tracing::info!(
        graph_degree,
        intermediate_graph_degree,
        min_ann_rows = tiered_min_ann_rows(),
        metric = %metric,
        "Tiered index build params"
    );
    Ok(params)
}

impl TieredIndex {
    /// Build a tiered index by streaming every embedding out of `store`.
    ///
    /// Resolves the metric from `CQS_DISTANCE_METRIC` (falling back to cosine);
    /// `try_open` passes the slot's stored metric explicitly via
    /// [`Self::build_from_store_with_metric`].
    pub fn build_from_store<Mode>(
        store: &crate::Store<Mode>,
        dim: usize,
    ) -> Result<Self, TieredError> {
        let metric = DistanceMetric::resolve().map_err(TieredError::Cuvs)?;
        Self::build_from_store_with_metric(store, dim, metric)
    }

    /// [`Self::build_from_store`] with an explicit [`DistanceMetric`].
    pub fn build_from_store_with_metric<Mode>(
        store: &crate::Store<Mode>,
        dim: usize,
        metric: DistanceMetric,
    ) -> Result<Self, TieredError> {
        let _span = tracing::info_span!("tiered_build_from_store", metric = %metric).entered();
        let chunk_count = store
            .chunk_count()
            .map_err(|e| TieredError::Store(format!("Failed to count chunks: {e}")))?
            as usize;
        if chunk_count == 0 {
            return Err(TieredError::Empty);
        }

        // Sanity bound before with_capacity (mirrors CAGRA's defensive cap).
        const MAX_CHUNKS_SANITY: usize = 1 << 28;
        if chunk_count > MAX_CHUNKS_SANITY {
            return Err(TieredError::Store(format!(
                "Refusing to allocate id_map for chunk_count={chunk_count} > {MAX_CHUNKS_SANITY}"
            )));
        }

        let mut id_map: Vec<String> = Vec::with_capacity(chunk_count);
        let mut flat_data: Vec<f32> = Vec::with_capacity(chunk_count.saturating_mul(dim));

        let batch_size = 10_000usize;
        for batch_result in store.embedding_batches(batch_size) {
            let batch = batch_result
                .map_err(|e| TieredError::Store(format!("Failed to fetch batch: {e}")))?;
            for (chunk_id, embedding) in batch {
                if embedding.len() != dim {
                    return Err(TieredError::DimensionMismatch {
                        expected: dim,
                        actual: embedding.len(),
                    });
                }
                id_map.push(chunk_id);
                flat_data.extend(embedding.into_inner());
            }
        }

        Self::build_from_flat(id_map, flat_data, dim, metric)
    }

    /// Build from pre-collected flat row-major data (also used by tests).
    pub(crate) fn build_from_flat(
        id_map: Vec<String>,
        flat_data: Vec<f32>,
        dim: usize,
        metric: DistanceMetric,
    ) -> Result<Self, TieredError> {
        let n_vectors = id_map.len();
        if n_vectors == 0 {
            return Err(TieredError::Empty);
        }
        tracing::info!(n_vectors, metric = %metric, "Building tiered index");

        let resources = cuvs::Resources::new().map_err(|e| TieredError::Cuvs(e.to_string()))?;
        let dataset = Array2::from_shape_vec((n_vectors, dim), flat_data)
            .map_err(|e| TieredError::Cuvs(format!("Failed to create dataset array: {e}")))?;
        let build_params = tiered_build_params(metric)?;

        // Build params take a device tensor; copy the dataset up.
        let dataset_device = cuvs::ManagedTensor::from(&dataset)
            .to_device(&resources)
            .map_err(|e| TieredError::Cuvs(format!("Failed to copy dataset to device: {e}")))?;
        let index = cuvs::tiered_index::Index::build(&resources, &build_params, dataset_device)
            .map_err(|e| TieredError::Cuvs(e.to_string()))?;

        let id_map_boxed: Vec<Box<str>> = id_map.into_iter().map(String::into_boxed_str).collect();

        tracing::info!(n_vectors, "Tiered index built");
        Ok(Self {
            dim,
            metric,
            gpu: Mutex::new(GpuState { resources, index }),
            id_map: Mutex::new(id_map_boxed),
            poisoned: AtomicBool::new(false),
        })
    }

    /// Incrementally add new vectors to the brute-force tier.
    ///
    /// The new rows are appended after the existing ids (cuVS numbers them
    /// `current_len .. current_len + n`), so we extend `id_map` in the same
    /// order. The vectors are immediately searchable; cuVS rebuilds the ANN
    /// tier internally once the incremental tier exceeds `min_ann_rows`.
    ///
    /// This is the path the watch loop routes incremental inserts through
    /// instead of marking the index dirty for a periodic rebuild.
    pub fn extend(&self, items: &[(String, Embedding)]) -> Result<(), TieredError> {
        if items.is_empty() {
            return Ok(());
        }
        let _span = tracing::debug_span!("tiered_extend", n = items.len()).entered();

        let n = items.len();
        let mut flat = Vec::with_capacity(n.saturating_mul(self.dim));
        for (_, emb) in items {
            if emb.len() != self.dim {
                return Err(TieredError::DimensionMismatch {
                    expected: self.dim,
                    actual: emb.len(),
                });
            }
            flat.extend_from_slice(emb.as_slice());
        }
        let new_arr = Array2::from_shape_vec((n, self.dim), flat)
            .map_err(|e| TieredError::Cuvs(format!("Failed to shape extend batch: {e}")))?;

        let gpu = self.gpu.lock().unwrap_or_else(|p| {
            self.poisoned.store(true, Ordering::Release);
            p.into_inner()
        });
        let new_device = cuvs::ManagedTensor::from(&new_arr)
            .to_device(&gpu.resources)
            .map_err(|e| {
                TieredError::Cuvs(format!("Failed to copy extend batch to device: {e}"))
            })?;
        gpu.index
            .extend(&gpu.resources, new_device)
            .map_err(|e| TieredError::Cuvs(e.to_string()))?;
        drop(gpu);

        // Append ids only after the cuVS extend succeeds, so a failed extend
        // doesn't desync the id map from cuVS's row numbering.
        let mut id_map = self.id_map.lock().unwrap_or_else(|p| p.into_inner());
        id_map.reserve(n);
        for (id, _) in items {
            id_map.push(id.clone().into_boxed_str());
        }
        tracing::debug!(added = n, total = id_map.len(), "Tiered extend");
        Ok(())
    }

    /// Number of vectors currently indexed (build + all extends).
    pub fn len(&self) -> usize {
        self.id_map.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Whether the index holds no vectors.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Core search: copy the query up, run the tiered search, convert
    /// distances to similarity per metric.
    fn search_impl(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        if k == 0 || self.is_empty() {
            return Vec::new();
        }
        if query.len() != self.dim {
            tracing::warn!(
                expected_dim = self.dim,
                actual_dim = query.len(),
                "Tiered query dimension mismatch"
            );
            return Vec::new();
        }
        if !query.as_slice().iter().all(|v| v.is_finite()) {
            tracing::warn!("Tiered query embedding has non-finite values; returning empty");
            return Vec::new();
        }

        let gpu = self.gpu.lock().unwrap_or_else(|p| {
            self.poisoned.store(true, Ordering::Release);
            tracing::warn!("Tiered GPU mutex poisoned — discarding this search, forcing rebuild");
            p.into_inner()
        });
        if self.poisoned.load(Ordering::Acquire) {
            return Vec::new();
        }

        // Cap k at the current vector count: requesting more neighbors than
        // exist makes cuVS return sentinel slots we'd just filter out.
        let len = {
            // Cheap read; id_map mutex is uncontended relative to the GPU lock.
            self.id_map.lock().unwrap_or_else(|p| p.into_inner()).len()
        };
        let k = k.min(len);
        if k == 0 {
            return Vec::new();
        }

        let query_host = match Array2::from_shape_vec((1, self.dim), query.as_slice().to_vec()) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(error = %e, "Invalid tiered query shape");
                return Vec::new();
            }
        };
        // Host arrays must outlive the device tensors (DLTensor shape ptr
        // references host ndarray storage).
        let mut neighbors_host: Array2<i64> = Array2::zeros((1, k));
        let mut distances_host: Array2<f32> = Array2::from_elem((1, k), INVALID_DISTANCE);

        let query_device = match cuvs::ManagedTensor::from(&query_host).to_device(&gpu.resources) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "Failed to copy tiered query to device");
                return Vec::new();
            }
        };
        let neighbors_device =
            match cuvs::ManagedTensor::from(&neighbors_host).to_device(&gpu.resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to alloc tiered neighbors on device");
                    return Vec::new();
                }
            };
        let distances_device =
            match cuvs::ManagedTensor::from(&distances_host).to_device(&gpu.resources) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to alloc tiered distances on device");
                    return Vec::new();
                }
            };

        // cqs always builds the CAGRA-backed tiered index, so search params are
        // always the CAGRA variant.
        let cagra_params = match cuvs::cagra::SearchParams::new() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "Failed to create tiered CAGRA search params");
                return Vec::new();
            }
        };
        let search_params = cuvs::tiered_index::SearchParams::Cagra(cagra_params);

        let result = gpu.index.search(
            &gpu.resources,
            &search_params,
            &query_device,
            &neighbors_device,
            &distances_device,
        );
        if let Err(e) = result {
            tracing::error!(error = %e, "Tiered search failed");
            return Vec::new();
        }

        if let Err(e) = neighbors_device.to_host(&gpu.resources, &mut neighbors_host) {
            tracing::error!(error = %e, "Failed to copy tiered neighbors from device");
            return Vec::new();
        }
        if let Err(e) = distances_device.to_host(&gpu.resources, &mut distances_host) {
            tracing::error!(error = %e, "Failed to copy tiered distances from device");
            return Vec::new();
        }
        drop(gpu);

        let id_map = self.id_map.lock().unwrap_or_else(|p| p.into_inner());
        let neighbor_row = neighbors_host.row(0);
        let distance_row = distances_host.row(0);
        let mut results = Vec::with_capacity(k);
        for i in 0..k {
            let dist = distance_row[i];
            // Untouched sentinel slot → paired neighbor id is garbage.
            if !dist.is_finite() {
                continue;
            }
            let idx = neighbor_row[i];
            if idx < 0 {
                continue;
            }
            let idx = idx as usize;
            if idx < id_map.len() {
                let score = match self.metric {
                    // L2Expanded squared distance on unit-norm vectors:
                    // d = 2 - 2cos → cos = 1 - d/2.
                    DistanceMetric::Cosine => 1.0 - dist / 2.0,
                    // InnerProduct returns the raw dot product (best-first).
                    DistanceMetric::DotProduct => dist,
                };
                results.push(IndexResult {
                    id: id_map[idx].to_string(),
                    score,
                });
            }
        }
        results
    }
}

impl VectorIndex for TieredIndex {
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        let _span = tracing::debug_span!("tiered_search", k).entered();
        self.search_impl(query, k)
    }

    fn len(&self) -> usize {
        TieredIndex::len(self)
    }

    fn is_empty(&self) -> bool {
        TieredIndex::is_empty(self)
    }

    fn name(&self) -> &'static str {
        "TIERED"
    }

    fn dim(&self) -> usize {
        self.dim
    }

    /// A poisoned GPU mutex means a prior panic left the CUDA stream in an
    /// unknown posture; the caller should rebuild rather than reuse it.
    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    // `index_scores_are_cosine` stays at the default `false`: the tiered
    // CAGRA tier derives cosine through L2Expanded (`1 - d/2`,
    // floating-point-divergent from the brute-force recompute), exactly as
    // CAGRA does — so the score-reuse optimization is gated off here too.
}

/// Backend that selects the tiered index. Registered behind the
/// `tiered-index` feature; only takes effect when `CQS_TIERED_INDEX=1` is set
/// (the env gate keeps default behavior — HNSW/CAGRA — unchanged even when the
/// feature is compiled in).
///
/// Priority 150 — above CAGRA (100) so that, when opted in and eligible, the
/// tiered index shadows the standalone CAGRA backend it replaces. When the env
/// gate is off, `try_open` returns `None` and selection falls through to CAGRA
/// then HNSW exactly as before.
pub struct TieredBackend;

impl<Mode: crate::store::ClearHnswDirty> crate::index::IndexBackend<Mode> for TieredBackend {
    fn name(&self) -> &'static str {
        "tiered"
    }

    fn priority(&self) -> i32 {
        150
    }

    fn try_open(
        &self,
        ctx: &crate::index::BackendContext<'_, Mode>,
    ) -> std::result::Result<Option<Box<dyn VectorIndex>>, crate::store::StoreError> {
        // Opt-in gate: unset / not "1" → fall through to CAGRA/HNSW. Default
        // behavior is unchanged even with the feature compiled in.
        if std::env::var("CQS_TIERED_INDEX").as_deref() != Ok("1") {
            return Ok(None);
        }

        // Reuse CAGRA's eligibility floor: the tiered index is a GPU backend
        // built on the same CAGRA ANN tier, so its applicability window is the
        // same (GPU present, corpus large enough to want ANN). Below it, plain
        // HNSW remains the better fit.
        const TIERED_THRESHOLD_DEFAULT: u64 = 5000;
        let threshold: u64 = std::env::var("CQS_TIERED_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                std::env::var("CQS_CAGRA_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .or_else(|| ctx.policy.and_then(|p| p.cagra_threshold))
            .unwrap_or(TIERED_THRESHOLD_DEFAULT);
        let chunk_count = ctx.store.chunk_count().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to get chunk count for tiered threshold check");
            0
        });
        let dim = ctx.store.dim();
        let gpu_available = crate::cagra::CagraIndex::gpu_available_for(chunk_count as usize, dim);
        if chunk_count < threshold || !gpu_available {
            tracing::info!(
                backend = "hnsw",
                source = "tiered-ineligible",
                chunk_count,
                threshold,
                dim,
                gpu_available,
                "Vector index backend selected"
            );
            return Ok(None);
        }

        // The tiered index has no persistence (no cuVS serialize/deserialize),
        // so there is no load-from-disk path — always a fresh build. This still
        // eliminates the *periodic* rebuild; only the cold-start cost remains.
        let metric = match DistanceMetric::from_env() {
            Ok(Some(m)) => m,
            Ok(None) => crate::hnsw::HnswIndex::stored_metric(ctx.cqs_dir, "index")
                .unwrap_or(DistanceMetric::Cosine),
            Err(e) => {
                tracing::warn!(error = %e, "Invalid CQS_DISTANCE_METRIC — falling through from tiered");
                return Ok(None);
            }
        };

        match TieredIndex::build_from_store_with_metric(ctx.store, dim, metric) {
            Ok(idx) => {
                tracing::info!(
                    backend = "tiered",
                    source = "rebuilt",
                    vectors = idx.len(),
                    chunk_count,
                    threshold,
                    "Vector index backend selected"
                );
                Ok(Some(Box::new(idx) as Box<dyn VectorIndex>))
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to build tiered index, falling through");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBackend;
    use crate::store::ReadWrite;

    /// The backend advertises a stable name and a priority above CAGRA (100),
    /// so `register_index_backends!`'s descending sort puts it ahead of CAGRA
    /// when the feature is compiled in.
    #[test]
    fn backend_name_and_priority() {
        let b = TieredBackend;
        assert_eq!(IndexBackend::<ReadWrite>::name(&b), "tiered");
        assert_eq!(IndexBackend::<ReadWrite>::priority(&b), 150);
        assert!(
            IndexBackend::<ReadWrite>::priority(&b) > 100,
            "must outrank CAGRA"
        );
    }

    /// `tiered_min_ann_rows` honors a valid positive env override, ignores
    /// non-positive / garbage values, and defaults otherwise.
    #[test]
    #[serial_test::serial]
    fn min_ann_rows_env_override() {
        // Guard: restore the prior value so a co-scheduled test isn't perturbed.
        let prev = std::env::var("CQS_TIERED_MIN_ANN_ROWS").ok();

        std::env::remove_var("CQS_TIERED_MIN_ANN_ROWS");
        assert_eq!(tiered_min_ann_rows(), TIERED_MIN_ANN_ROWS_DEFAULT);

        std::env::set_var("CQS_TIERED_MIN_ANN_ROWS", "256");
        assert_eq!(tiered_min_ann_rows(), 256);

        // Non-positive and garbage fall back to the default.
        std::env::set_var("CQS_TIERED_MIN_ANN_ROWS", "0");
        assert_eq!(tiered_min_ann_rows(), TIERED_MIN_ANN_ROWS_DEFAULT);
        std::env::set_var("CQS_TIERED_MIN_ANN_ROWS", "-5");
        assert_eq!(tiered_min_ann_rows(), TIERED_MIN_ANN_ROWS_DEFAULT);
        std::env::set_var("CQS_TIERED_MIN_ANN_ROWS", "notanumber");
        assert_eq!(tiered_min_ann_rows(), TIERED_MIN_ANN_ROWS_DEFAULT);

        match prev {
            Some(v) => std::env::set_var("CQS_TIERED_MIN_ANN_ROWS", v),
            None => std::env::remove_var("CQS_TIERED_MIN_ANN_ROWS"),
        }
    }

    /// Build params resolve for both metrics without panicking (pure
    /// param-construction path — no GPU build).
    #[test]
    fn build_params_construct_for_both_metrics() {
        // These allocate cuVS param structs (host-side, no CUDA context),
        // mirroring `cagra::cagra_build_params`'s own unit coverage.
        assert!(tiered_build_params(DistanceMetric::Cosine).is_ok());
        assert!(tiered_build_params(DistanceMetric::DotProduct).is_ok());
    }

    /// GPU smoke pinning the tiered contract end to end: build a tiered index,
    /// search (each vector finds itself), `extend` with brand-new vectors, then
    /// search those — they must be immediately findable as their own nearest
    /// neighbor without any rebuild. This is the property the whole feature
    /// rests on (incremental adds visible without a periodic rebuild).
    ///
    /// `#[ignore]` — requires a GPU + conda libcuvs; run explicitly:
    /// `cargo test --features tiered-index tiered::tests::gpu_smoke_build_extend_search -- --ignored --test-threads=1`
    #[test]
    #[ignore = "requires GPU + libcuvs"]
    fn gpu_smoke_build_extend_search() {
        let dim = 16usize;
        let n = 512usize;
        // Deterministic, well-separated base vectors: row i is a one-hot-ish
        // ramp so each is its own clear nearest neighbor.
        let mut flat = Vec::with_capacity(n * dim);
        let mut id_map = Vec::with_capacity(n);
        for i in 0..n {
            for d in 0..dim {
                // distinct, normalized-ish pattern per row
                flat.push(((i + 1) as f32 * (d + 1) as f32).sin());
            }
            id_map.push(format!("base_{i}"));
        }
        let idx = TieredIndex::build_from_flat(id_map, flat, dim, DistanceMetric::Cosine)
            .expect("tiered build_from_flat failed");
        assert_eq!(idx.len(), n);

        // Search with the first base row; it should find itself at rank 0.
        let q0: Vec<f32> = (0..dim).map(|d| (1.0_f32 * (d + 1) as f32).sin()).collect();
        let res = idx.search(&Embedding::new(q0), 5);
        assert!(!res.is_empty(), "search returned no results");
        assert_eq!(res[0].id, "base_0", "query 0 should find itself first");

        // Extend with a brand-new vector far from the base cluster.
        let new_vec: Vec<f32> = (0..dim).map(|d| 100.0 + d as f32).collect();
        idx.extend(&[("extended_0".to_string(), Embedding::new(new_vec.clone()))])
            .expect("tiered extend failed");
        assert_eq!(idx.len(), n + 1, "extend should grow the index by 1");

        // The just-added vector must be immediately findable as its own NN —
        // no rebuild, straight from the brute-force tier.
        let res2 = idx.search(&Embedding::new(new_vec), 5);
        assert!(!res2.is_empty(), "post-extend search returned no results");
        assert_eq!(
            res2[0].id, "extended_0",
            "extended vector must be immediately findable as its own nearest neighbor"
        );
        // Self-match score should be high (cosine ≈ 1 for the same vector).
        assert!(
            res2[0].score > 0.9,
            "extended self-match score {} should be ≈1",
            res2[0].score
        );
    }
}
