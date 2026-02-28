//! HNSW index persistence (save/load)

use std::cell::UnsafeCell;
use std::path::Path;

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnswio::HnswIo;

use crate::index::VectorIndex;

use super::{HnswError, HnswIndex, HnswInner, HnswIoCell, LoadedHnsw};

/// Valid HNSW file extensions (prevents path traversal via malicious checksum file)
const HNSW_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"];

/// All HNSW file extensions including checksum (for cleanup/deletion)
pub const HNSW_ALL_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"];

/// Verify HNSW index file checksums using blake3.
///
/// # Security Model
///
/// **WARNING:** These checksums detect accidental corruption only (disk errors,
/// incomplete writes). They do NOT provide tamper-detection or authenticity
/// guarantees - an attacker with filesystem access can update both files and
/// checksums. For tamper-proofing, the checksum file would need to be signed
/// or stored separately in a trusted location.
///
/// Returns Ok if checksums match or no checksum file exists (with warning).
fn verify_hnsw_checksums(dir: &Path, basename: &str) -> Result<(), HnswError> {
    let checksum_path = dir.join(format!("{}.hnsw.checksum", basename));

    if !checksum_path.exists() {
        return Err(HnswError::Internal(
            "No checksum file for HNSW index — run 'cqs index --force' to regenerate".to_string(),
        ));
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
                // Stream file through blake3 hasher to avoid loading entire file into memory
                let file = std::fs::File::open(&path).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to open {} for checksum: {}",
                        path.display(),
                        e
                    ))
                })?;
                let mut hasher = blake3::Hasher::new();
                std::io::copy(&mut std::io::BufReader::new(file), &mut hasher).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to read {} for checksum: {}",
                        path.display(),
                        e
                    ))
                })?;
                let actual = hasher.finalize().to_hex().to_string();
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

impl HnswIndex {
    /// Save the index to disk
    ///
    /// Creates files in the directory:
    /// - `{basename}.hnsw.data` - Vector data
    /// - `{basename}.hnsw.graph` - HNSW graph structure
    /// - `{basename}.hnsw.ids` - Chunk ID mapping (our addition)
    /// - `{basename}.hnsw.checksum` - Blake3 checksums for integrity
    ///
    /// # Crash safety
    /// The ID map and checksum files are written atomically (write-to-temp, then rename).
    /// The checksum file is written last, so if the process crashes during save:
    /// - If checksum is missing/incomplete, load() will fail verification
    /// - If graph/data are incomplete, load() will fail checksum verification
    ///
    /// Note: The underlying library writes graph/data non-atomically. However, the
    /// checksum verification on load ensures we never use a corrupted index.
    pub fn save(&self, dir: &Path, basename: &str) -> Result<(), HnswError> {
        tracing::info!("Saving HNSW index to {}/{}", dir.display(), basename);

        // Verify ID map matches HNSW vector count before saving
        let hnsw_count = self.inner.with_hnsw(|h| h.get_nb_point());
        if hnsw_count != self.id_map.len() {
            return Err(HnswError::Internal(format!(
                "HNSW/ID map count mismatch on save: HNSW has {} vectors but id_map has {}. This is a bug.",
                hnsw_count,
                self.id_map.len()
            )));
        }

        // Ensure target directory exists
        std::fs::create_dir_all(dir).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        // Acquire exclusive lock for save
        // NOTE: File locking is advisory only on WSL over 9P.
        // This prevents concurrent cqs processes from corrupting the index,
        // but cannot protect against external Windows process modifications.
        let lock_path = dir.join(format!("{}.hnsw.lock", basename));
        let lock_file = std::fs::File::create(&lock_path)?;
        lock_file.lock().map_err(HnswError::Io)?;
        tracing::debug!(lock_path = %lock_path.display(), "Acquired HNSW save lock");

        // Use a temporary directory for atomic writes
        // This ensures that if we crash mid-save, the old index remains intact
        let temp_dir = dir.join(format!(".{}.tmp", basename));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir).map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to clean up temp dir {}: {}",
                    temp_dir.display(),
                    e
                ))
            })?;
        }
        std::fs::create_dir_all(&temp_dir).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to create temp dir {}: {}",
                temp_dir.display(),
                e
            ))
        })?;

        // Save the HNSW graph and data to temp directory
        self.inner
            .with_hnsw(|h| h.file_dump(&temp_dir, basename))
            .map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to dump HNSW to {}/{}: {}",
                    temp_dir.display(),
                    basename,
                    e
                ))
            })?;

        // Save the ID map to temp directory
        let id_map_json = serde_json::to_string(&self.id_map)
            .map_err(|e| HnswError::Internal(format!("Failed to serialize ID map: {}", e)))?;
        let id_map_temp = temp_dir.join(format!("{}.hnsw.ids", basename));
        std::fs::write(&id_map_temp, &id_map_json).map_err(|e| {
            HnswError::Internal(format!("Failed to write {}: {}", id_map_temp.display(), e))
        })?;

        // Compute checksums from temp files
        let ids_hash = blake3::hash(id_map_json.as_bytes());
        let mut checksums = vec![format!("hnsw.ids:{}", ids_hash.to_hex())];
        for ext in &["hnsw.graph", "hnsw.data"] {
            let path = temp_dir.join(format!("{}.{}", basename, ext));
            if path.exists() {
                let file = std::fs::File::open(&path).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to open {} for checksum: {}",
                        path.display(),
                        e
                    ))
                })?;
                let mut hasher = blake3::Hasher::new();
                hasher.update_reader(file).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to read {} for checksum: {}",
                        path.display(),
                        e
                    ))
                })?;
                let hash = hasher.finalize();
                checksums.push(format!("{}:{}", ext, hash.to_hex()));
            }
        }

        // Write checksum to temp directory
        let checksum_temp = temp_dir.join(format!("{}.hnsw.checksum", basename));
        std::fs::write(&checksum_temp, checksums.join("\n")).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to write {}: {}",
                checksum_temp.display(),
                e
            ))
        })?;

        // Set restrictive permissions in temp dir (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let restrictive = std::fs::Permissions::from_mode(0o600);
            for ext in &["hnsw.ids", "hnsw.graph", "hnsw.data", "hnsw.checksum"] {
                let path = temp_dir.join(format!("{}.{}", basename, ext));
                if path.exists() {
                    if let Err(e) = std::fs::set_permissions(&path, restrictive.clone()) {
                        tracing::debug!(path = %path.display(), error = %e, "Failed to set HNSW file permissions");
                    }
                }
            }
        }

        // Atomically rename each file from temp to final location
        // This ensures each individual file is either fully written or not present
        for ext in &["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"] {
            let temp_path = temp_dir.join(format!("{}.{}", basename, ext));
            let final_path = dir.join(format!("{}.{}", basename, ext));
            if temp_path.exists() {
                if let Err(rename_err) = std::fs::rename(&temp_path, &final_path) {
                    // Cross-device fallback (Docker overlayfs, NFS, etc.)
                    std::fs::copy(&temp_path, &final_path).map_err(|copy_err| {
                        HnswError::Internal(format!(
                            "Failed to rename {} → {} ({}), copy fallback also failed: {}",
                            temp_path.display(),
                            final_path.display(),
                            rename_err,
                            copy_err
                        ))
                    })?;
                    let _ = std::fs::remove_file(&temp_path);
                }
            }
        }

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&temp_dir);

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
        // Clean up stale temp dir from interrupted save (before anything else)
        let temp_dir = dir.join(format!(".{}.tmp", basename));
        if temp_dir.exists() {
            tracing::info!("Cleaning up interrupted HNSW save");
            let _ = std::fs::remove_dir_all(&temp_dir);
        }

        let graph_path = dir.join(format!("{}.hnsw.graph", basename));
        let data_path = dir.join(format!("{}.hnsw.data", basename));
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));

        if !graph_path.exists() || !data_path.exists() || !id_map_path.exists() {
            return Err(HnswError::NotFound(dir.display().to_string()));
        }

        // Acquire shared lock for load (allows concurrent reads)
        // NOTE: File locking is advisory only on WSL over 9P.
        // This prevents concurrent cqs processes from corrupting the index,
        // but cannot protect against external Windows process modifications.
        let lock_path = dir.join(format!("{}.hnsw.lock", basename));
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock_file.lock_shared().map_err(HnswError::Io)?;
        tracing::debug!(lock_path = %lock_path.display(), "Acquired HNSW load lock (shared)");

        tracing::info!("Loading HNSW index from {}/{}", dir.display(), basename);
        verify_hnsw_checksums(dir, basename)?;

        // Check ID map file size to prevent OOM (limit: 500MB for ~10M chunk IDs)
        const MAX_ID_MAP_SIZE: u64 = 500 * 1024 * 1024;
        let id_map_size = std::fs::metadata(&id_map_path)
            .map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to stat ID map {}: {}",
                    id_map_path.display(),
                    e
                ))
            })?
            .len();
        if id_map_size > MAX_ID_MAP_SIZE {
            return Err(HnswError::Internal(format!(
                "ID map too large: {}MB > {}MB limit",
                id_map_size / (1024 * 1024),
                MAX_ID_MAP_SIZE / (1024 * 1024)
            )));
        }

        // Check graph and data file sizes to prevent OOM before deserialization
        const MAX_HNSW_GRAPH_SIZE: u64 = 500 * 1024 * 1024; // 500MB
        const MAX_HNSW_DATA_SIZE: u64 = 1024 * 1024 * 1024; // 1GB
        for (path, limit, label) in [
            (&graph_path, MAX_HNSW_GRAPH_SIZE, "graph"),
            (&data_path, MAX_HNSW_DATA_SIZE, "data"),
        ] {
            let size = std::fs::metadata(path)
                .map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to stat HNSW {} file {}: {}",
                        label,
                        path.display(),
                        e
                    ))
                })?
                .len();
            if size > limit {
                return Err(HnswError::Internal(format!(
                    "HNSW {} file too large: {}MB > {}MB limit",
                    label,
                    size / (1024 * 1024),
                    limit / (1024 * 1024)
                )));
            }
        }

        // Load ID map via streaming parse to avoid holding raw JSON + parsed Vec simultaneously
        let id_map_file = std::fs::File::open(&id_map_path).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to open ID map {}: {}",
                id_map_path.display(),
                e
            ))
        })?;
        let id_map_reader = std::io::BufReader::new(id_map_file);
        let id_map: Vec<String> = serde_json::from_reader(id_map_reader)
            .map_err(|e| HnswError::Internal(format!("Failed to parse ID map: {}", e)))?;

        // Load HNSW graph using self_cell for safe self-referential ownership
        //
        // hnsw_rs returns Hnsw<'a> borrowing from &'a mut HnswIo.
        // self_cell ties these lifetimes together without transmute.
        let hnsw_io_cell = Box::new(HnswIoCell(UnsafeCell::new(HnswIo::new(dir, basename))));

        let loaded = LoadedHnsw::try_new(hnsw_io_cell, |cell| {
            // SAFETY: Exclusive access during construction — no other references exist.
            // After this closure returns, the UnsafeCell is never accessed again directly.
            let io = unsafe { &mut *cell.0.get() };
            io.load_hnsw::<f32, DistCosine>()
                .map_err(|e| HnswError::Internal(format!("Failed to load HNSW: {}", e)))
        })?;

        // Validate id_map size matches HNSW vector count
        let hnsw_count = loaded.with_dependent(|_, hnsw| hnsw.get_nb_point());
        if hnsw_count != id_map.len() {
            return Err(HnswError::Internal(format!(
                "ID map size mismatch: HNSW has {} vectors but id_map has {}",
                hnsw_count,
                id_map.len()
            )));
        }

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
        // Guard against oversized id map files
        const MAX_ID_MAP_SIZE: usize = 100 * 1024 * 1024; // 100MB
        if content.len() > MAX_ID_MAP_SIZE {
            tracing::warn!(
                "HNSW id map too large ({} bytes): {}",
                content.len(),
                id_map_path.display()
            );
            return None;
        }
        let ids: Vec<String> = match serde_json::from_str(&content) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("Corrupted HNSW id map {}: {}", id_map_path.display(), e);
                return None;
            }
        };
        Some(ids.len())
    }

    /// Load HNSW index if available, wrapped as VectorIndex trait object.
    /// Shared helper for CLI commands.
    pub fn try_load(cq_dir: &Path) -> Option<Box<dyn VectorIndex>> {
        if Self::exists(cq_dir, "index") {
            match Self::load(cq_dir, "index") {
                Ok(index) => {
                    tracing::info!("HNSW index loaded ({} vectors)", index.len());
                    Some(Box::new(index))
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "HNSW index corrupted or incomplete — falling back to brute-force search. \
                         Run 'cqs index' to rebuild."
                    );
                    None
                }
            }
        } else {
            tracing::debug!("No HNSW index found");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::hnsw::make_test_embedding as make_embedding;

    /// Write a checksum file matching the given HNSW files
    fn write_checksums(dir: &Path, basename: &str) {
        let mut lines = Vec::new();
        for ext in &["hnsw.graph", "hnsw.data", "hnsw.ids"] {
            let path = dir.join(format!("{}.{}", basename, ext));
            if path.exists() {
                let mut hasher = blake3::Hasher::new();
                let mut file = std::fs::File::open(&path).unwrap();
                std::io::copy(&mut file, &mut hasher).unwrap();
                let hash = hasher.finalize().to_hex().to_string();
                lines.push(format!("{}:{}", ext, hash));
            }
        }
        std::fs::write(
            dir.join(format!("{}.hnsw.checksum", basename)),
            lines.join("\n"),
        )
        .unwrap();
    }

    #[test]
    fn test_load_rejects_oversized_graph_file() {
        let tmp = TempDir::new().unwrap();

        // Create valid-looking HNSW files, but make graph oversized
        let graph_path = tmp.path().join("test.hnsw.graph");
        let data_path = tmp.path().join("test.hnsw.data");
        let ids_path = tmp.path().join("test.hnsw.ids");

        // Write oversized graph file (just over 500MB limit)
        // We use set_len to create a sparse file — no actual disk I/O
        let f = std::fs::File::create(&graph_path).unwrap();
        f.set_len(501 * 1024 * 1024).unwrap();

        std::fs::write(&data_path, b"dummy").unwrap();
        std::fs::write(&ids_path, b"[]").unwrap();
        write_checksums(tmp.path(), "test");

        match HnswIndex::load(tmp.path(), "test") {
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("graph") && msg.contains("too large"),
                    "Expected graph size error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected error for oversized graph file"),
        }
    }

    #[test]
    fn test_load_rejects_oversized_data_file() {
        let tmp = TempDir::new().unwrap();

        let graph_path = tmp.path().join("test.hnsw.graph");
        let data_path = tmp.path().join("test.hnsw.data");
        let ids_path = tmp.path().join("test.hnsw.ids");

        std::fs::write(&graph_path, b"dummy").unwrap();

        // Write oversized data file (just over 1GB limit)
        let f = std::fs::File::create(&data_path).unwrap();
        f.set_len(1025 * 1024 * 1024).unwrap();

        std::fs::write(&ids_path, b"[]").unwrap();
        write_checksums(tmp.path(), "test");

        match HnswIndex::load(tmp.path(), "test") {
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("data") && msg.contains("too large"),
                    "Expected data size error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected error for oversized data file"),
        }
    }

    #[test]
    fn test_load_rejects_missing_checksum() {
        let tmp = TempDir::new().unwrap();

        std::fs::write(tmp.path().join("test.hnsw.graph"), b"data").unwrap();
        std::fs::write(tmp.path().join("test.hnsw.data"), b"data").unwrap();
        std::fs::write(tmp.path().join("test.hnsw.ids"), b"[]").unwrap();
        // No checksum file

        match HnswIndex::load(tmp.path(), "test") {
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("No checksum file"),
                    "Expected checksum error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected error for missing checksum file"),
        }
    }

    #[test]
    fn test_save_creates_lock_file() {
        let tmp = TempDir::new().unwrap();
        let basename = "test_lock";

        let embeddings = vec![
            ("chunk1".to_string(), make_embedding(1)),
            ("chunk2".to_string(), make_embedding(2)),
        ];

        let index = HnswIndex::build(embeddings).unwrap();
        index.save(tmp.path(), basename).unwrap();

        let lock_path = tmp.path().join(format!("{}.hnsw.lock", basename));
        assert!(lock_path.exists(), "Lock file should exist after save");
    }

    #[test]
    fn test_concurrent_load_shared() {
        let tmp = TempDir::new().unwrap();
        let basename = "test_shared";

        let embeddings = vec![
            ("chunk1".to_string(), make_embedding(1)),
            ("chunk2".to_string(), make_embedding(2)),
        ];

        let index = HnswIndex::build(embeddings).unwrap();
        index.save(tmp.path(), basename).unwrap();

        // Load twice — shared locks should not block each other
        let loaded1 = HnswIndex::load(tmp.path(), basename).unwrap();
        let loaded2 = HnswIndex::load(tmp.path(), basename).unwrap();
        assert_eq!(loaded1.len(), 2);
        assert_eq!(loaded2.len(), 2);
    }

    #[test]
    fn test_load_cleans_stale_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let basename = "test_index";
        let temp_dir = dir.path().join(format!(".{}.tmp", basename));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Load should clean up the temp dir even though no index exists
        let result = HnswIndex::load(dir.path(), basename);
        assert!(result.is_err()); // no index to load
        assert!(!temp_dir.exists()); // but temp dir should be cleaned
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
}
