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

use std::mem::ManuallyDrop;
use std::path::Path;

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
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

// Note: Uses crate::index::IndexResult instead of a separate HnswResult type
// since they have identical structure (id: String, score: f32)

/// Valid HNSW file extensions (prevents path traversal via malicious checksum file)
const HNSW_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"];

/// Verify HNSW index file checksums using blake3.
///
/// **Security note:** These checksums detect accidental corruption (disk errors,
/// incomplete writes), not malicious tampering. An attacker with write access
/// to the index directory can update both the files and the checksum file.
/// For tamper-proofing, the checksum file would need to be signed or stored
/// separately in a trusted location.
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

    let checksum_content = std::fs::read_to_string(&checksum_path).map_err(|e| {
        HnswError::Internal(format!("Failed to read {}: {}", checksum_path.display(), e))
    })?;
    for line in checksum_content.lines() {
        if let Some((ext, expected)) = line.split_once(':') {
            // Only allow known extensions to prevent path traversal
            if !HNSW_EXTENSIONS.contains(&ext) {
                tracing::warn!("Ignoring unknown extension in checksum file: {}", ext);
                continue;
            }
            let path = dir.join(format!("{}.{}", basename, ext));
            if path.exists() {
                let data = std::fs::read(&path).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to read {} for checksum: {}",
                        path.display(),
                        e
                    ))
                })?;
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
    /// Build a new HNSW index from embeddings (single-pass).
    ///
    /// # When to use `build` vs `build_batched`
    ///
    /// - **`build`**: Use when all embeddings fit comfortably in memory (<50k chunks,
    ///   ~150MB for 50k × 769 × 4 bytes). Slightly higher graph quality since all
    ///   vectors are available during construction.
    ///
    /// - **`build_batched`**: Use for large indexes (>50k chunks) or memory-constrained
    ///   environments. Streams embeddings in batches to avoid OOM. Graph quality is
    ///   marginally lower but negligible for practical search accuracy.
    ///
    /// **Warning:** This loads all embeddings into memory at once.
    /// For large indexes (>50k chunks), prefer `build_batched()` to avoid OOM.
    ///
    /// # Deprecation Notice
    ///
    /// This method is soft-deprecated for new code. Prefer `build_batched()` which:
    /// - Streams embeddings in configurable batch sizes
    /// - Avoids OOM on large indexes
    /// - Has negligible quality difference in practice
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

    /// Build HNSW index incrementally from batches (memory-efficient).
    ///
    /// Processes embeddings in batches to avoid loading everything into RAM.
    /// Each batch is inserted via `parallel_insert`, building the graph incrementally.
    ///
    /// Memory usage: O(batch_size) instead of O(total_embeddings).
    /// Trade-off: Slightly lower graph quality vs. single-pass build, but
    /// negligible for practical search accuracy.
    ///
    /// # Arguments
    /// * `batches` - Iterator yielding `Result<Vec<(id, embedding)>>` batches
    /// * `estimated_total` - Hint for HNSW capacity (can be approximate)
    ///
    /// # Example
    /// ```ignore
    /// let index = HnswIndex::build_batched(
    ///     store.embedding_batches(10_000),
    ///     store.chunk_count()?,
    /// )?;
    /// ```
    pub fn build_batched<I, E>(batches: I, estimated_total: usize) -> Result<Self, HnswError>
    where
        I: Iterator<Item = Result<Vec<(String, Embedding)>, E>>,
        E: std::fmt::Display,
    {
        let capacity = estimated_total.max(1);
        tracing::info!(
            "Building HNSW index incrementally (estimated {} vectors)",
            capacity
        );

        let mut hnsw = Hnsw::new(
            MAX_NB_CONNECTION,
            capacity,
            MAX_LAYER,
            EF_CONSTRUCTION,
            DistCosine,
        );

        let mut id_map: Vec<String> = Vec::with_capacity(capacity);
        let mut total_inserted = 0usize;

        for batch_result in batches {
            let batch = batch_result
                .map_err(|e| HnswError::Internal(format!("Batch fetch failed: {}", e)))?;

            if batch.is_empty() {
                continue;
            }

            // Validate dimensions for this batch
            for (id, emb) in &batch {
                if emb.len() != EMBEDDING_DIM {
                    return Err(HnswError::DimensionMismatch {
                        expected: EMBEDDING_DIM,
                        actual: emb.len(),
                    });
                }
                tracing::trace!("Adding {} to HNSW index", id);
            }

            // Build insertion data for this batch
            // IDs are assigned sequentially starting from current id_map length
            let base_idx = id_map.len();
            let mut data_for_insert: Vec<(&Vec<f32>, usize)> = Vec::with_capacity(batch.len());

            for (i, (chunk_id, embedding)) in batch.iter().enumerate() {
                id_map.push(chunk_id.clone());
                data_for_insert.push((embedding.as_vec(), base_idx + i));
            }

            // Insert this batch (hnsw_rs supports consecutive parallel_insert calls)
            hnsw.parallel_insert_data(&data_for_insert);

            total_inserted += batch.len();
            tracing::debug!(
                "Inserted batch: {} vectors (total: {})",
                batch.len(),
                total_inserted
            );
        }

        if id_map.is_empty() {
            tracing::info!("HNSW index built (empty)");
            return Ok(Self {
                inner: HnswInner::Owned(Hnsw::new(
                    MAX_NB_CONNECTION,
                    1,
                    MAX_LAYER,
                    EF_CONSTRUCTION,
                    DistCosine,
                )),
                id_map: Vec::new(),
            });
        }

        tracing::info!("HNSW index built: {} vectors", id_map.len());

        Ok(Self {
            inner: HnswInner::Owned(hnsw),
            id_map,
        })
    }

    /// Search for nearest neighbors (inherent implementation).
    ///
    /// This is the actual search implementation. The `VectorIndex` trait method
    /// delegates to this inherent method. Both methods have identical signatures
    /// and behavior - use whichever is more convenient at the call site.
    ///
    /// # Arguments
    /// * `query` - Query embedding (769-dim: 768 model + 1 sentiment)
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of (chunk_id, score) pairs, sorted by descending score
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
                    Some(IndexResult {
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
    /// - `{basename}.hnsw.checksum` - Blake3 checksums for integrity
    pub fn save(&self, dir: &Path, basename: &str) -> Result<(), HnswError> {
        tracing::info!("Saving HNSW index to {}/{}", dir.display(), basename);

        // Verify ID map matches HNSW vector count before saving
        let hnsw_count = match &self.inner {
            HnswInner::Owned(hnsw) => hnsw.get_nb_point(),
            HnswInner::Loaded(loaded) => loaded.hnsw.get_nb_point(),
        };
        assert_eq!(
            hnsw_count,
            self.id_map.len(),
            "HNSW/ID map count mismatch on save: HNSW has {} vectors but id_map has {}. This is a bug.",
            hnsw_count,
            self.id_map.len()
        );

        // Ensure directory exists
        std::fs::create_dir_all(dir).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        // Save the HNSW graph and data using the library's file_dump
        match &self.inner {
            HnswInner::Owned(hnsw) => {
                hnsw.file_dump(dir, basename).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to dump HNSW to {}/{}: {}",
                        dir.display(),
                        basename,
                        e
                    ))
                })?;
            }
            HnswInner::Loaded(loaded) => {
                loaded.hnsw.file_dump(dir, basename).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to dump HNSW to {}/{}: {}",
                        dir.display(),
                        basename,
                        e
                    ))
                })?;
            }
        }

        // Save the ID map separately (the library doesn't store our string IDs)
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));
        let id_map_json = serde_json::to_string(&self.id_map)
            .map_err(|e| HnswError::Internal(format!("Failed to serialize ID map: {}", e)))?;
        std::fs::write(&id_map_path, &id_map_json).map_err(|e| {
            HnswError::Internal(format!("Failed to write {}: {}", id_map_path.display(), e))
        })?;

        // Set restrictive permissions on index files (Unix only)
        // These files contain code embeddings - not secrets, but defense-in-depth
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let restrictive = std::fs::Permissions::from_mode(0o600);
            // Set permissions on files we control
            let _ = std::fs::set_permissions(&id_map_path, restrictive.clone());
            // Also set on library-written files
            for ext in &["hnsw.graph", "hnsw.data"] {
                let path = dir.join(format!("{}.{}", basename, ext));
                if path.exists() {
                    let _ = std::fs::set_permissions(&path, restrictive.clone());
                }
            }
        }

        // Compute and save checksums for all files (mitigates bincode deserialization risks)
        // For .ids we hash the in-memory data to avoid re-reading the file
        let ids_hash = blake3::hash(id_map_json.as_bytes());
        let mut checksums = vec![format!("hnsw.ids:{}", ids_hash.to_hex())];
        for ext in &["hnsw.graph", "hnsw.data"] {
            let path = dir.join(format!("{}.{}", basename, ext));
            if path.exists() {
                let data = std::fs::read(&path).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to read {} for checksum: {}",
                        path.display(),
                        e
                    ))
                })?;
                let hash = blake3::hash(&data);
                checksums.push(format!("{}:{}", ext, hash.to_hex()));
            }
        }
        let checksum_path = dir.join(format!("{}.hnsw.checksum", basename));
        std::fs::write(&checksum_path, checksums.join("\n")).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to write {}: {}",
                checksum_path.display(),
                e
            ))
        })?;

        // Set permissions on checksum file too
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&checksum_path, std::fs::Permissions::from_mode(0o600));
        }

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
        let id_map_json = std::fs::read_to_string(&id_map_path).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to read ID map {}: {}",
                id_map_path.display(),
                e
            ))
        })?;
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

        // Validate id_map size matches HNSW vector count
        let hnsw_count = hnsw.get_nb_point();
        if hnsw_count != id_map.len() {
            // SAFETY: io_ptr was created from Box::into_raw, safe to reclaim
            unsafe {
                drop(Box::from_raw(io_ptr));
            }
            return Err(HnswError::Internal(format!(
                "ID map size mismatch: HNSW has {} vectors but id_map has {}",
                hnsw_count,
                id_map.len()
            )));
        }

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

    /// Get vector count without loading the full index (fast, for stats)
    pub fn count_vectors(dir: &Path, basename: &str) -> Option<usize> {
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));
        let content = match std::fs::read_to_string(&id_map_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    "Could not read HNSW id map {}: {}",
                    id_map_path.display(),
                    e
                );
                return None;
            }
        };
        let ids: Vec<String> = match serde_json::from_str(&content) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("Corrupted HNSW id map {}: {}", id_map_path.display(), e);
                return None;
            }
        };
        Some(ids.len())
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

    #[test]
    fn test_build_batched() {
        // Simulate streaming batches like Store::embedding_batches would provide
        let all_embeddings: Vec<(String, Embedding)> = (1..=25)
            .map(|i| (format!("chunk{}", i), make_embedding(i)))
            .collect();

        // Split into batches of 10 (simulating LIMIT/OFFSET pagination)
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> =
            all_embeddings
                .chunks(10)
                .map(|chunk| Ok(chunk.to_vec()))
                .collect();

        let index = HnswIndex::build_batched(batches.into_iter(), 25).unwrap();
        assert_eq!(index.len(), 25);

        // Search should work correctly
        let query = make_embedding(1);
        let results = index.search(&query, 5);
        assert!(!results.is_empty());
        // chunk1 should be in top results
        assert!(results.iter().any(|r| r.id == "chunk1"));
    }

    #[test]
    fn test_build_batched_empty() {
        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = vec![];
        let index = HnswIndex::build_batched(batches.into_iter(), 0).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_build_batched_vs_regular_equivalence() {
        // Build same index both ways, verify similar search results
        let embeddings: Vec<(String, Embedding)> = (1..=20)
            .map(|i| (format!("item{}", i), make_embedding(i)))
            .collect();

        let regular = HnswIndex::build(embeddings.clone()).unwrap();

        let batches: Vec<Result<Vec<(String, Embedding)>, std::convert::Infallible>> = embeddings
            .chunks(7) // Odd batch size to test edge cases
            .map(|chunk| Ok(chunk.to_vec()))
            .collect();
        let batched = HnswIndex::build_batched(batches.into_iter(), 20).unwrap();

        assert_eq!(regular.len(), batched.len());

        // Both should find the same items (though scores may differ slightly)
        let query = make_embedding(10);
        let regular_results = regular.search(&query, 5);
        let batched_results = batched.search(&query, 5);

        // item10 should be top result for both
        assert_eq!(regular_results[0].id, "item10");
        assert_eq!(batched_results[0].id, "item10");
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

/// Safety tests for LoadedHnsw self-referential pattern
///
/// The LoadedHnsw struct uses a raw pointer and lifetime transmute to handle
/// hnsw_rs's borrowing API. These tests verify memory safety invariants.
///
/// # CRITICAL: hnsw_rs version dependency
///
/// The safety of LoadedHnsw depends on hnsw_rs internals:
/// - `HnswIo::load_hnsw()` must return `Hnsw<'a>` borrowing from `&'a mut HnswIo`
/// - The `Hnsw` must only read (not mutate) data owned by `HnswIo`
/// - Memory layout of `Hnsw` must not change in incompatible ways
///
/// If upgrading hnsw_rs, re-run these tests and verify no UB with miri if possible.
#[cfg(test)]
mod safety_tests {
    use super::*;
    use tempfile::TempDir;

    fn make_embedding(seed: u32) -> Embedding {
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        for (i, val) in v.iter_mut().enumerate() {
            // Use large seed multiplier for clear separation between seeds
            // 10.0 ensures adjacent seeds differ by ~10 radians in the sin argument
            *val = ((seed as f32 * 10.0) + (i as f32 * 0.001)).sin();
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut v {
                *val /= norm;
            }
        }
        Embedding::new(v)
    }

    /// Test that loaded index survives multiple search operations.
    /// This exercises the self-referential pattern under repeated use.
    #[test]
    fn test_loaded_index_multiple_searches() {
        let tmp = TempDir::new().unwrap();

        // Build and save an index with several vectors
        let embeddings: Vec<_> = (1..=10)
            .map(|i| (format!("chunk{}", i), make_embedding(i)))
            .collect();
        let index = HnswIndex::build(embeddings).unwrap();
        index.save(tmp.path(), "safety_test").unwrap();

        // Load and perform many searches
        let loaded = HnswIndex::load(tmp.path(), "safety_test").unwrap();
        assert_eq!(loaded.len(), 10);

        // Multiple searches should all succeed without memory corruption
        for i in 1..=10 {
            let query = make_embedding(i);
            let results = loaded.search(&query, 5);
            assert!(!results.is_empty(), "Search {} should return results", i);

            // The correct chunk should be in top results with high similarity
            // (HNSW is approximate, so we check top-3 rather than exact first place)
            let expected_id = format!("chunk{}", i);
            let found_in_top3 = results.iter().take(3).any(|r| r.id == expected_id);
            assert!(
                found_in_top3,
                "Search {} should find chunk{} in top 3, got: {:?}",
                i,
                i,
                results.iter().take(3).map(|r| &r.id).collect::<Vec<_>>()
            );

            // The best match should have high similarity
            assert!(
                results[0].score > 0.9,
                "Best match should have high similarity, got {}",
                results[0].score
            );
        }
    }

    /// Test that loading, searching, and dropping work correctly in sequence.
    /// Verifies drop order doesn't cause use-after-free.
    #[test]
    fn test_loaded_index_lifecycle() {
        let tmp = TempDir::new().unwrap();

        let embeddings = vec![
            ("a".to_string(), make_embedding(100)),
            ("b".to_string(), make_embedding(200)),
            ("c".to_string(), make_embedding(300)),
        ];
        HnswIndex::build(embeddings)
            .unwrap()
            .save(tmp.path(), "lifecycle")
            .unwrap();

        // Load-search-drop cycle multiple times
        for cycle in 0..5 {
            let loaded = HnswIndex::load(tmp.path(), "lifecycle").unwrap();
            let results = loaded.search(&make_embedding(100), 3);
            assert_eq!(results[0].id, "a", "Cycle {} failed", cycle);
            // Drop happens here
        }
    }

    /// Test concurrent access from multiple threads.
    /// LoadedHnsw is marked Send+Sync, this verifies it's actually safe.
    #[test]
    fn test_loaded_index_threaded_access() {
        use std::sync::Arc;
        use std::thread;

        let tmp = TempDir::new().unwrap();

        let embeddings: Vec<_> = (1..=20)
            .map(|i| (format!("item{}", i), make_embedding(i)))
            .collect();
        HnswIndex::build(embeddings)
            .unwrap()
            .save(tmp.path(), "threaded")
            .unwrap();

        let loaded = Arc::new(HnswIndex::load(tmp.path(), "threaded").unwrap());

        // Spawn multiple threads doing concurrent searches
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let index = Arc::clone(&loaded);
                thread::spawn(move || {
                    for i in 1..=20 {
                        let query = make_embedding(i);
                        let results = index.search(&query, 3);
                        assert!(!results.is_empty(), "Thread {} search {} failed", t, i);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("Thread panicked");
        }
    }

    /// Verify the memory layout assumptions documented in LoadedHnsw.
    /// These are compile-time checks via static assertions.
    #[test]
    fn test_layout_invariants() {
        use std::mem::{align_of, size_of};

        // LoadedHnsw must be a reasonable size (not accidentally huge)
        let loaded_size = size_of::<LoadedHnsw>();
        assert!(
            loaded_size < 1024,
            "LoadedHnsw unexpectedly large: {} bytes",
            loaded_size
        );

        // Pointer alignment check
        assert_eq!(
            align_of::<*mut HnswIo>(),
            align_of::<usize>(),
            "Pointer alignment unexpected"
        );

        // HnswInner should be efficient - no excessive padding
        let inner_size = size_of::<HnswInner>();
        let owned_size = size_of::<Hnsw<'static, f32, DistCosine>>();
        // Inner should be at most slightly larger than the largest variant
        assert!(
            inner_size <= owned_size + 32,
            "HnswInner has excessive padding: {} vs {}",
            inner_size,
            owned_size
        );
    }

    /// Test behavior with a minimal index (single vector).
    /// Note: hnsw_rs cannot save/load empty indexes, so we test with 1 vector.
    #[test]
    fn test_loaded_minimal_index() {
        let tmp = TempDir::new().unwrap();

        let index = HnswIndex::build(vec![("only".to_string(), make_embedding(42))]).unwrap();
        index.save(tmp.path(), "minimal").unwrap();

        let loaded = HnswIndex::load(tmp.path(), "minimal").unwrap();
        assert_eq!(loaded.len(), 1);

        let results = loaded.search(&make_embedding(42), 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "only");
    }
}
