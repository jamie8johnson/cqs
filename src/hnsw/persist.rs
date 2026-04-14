//! HNSW index persistence (save/load)

use std::cell::UnsafeCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnswio::HnswIo;

use crate::index::VectorIndex;

use super::{HnswError, HnswIndex, HnswInner, HnswIoCell, LoadedHnsw};

/// SHL-17: Configurable HNSW graph file size limit via `CQS_HNSW_MAX_GRAPH_BYTES` env var.
/// Defaults to 500MB. Cached in OnceLock for single parse.
fn hnsw_max_graph_bytes() -> u64 {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| match std::env::var("CQS_HNSW_MAX_GRAPH_BYTES") {
        Ok(val) => match val.parse::<u64>() {
            Ok(n) if n > 0 => {
                tracing::info!(max_bytes = n, "CQS_HNSW_MAX_GRAPH_BYTES override");
                n
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_HNSW_MAX_GRAPH_BYTES, using default 500MB"
                );
                500 * 1024 * 1024
            }
        },
        Err(_) => 500 * 1024 * 1024,
    })
}

/// SHL-17: Configurable HNSW data file size limit via `CQS_HNSW_MAX_DATA_BYTES` env var.
/// Defaults to 1GB. Cached in OnceLock for single parse.
fn hnsw_max_data_bytes() -> u64 {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| match std::env::var("CQS_HNSW_MAX_DATA_BYTES") {
        Ok(val) => match val.parse::<u64>() {
            Ok(n) if n > 0 => {
                tracing::info!(max_bytes = n, "CQS_HNSW_MAX_DATA_BYTES override");
                n
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_HNSW_MAX_DATA_BYTES, using default 1GB"
                );
                1024 * 1024 * 1024
            }
        },
        Err(_) => 1024 * 1024 * 1024,
    })
}

/// SHL-30: Configurable HNSW ID map file size limit via `CQS_HNSW_MAX_ID_MAP_BYTES` env var.
/// Defaults to 500MB. Cached in OnceLock for single parse.
fn hnsw_max_id_map_bytes() -> u64 {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| match std::env::var("CQS_HNSW_MAX_ID_MAP_BYTES") {
        Ok(val) => match val.parse::<u64>() {
            Ok(n) if n > 0 => {
                tracing::info!(max_bytes = n, "CQS_HNSW_MAX_ID_MAP_BYTES override");
                n
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_HNSW_MAX_ID_MAP_BYTES, using default 500MB"
                );
                500 * 1024 * 1024
            }
        },
        Err(_) => 500 * 1024 * 1024,
    })
}

/// Whether the WSL advisory locking warning has been emitted (once per process)
static WSL_LOCK_WARNED: AtomicBool = AtomicBool::new(false);

/// Emit a one-time warning about advisory-only file locking on WSL/NTFS mounts.
fn warn_wsl_advisory_locking(dir: &Path) {
    if crate::config::is_wsl()
        && dir.to_str().is_some_and(|p| p.starts_with("/mnt/"))
        && !WSL_LOCK_WARNED.swap(true, Ordering::Relaxed)
    {
        tracing::warn!(
            "HNSW file locking is advisory-only on WSL/NTFS — avoid concurrent index operations"
        );
    }
}

/// Core HNSW file extensions (graph, data, IDs)
const HNSW_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"];

/// All HNSW file extensions including checksum (for cleanup/deletion).
/// NOTE: Keep in sync with HNSW_EXTENSIONS above — first 3 elements must match.
pub const HNSW_ALL_EXTENSIONS: &[&str] = &[
    "hnsw.graph",
    "hnsw.data",
    "hnsw.ids",
    "hnsw.checksum",
    "hnsw.lock",
];

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
pub fn verify_hnsw_checksums(dir: &Path, basename: &str) -> Result<(), HnswError> {
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
        let _span = tracing::debug_span!("hnsw_save", dir = %dir.display(), basename).entered();
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
        #[allow(clippy::suspicious_open_options)] // Intentional: create if missing, don't truncate
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;
        lock_file.lock().map_err(HnswError::Io)?;
        warn_wsl_advisory_locking(dir);
        tracing::debug!(lock_path = %lock_path.display(), "Acquired HNSW save lock");

        // Use a temporary directory for atomic writes
        // This ensures that if we crash mid-save, the old index remains intact
        // PB-20: unpredictable suffix to prevent symlink TOCTOU
        let suffix = crate::temp_suffix();
        let temp_dir = dir.join(format!(".{}.{:016x}.tmp", basename, suffix));
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

        // RM-16: Stream ID map directly to file via BufWriter instead of
        // serializing to an in-memory JSON string first.
        let id_map_temp = temp_dir.join(format!("{}.hnsw.ids", basename));
        {
            // SEC-1: Create with mode 0o600 so file is never world-readable
            let file = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&id_map_temp)
                }
                #[cfg(not(unix))]
                {
                    std::fs::File::create(&id_map_temp)
                }
            }
            .map_err(|e| {
                HnswError::Internal(format!("Failed to create {}: {}", id_map_temp.display(), e))
            })?;
            let writer = std::io::BufWriter::new(file);
            serde_json::to_writer(writer, &self.id_map)
                .map_err(|e| HnswError::Internal(format!("Failed to serialize ID map: {}", e)))?;
        }

        // Compute checksum by reading back the file (avoids holding JSON in memory)
        let ids_hash = {
            let file = std::fs::File::open(&id_map_temp).map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to open {} for checksum: {}",
                    id_map_temp.display(),
                    e
                ))
            })?;
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(file).map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to read {} for checksum: {}",
                    id_map_temp.display(),
                    e
                ))
            })?;
            hasher.finalize()
        };
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

        // SEC-1: Write checksum with mode 0o600 from creation
        let checksum_temp = temp_dir.join(format!("{}.hnsw.checksum", basename));
        {
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&checksum_temp)
                    .map_err(|e| {
                        HnswError::Internal(format!(
                            "Failed to write {}: {}",
                            checksum_temp.display(),
                            e
                        ))
                    })?;
                f.write_all(checksums.join("\n").as_bytes()).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to write {}: {}",
                        checksum_temp.display(),
                        e
                    ))
                })?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&checksum_temp, checksums.join("\n")).map_err(|e| {
                    HnswError::Internal(format!(
                        "Failed to write {}: {}",
                        checksum_temp.display(),
                        e
                    ))
                })?;
            }
        }

        // Set restrictive permissions on remaining temp files (graph/data written by library)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let restrictive = std::fs::Permissions::from_mode(0o600);
            for ext in &["hnsw.graph", "hnsw.data"] {
                let path = temp_dir.join(format!("{}.{}", basename, ext));
                if path.exists() {
                    if let Err(e) = std::fs::set_permissions(&path, restrictive.clone()) {
                        tracing::debug!(path = %path.display(), error = %e, "Failed to set HNSW file permissions");
                    }
                }
            }
        }

        // Atomically rename each file from temp to final location.
        // Track which files were successfully moved so we can roll back on failure.
        let all_exts = ["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"];
        let mut moved_exts: Vec<&str> = Vec::new();

        let rename_result: Result<(), HnswError> = (|| {
            // Back up existing files before overwriting so rollback can restore them
            for ext in &all_exts {
                let final_path = dir.join(format!("{}.{}", basename, ext));
                let bak_path = dir.join(format!("{}.{}.bak", basename, ext));
                if final_path.exists() {
                    if let Err(e) = std::fs::rename(&final_path, &bak_path) {
                        tracing::warn!(
                            path = %final_path.display(),
                            error = %e,
                            "Failed to back up existing HNSW file before save"
                        );
                    }
                }
            }

            for ext in &all_exts {
                let temp_path = temp_dir.join(format!("{}.{}", basename, ext));
                let final_path = dir.join(format!("{}.{}", basename, ext));
                if temp_path.exists() {
                    if let Err(rename_err) = std::fs::rename(&temp_path, &final_path) {
                        // Cross-device fallback (Docker overlayfs, NFS, etc.)
                        // PB-20: unpredictable suffix
                        let fb_suffix = crate::temp_suffix();
                        let target_tmp =
                            dir.join(format!(".{}.{}.{:016x}.tmp", basename, ext, fb_suffix));
                        std::fs::copy(&temp_path, &target_tmp).map_err(|copy_err| {
                            HnswError::Internal(format!(
                                "Failed to rename {} -> {} ({}), copy fallback also failed: {}",
                                temp_path.display(),
                                final_path.display(),
                                rename_err,
                                copy_err
                            ))
                        })?;
                        // SEC-2: Restrict permissions on copy fallback target
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = std::fs::set_permissions(
                                &target_tmp,
                                std::fs::Permissions::from_mode(0o600),
                            );
                        }
                        std::fs::rename(&target_tmp, &final_path).map_err(|e| {
                            let _ = std::fs::remove_file(&target_tmp);
                            HnswError::Internal(format!(
                                "Failed to rename {} -> {} after cross-device copy: {}",
                                target_tmp.display(),
                                final_path.display(),
                                e
                            ))
                        })?;
                        let _ = std::fs::remove_file(&temp_path);
                    }
                    moved_exts.push(ext);
                }
            }
            Ok(())
        })();

        if let Err(e) = rename_result {
            // Roll back: remove new files and restore originals from .bak
            for ext in &moved_exts {
                let final_path = dir.join(format!("{}.{}", basename, ext));
                let _ = std::fs::remove_file(&final_path);
            }
            for ext in &all_exts {
                let bak_path = dir.join(format!("{}.{}.bak", basename, ext));
                let final_path = dir.join(format!("{}.{}", basename, ext));
                if bak_path.exists() {
                    if let Err(e) = std::fs::rename(&bak_path, &final_path) {
                        tracing::error!(
                            path = %final_path.display(),
                            error = %e,
                            "Failed to restore backup during HNSW save rollback"
                        );
                    }
                }
            }
            tracing::warn!(error = %e, "HNSW save failed mid-rename, rolled back to original files");
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(e);
        }

        // Clean up temp directory and .bak files from successful save
        let _ = std::fs::remove_dir_all(&temp_dir);
        for ext in &all_exts {
            let bak_path = dir.join(format!("{}.{}.bak", basename, ext));
            let _ = std::fs::remove_file(&bak_path);
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
    pub fn load_with_dim(dir: &Path, basename: &str, dim: usize) -> Result<Self, HnswError> {
        let _span = tracing::debug_span!("hnsw_load", dir = %dir.display(), basename).entered();
        // Clean up stale temp dirs from interrupted saves (before anything else).
        // PB-20: temp dirs now have unpredictable suffixes, so match by prefix+suffix pattern.
        if let Ok(entries) = std::fs::read_dir(dir) {
            let prefix = format!(".{}.", basename);
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(".tmp") && entry.path().is_dir() {
                    tracing::info!(dir = %entry.path().display(), "Cleaning up interrupted HNSW save");
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
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
        warn_wsl_advisory_locking(dir);
        tracing::debug!(lock_path = %lock_path.display(), "Acquired HNSW load lock (shared)");

        tracing::info!("Loading HNSW index from {}/{}", dir.display(), basename);
        verify_hnsw_checksums(dir, basename)?;

        // Check ID map file size to prevent OOM (limit configurable via CQS_HNSW_MAX_ID_MAP_BYTES)
        let max_id_map_size = hnsw_max_id_map_bytes();
        let id_map_size = std::fs::metadata(&id_map_path)
            .map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to stat ID map {}: {}",
                    id_map_path.display(),
                    e
                ))
            })?
            .len();
        if id_map_size > max_id_map_size {
            return Err(HnswError::Internal(format!(
                "ID map too large: {}MB > {}MB limit",
                id_map_size / (1024 * 1024),
                max_id_map_size / (1024 * 1024)
            )));
        }

        // Check graph and data file sizes to prevent OOM before deserialization
        for (path, limit, label) in [
            (&graph_path, hnsw_max_graph_bytes(), "graph"),
            (&data_path, hnsw_max_data_bytes(), "data"),
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

        // SEC-15: Cap element count to prevent memory exhaustion from crafted id_map files.
        // 10M entries at ~64 bytes average ID = ~640MB — well above any real codebase.
        const MAX_ID_MAP_ENTRIES: usize = 10_000_000;
        if id_map.len() > MAX_ID_MAP_ENTRIES {
            return Err(HnswError::Internal(format!(
                "ID map has {} entries, exceeding {} limit — possible corruption",
                id_map.len(),
                MAX_ID_MAP_ENTRIES
            )));
        }

        // SEC-7: Validate data file size against id_map before bincode deserialization.
        // A crafted file could claim more vectors than the id_map supports, causing
        // unbounded allocation during deserialization. Each vector is `dim` f32s,
        // with 2x headroom for HNSW graph overhead (neighbor lists, metadata).
        if !id_map.is_empty() {
            let expected_max_data = id_map.len() * dim * std::mem::size_of::<f32>() * 2;
            let data_meta = std::fs::metadata(&data_path).map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to stat data file {}: {}",
                    data_path.display(),
                    e
                ))
            })?;
            if data_meta.len() as usize > expected_max_data {
                return Err(HnswError::Internal(format!(
                    "HNSW data file ({} bytes) too large for {} vectors (max {} bytes)",
                    data_meta.len(),
                    id_map.len(),
                    expected_max_data
                )));
            }
        }

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
            ef_search: super::ef_search(),
            dim,
            _lock_file: Some(lock_file),
        })
    }

    /// Check if an HNSW index exists at the given path
    pub fn exists(dir: &Path, basename: &str) -> bool {
        let graph_path = dir.join(format!("{}.hnsw.graph", basename));
        let data_path = dir.join(format!("{}.hnsw.data", basename));
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));

        graph_path.exists() && data_path.exists() && id_map_path.exists()
    }

    /// Get vector count without loading the full index (fast, for stats).
    ///
    /// Uses `BufReader` + `serde_json::from_reader` to avoid reading the entire
    /// id map file into a String first. The file is a JSON array of chunk ID strings.
    pub fn count_vectors(dir: &Path, basename: &str) -> Option<usize> {
        let id_map_path = dir.join(format!("{}.hnsw.ids", basename));
        let file = match std::fs::File::open(&id_map_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(
                    path = %id_map_path.display(),
                    error = %e,
                    "Could not read HNSW id map"
                );
                return None;
            }
        };
        // Acquire shared lock for consistency with load()
        if let Err(e) = file.lock_shared() {
            tracing::debug!(error = %e, "Could not lock HNSW id map");
            return None;
        }
        // Guard against oversized id map files
        const MAX_ID_MAP_SIZE: u64 = 100 * 1024 * 1024; // 100MB
        match file.metadata() {
            Ok(meta) if meta.len() > MAX_ID_MAP_SIZE => {
                tracing::warn!(
                    size_bytes = meta.len(),
                    path = %id_map_path.display(),
                    "HNSW id map too large"
                );
                return None;
            }
            Err(e) => {
                tracing::debug!(
                    path = %id_map_path.display(),
                    error = %e,
                    "Could not stat HNSW id map"
                );
                return None;
            }
            _ => {}
        }
        // Count array elements by streaming JSON without allocating all strings.
        // The id map is a JSON array of strings: ["id1","id2",...].
        // We iterate the stream and count SeqAccess elements rather than
        // deserializing into a Vec<String>.
        use serde::de::{Deserializer, SeqAccess, Visitor};
        use std::fmt;

        struct CountVisitor;
        impl<'de> Visitor<'de> for CountVisitor {
            type Value = usize;
            /// Writes a human-readable description of the expected type to the given formatter.
            ///
            /// # Arguments
            ///
            /// * `f` - The formatter to write the description to
            ///
            /// # Returns
            ///
            /// A `fmt::Result` indicating whether the write operation succeeded
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an array")
            }
            /// Counts the number of elements in a sequence by iterating through all elements.
            ///
            /// # Arguments
            /// * `seq` - A sequence accessor that provides access to elements in the sequence
            ///
            /// # Returns
            /// Returns `Ok(count)` where `count` is the total number of elements in the sequence, or an error if deserialization fails during iteration.
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<usize, A::Error> {
                let mut count = 0usize;
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    count += 1;
                }
                Ok(count)
            }
        }

        let reader = std::io::BufReader::new(file);
        let mut de = serde_json::Deserializer::from_reader(reader);
        match de.deserialize_seq(CountVisitor) {
            Ok(count) => Some(count),
            Err(e) => {
                tracing::warn!(path = %id_map_path.display(), error = %e, "Corrupted HNSW id map");
                None
            }
        }
    }

    /// Load HNSW index with optional ef_search override and an explicit
    /// runtime dim.
    ///
    /// `dim` is required — it must match the dimension the index was built
    /// with. Mismatches surface as load errors rather than silent misreads.
    pub fn try_load_with_ef(
        cq_dir: &Path,
        ef_search: Option<usize>,
        dim: usize,
    ) -> Option<Box<dyn VectorIndex>> {
        Self::try_load_named(cq_dir, "index", ef_search, dim)
    }

    /// Phase 5: load the base (non-enriched) HNSW index.
    ///
    /// Returns `None` when `index_base.hnsw.*` files are absent or corrupt —
    /// the router treats that as a signal to fall back to the enriched index.
    ///
    /// `dim` is required for the same reasons as `try_load_with_ef`.
    pub fn try_load_base_with_ef(
        cq_dir: &Path,
        ef_search: Option<usize>,
        dim: usize,
    ) -> Option<Box<dyn VectorIndex>> {
        Self::try_load_named(cq_dir, "index_base", ef_search, dim)
    }

    /// Internal: load any named HNSW index (enriched, base, or future variants).
    fn try_load_named(
        cq_dir: &Path,
        basename: &str,
        ef_search: Option<usize>,
        dim: usize,
    ) -> Option<Box<dyn VectorIndex>> {
        if Self::exists(cq_dir, basename) {
            match Self::load_with_dim(cq_dir, basename, dim) {
                Ok(mut index) => {
                    if let Some(ef) = ef_search {
                        index.set_ef_search(ef);
                        tracing::debug!(
                            basename = basename,
                            ef_search = ef,
                            "Applied config ef_search override"
                        );
                    }
                    tracing::info!(
                        basename = basename,
                        vectors = index.len(),
                        "HNSW index loaded"
                    );
                    Some(Box::new(index))
                }
                Err(e) => {
                    tracing::warn!(
                        basename = basename,
                        error = %e,
                        "HNSW index corrupted or incomplete — falling back to brute-force search. \
                         Run 'cqs index' to rebuild."
                    );
                    None
                }
            }
        } else {
            tracing::debug!(basename = basename, "No HNSW index found");
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

        match HnswIndex::load_with_dim(tmp.path(), "test", crate::EMBEDDING_DIM) {
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

        match HnswIndex::load_with_dim(tmp.path(), "test", crate::EMBEDDING_DIM) {
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
    fn test_load_rejects_data_too_large_for_id_map() {
        let tmp = TempDir::new().unwrap();

        let graph_path = tmp.path().join("test.hnsw.graph");
        let data_path = tmp.path().join("test.hnsw.data");
        let ids_path = tmp.path().join("test.hnsw.ids");

        std::fs::write(&graph_path, b"dummy").unwrap();

        // id_map claims 2 vectors, but data file is far larger than
        // 2 * 768 * 4 * 2 = 12,288 bytes would allow
        std::fs::write(&ids_path, r#"["a","b"]"#).unwrap();
        let f = std::fs::File::create(&data_path).unwrap();
        f.set_len(1_000_000).unwrap(); // ~1MB >> 12KB limit
        write_checksums(tmp.path(), "test");

        match HnswIndex::load_with_dim(tmp.path(), "test", crate::EMBEDDING_DIM) {
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("data file") && msg.contains("too large for"),
                    "Expected data/id_map size mismatch error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected error for data file exceeding id_map capacity"),
        }
    }

    #[test]
    fn test_load_rejects_missing_checksum() {
        let tmp = TempDir::new().unwrap();

        std::fs::write(tmp.path().join("test.hnsw.graph"), b"data").unwrap();
        std::fs::write(tmp.path().join("test.hnsw.data"), b"data").unwrap();
        std::fs::write(tmp.path().join("test.hnsw.ids"), b"[]").unwrap();
        // No checksum file

        match HnswIndex::load_with_dim(tmp.path(), "test", crate::EMBEDDING_DIM) {
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

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
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

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), basename).unwrap();

        // Load twice — shared locks should not block each other
        let loaded1 = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).unwrap();
        let loaded2 = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).unwrap();
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
        let result = HnswIndex::load_with_dim(dir.path(), basename, crate::EMBEDDING_DIM);
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

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), "index").unwrap();

        assert!(HnswIndex::exists(tmp.path(), "index"));

        let loaded = HnswIndex::load_with_dim(tmp.path(), "index", crate::EMBEDDING_DIM).unwrap();
        assert_eq!(loaded.len(), 2);

        // Verify search still works
        let query = make_embedding(1);
        let results = loaded.search(&query, 2);
        assert_eq!(results[0].id, "chunk1");
    }

    // ===== TC-31: multi-model dim-threading (HNSW persist) =====

    /// Create a deterministic normalized embedding of arbitrary dimension.
    fn make_embedding_dim(seed: u32, dim: usize) -> crate::embedder::Embedding {
        let mut v = vec![0.0f32; dim];
        for (i, val) in v.iter_mut().enumerate() {
            *val = ((seed as f32 * 0.1) + (i as f32 * 0.001)).sin();
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut v {
                *val /= norm;
            }
        }
        crate::embedder::Embedding::new(v)
    }

    #[test]
    fn tc31_save_and_load_with_dim_1024() {
        // TC-31.5: Save a 1024-dim HNSW index, load with load_with_dim(1024),
        // verify it loads and searches correctly.
        let tmp = TempDir::new().unwrap();
        let basename = "test_1024";

        let embeddings: Vec<(String, crate::embedder::Embedding)> = (1..=5)
            .map(|i| (format!("vec{}", i), make_embedding_dim(i, 1024)))
            .collect();

        let index = HnswIndex::build_with_dim(embeddings, 1024).unwrap();
        assert_eq!(index.dim, 1024);
        index.save(tmp.path(), basename).unwrap();

        // Load with matching dim
        let loaded = HnswIndex::load_with_dim(tmp.path(), basename, 1024).unwrap();
        assert_eq!(loaded.len(), 5, "Loaded index should have 5 vectors");
        assert_eq!(loaded.dim, 1024, "Loaded index dim should be 1024");

        // Search should work correctly
        let query = make_embedding_dim(1, 1024);
        let results = loaded.search(&query, 3);
        assert!(!results.is_empty(), "Search should return results");
        assert_eq!(results[0].id, "vec1", "Nearest neighbor should be vec1");
    }

    #[test]
    fn tc31_load_with_wrong_dim_data_size_rejected() {
        // TC-31.6: Build with dim=1024, try to load with a much smaller dim.
        // The SEC-7 check: expected_max_data = id_map.len() * dim * sizeof(f32) * 2
        // We use dim=128 for the load so the expected_max is small enough that
        // the actual data file (sized for 1024-dim vectors + HNSW overhead)
        // exceeds it, triggering the "data file too large" error.
        //
        // Math: 20 vectors * 128 * 4 * 2 = 20,480 expected_max
        //       Actual file: 20 * 1024 * 4 + HNSW overhead >> 20,480
        let tmp = TempDir::new().unwrap();
        let basename = "test_dim_mismatch";

        let embeddings: Vec<(String, crate::embedder::Embedding)> = (1..=20)
            .map(|i| (format!("vec{}", i), make_embedding_dim(i, 1024)))
            .collect();

        let index = HnswIndex::build_with_dim(embeddings, 1024).unwrap();
        index.save(tmp.path(), basename).unwrap();

        // Load with much smaller dim — expected_max_data will be far too small
        let result = HnswIndex::load_with_dim(tmp.path(), basename, 128);
        assert!(
            result.is_err(),
            "Loading 1024-dim index with dim=128 should fail due to data size mismatch"
        );
        let err_msg = match result {
            Err(e) => format!("{}", e),
            Ok(_) => panic!("Expected error, got Ok"),
        };
        assert!(
            err_msg.contains("too large"),
            "Error should mention data file size: {}",
            err_msg
        );
    }

    /// Phase 5: `try_load_base_with_ef` returns `None` when the index_base
    /// files don't exist (fresh-migration state). The caller treats this as
    /// "fall back to enriched index".
    #[test]
    fn test_try_load_base_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        // No index_base.* files written; only the enriched index.
        let embeddings: Vec<(String, crate::embedder::Embedding)> = (1..=10)
            .map(|i| (format!("vec{}", i), make_embedding(i)))
            .collect();
        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), "index").unwrap();

        // Enriched load should succeed.
        let enriched = HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM);
        assert!(enriched.is_some(), "enriched HNSW should load");

        // Base load should return None — no index_base.* files exist.
        let base = HnswIndex::try_load_base_with_ef(tmp.path(), None, crate::EMBEDDING_DIM);
        assert!(
            base.is_none(),
            "base HNSW should return None when index_base files are absent"
        );
    }

    /// Phase 5: `try_load_base_with_ef` succeeds when index_base files exist.
    /// Verifies the basename routing is correct — loading "index_base" when
    /// the base files are present and "index" when only enriched is present.
    #[test]
    fn test_try_load_base_loads_when_present() {
        let tmp = TempDir::new().unwrap();
        let embeddings: Vec<(String, crate::embedder::Embedding)> = (1..=10)
            .map(|i| (format!("vec{}", i), make_embedding(i)))
            .collect();
        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), "index_base").unwrap();

        // Base load succeeds.
        let base = HnswIndex::try_load_base_with_ef(tmp.path(), None, crate::EMBEDDING_DIM);
        assert!(base.is_some(), "base HNSW should load when files present");
        assert_eq!(base.unwrap().len(), 10);

        // Enriched should still return None — only the base files exist.
        let enriched = HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM);
        assert!(
            enriched.is_none(),
            "enriched should return None when only index_base files exist"
        );
    }
}
