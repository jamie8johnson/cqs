//! In-memory inverted index for SPLADE sparse vectors.
//!
//! Loaded from SQLite at startup, queried during search.
//! Supports filtered search with the same chunk_type/language predicate
//! used by HNSW traversal-time filtering.
//!
//! ## Persistence
//!
//! The index also has an on-disk format mirroring the HNSW persistence
//! pattern. Build-from-SQLite is slow (7.58M postings for SPLADE-Code 0.6B
//! = ~45s per CLI invocation), so we serialize the built index alongside
//! the HNSW files and load it in a single read on subsequent invocations.
//! Invalidation is driven by a `splade_generation` counter in the `metadata`
//! table, bumped on every write to `sparse_vectors`; the generation is
//! embedded in the file header so loads from a stale file are detected
//! and fall back to rebuild-from-SQLite.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use crate::index::IndexResult;

use super::SparseVector;

/// File magic for the SPLADE persisted index.
const SPLADE_INDEX_MAGIC: &[u8; 4] = b"SPDX";

/// Format version. Bump when the on-disk layout changes.
const SPLADE_INDEX_VERSION: u32 = 1;

/// Canonical filename for the persisted SPLADE index inside the project's
/// `.cqs/` directory. Lives alongside the HNSW files so the whole index
/// dir moves as a unit.
pub const SPLADE_INDEX_FILENAME: &str = "splade.index.bin";

/// Fixed header size in bytes: magic(4) + version(4) + generation(8)
/// + chunk_count(8) + token_count(8) + body_checksum(32) = 64 bytes.
const SPLADE_INDEX_HEADER_LEN: usize = 64;

/// Default cap on `splade.index.bin` file size read at load time.
/// Audit RB-2: without an upper bound `read_to_end` could unbounded-alloc
/// from a corrupted or maliciously-grown file. 2 GB leaves ~20× headroom
/// over SPLADE-Code 0.6B on a cqs-sized project (~100 MB). Env override:
/// `CQS_SPLADE_MAX_INDEX_BYTES`.
const DEFAULT_SPLADE_MAX_INDEX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Read `CQS_SPLADE_MAX_INDEX_BYTES` env var, fall back to default. Cached
/// via `OnceLock` to avoid re-parsing per load call.
fn splade_max_index_bytes() -> u64 {
    static CACHED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| match std::env::var("CQS_SPLADE_MAX_INDEX_BYTES") {
        Ok(val) => match val.parse::<u64>() {
            Ok(n) if n > 0 => {
                tracing::info!(max_bytes = n, "CQS_SPLADE_MAX_INDEX_BYTES override");
                n
            }
            _ => {
                tracing::warn!(
                    value = %val,
                    "Invalid CQS_SPLADE_MAX_INDEX_BYTES, using default 2GB"
                );
                DEFAULT_SPLADE_MAX_INDEX_BYTES
            }
        },
        Err(_) => DEFAULT_SPLADE_MAX_INDEX_BYTES,
    })
}

/// Errors specific to SpladeIndex persistence.
///
/// Audit EH-4 / API-8 / API-9: prior to v1.22.0 audit, five distinct
/// structural corruption conditions (chunk id > u32::MAX, posting list >
/// u32::MAX, chunk_idx > u32::MAX, chunk_count overflow, invalid utf-8 in
/// chunk id, out-of-bounds posting chunk_idx) were all wrapped as
/// `Io(io::Error::new(InvalidData, ...))`. That made the enum less
/// expressive than the dedicated variants already in place and produced
/// nonsense Display output ("io: chunk id exceeds u32::MAX bytes: …").
/// They now route through [`CorruptData`], which is structurally distinct
/// from actual I/O failures. The [`ChecksumMismatch`] variant gained
/// `path`, `expected`, `actual` fields to match `HnswError::ChecksumMismatch`.
/// [`FileTooLarge`] is new and covers audit RB-2 (unbounded allocation from
/// an oversized on-disk file).
#[derive(thiserror::Error, Debug)]
pub enum SpladeIndexPersistError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("SPLADE index file has wrong magic (not a SPLADE index)")]
    BadMagic,
    #[error("SPLADE index file version {0} not supported by this build (expected {1})")]
    UnsupportedVersion(u32, u32),
    #[error(
        "SPLADE index generation {disk} does not match store generation {store} — \
         sparse_vectors have been modified since the index was persisted"
    )]
    GenerationMismatch { disk: u64, store: u64 },
    #[error(
        "SPLADE index body checksum mismatch — file {path} is corrupt \
         (expected {expected}, got {actual})"
    )]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("SPLADE index file truncated — expected more data at offset {0}")]
    Truncated(u64),
    #[error("SPLADE index payload corrupt: {0}")]
    CorruptData(String),
    #[error(
        "SPLADE index file {path} is {size} bytes, exceeds maximum {limit} bytes. \
         Set CQS_SPLADE_MAX_INDEX_BYTES to override."
    )]
    FileTooLarge { path: String, size: u64, limit: u64 },
}

/// In-memory inverted index for sparse vector search.
///
/// Structure: `token_id → [(chunk_index, weight)]`. For each vocabulary
/// token, stores which chunks contain it and how important it is.
pub struct SpladeIndex {
    /// Inverted postings: token_id → [(chunk_index, weight)]
    postings: HashMap<u32, Vec<(usize, f32)>>,
    /// Sequential chunk ID map (chunk_index → chunk_id string)
    id_map: Vec<String>,
}

impl SpladeIndex {
    /// Build from a list of (chunk_id, sparse_vector) pairs.
    pub fn build(chunks: Vec<(String, SparseVector)>) -> Self {
        let _span = tracing::info_span!("splade_index_build", chunks = chunks.len()).entered();

        let mut postings: HashMap<u32, Vec<(usize, f32)>> = HashMap::new();
        let mut id_map = Vec::with_capacity(chunks.len());

        for (idx, (chunk_id, sparse)) in chunks.into_iter().enumerate() {
            for &(token_id, weight) in &sparse {
                postings.entry(token_id).or_default().push((idx, weight));
            }
            id_map.push(chunk_id);
        }

        tracing::info!(
            unique_tokens = postings.len(),
            chunks = id_map.len(),
            "SPLADE index built"
        );

        Self { postings, id_map }
    }

    /// Search the inverted index (unfiltered).
    pub fn search(&self, query: &SparseVector, k: usize) -> Vec<IndexResult> {
        self.search_with_filter(query, k, &|_: &str| true)
    }

    /// Search with a chunk_id predicate filter.
    ///
    /// Computes dot product between query sparse vector and each document's
    /// sparse vector via the inverted index. Non-matching chunks (per filter)
    /// are skipped during score accumulation.
    pub fn search_with_filter(
        &self,
        query: &SparseVector,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        let _span = tracing::debug_span!(
            "splade_index_search",
            k,
            query_terms = query.len(),
            index_size = self.id_map.len()
        )
        .entered();

        if query.is_empty() || self.id_map.is_empty() {
            return Vec::new();
        }

        // Accumulate dot product scores per chunk
        let mut scores: HashMap<usize, f32> = HashMap::new();
        for &(token_id, query_weight) in query {
            if let Some(posting_list) = self.postings.get(&token_id) {
                for &(chunk_idx, doc_weight) in posting_list {
                    // Apply filter (PF-13: direct indexing — idx always valid by construction)
                    if chunk_idx >= self.id_map.len() || !filter(&self.id_map[chunk_idx]) {
                        continue;
                    }
                    *scores.entry(chunk_idx).or_insert(0.0) += query_weight * doc_weight;
                }
            }
        }

        // Sort by score descending, take top-k
        let mut results: Vec<_> = scores
            .into_iter()
            .filter_map(|(idx, score)| {
                self.id_map.get(idx).map(|id| IndexResult {
                    id: id.clone(),
                    score,
                })
            })
            .collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);

        tracing::debug!(results = results.len(), "SPLADE search complete");
        results
    }

    /// Number of chunks in the index.
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }

    /// Number of unique tokens in the index.
    pub fn unique_tokens(&self) -> usize {
        self.postings.len()
    }

    /// Serialize the index to `path` with the given generation counter.
    ///
    /// Writes atomically via a temp file + rename so a crash mid-save leaves
    /// the old file untouched. The file layout is:
    ///
    /// ```text
    /// Header (64 bytes):
    ///   [0..4]   magic "SPDX"
    ///   [4..8]   format version (u32 LE)
    ///   [8..16]  generation (u64 LE)
    ///   [16..24] chunk count (u64 LE)
    ///   [24..32] unique token count (u64 LE)
    ///   [32..64] blake3-256 of body
    ///
    /// Body:
    ///   id_map section:
    ///     for each chunk in insertion order:
    ///       u32 LE  id length (bytes)
    ///       N bytes id (utf-8, not null-terminated)
    ///   postings section:
    ///     for each unique token (HashMap iteration order — non-deterministic
    ///     across builds; the body checksum still matches because we hash
    ///     what we actually wrote):
    ///       u32 LE  token_id
    ///       u32 LE  posting count
    ///       for each posting (count times):
    ///         u32 LE  chunk_idx
    ///         f32 LE  weight
    /// ```
    ///
    /// The body is built in memory (~60-100MB for SPLADE-Code 0.6B on a
    /// cqs-sized project) so we can hash and write in one pass. That's the
    /// same memory footprint we already hold for the in-memory index itself,
    /// so no new budget is introduced.
    pub fn save(&self, path: &Path, generation: u64) -> Result<(), SpladeIndexPersistError> {
        let _span = tracing::info_span!(
            "splade_index_save",
            path = %path.display(),
            generation,
            chunks = self.id_map.len(),
            tokens = self.postings.len(),
        )
        .entered();

        // Build the body into a Vec<u8> so we can hash it in one pass and
        // write it without an extra seek-back step on the real file.
        let mut body: Vec<u8> = Vec::with_capacity(Self::estimate_body_size(
            self.id_map.len(),
            self.postings.values().map(|v| v.len()).sum::<usize>(),
        ));

        // id_map
        for id in &self.id_map {
            let len_u32: u32 = id.len().try_into().map_err(|_| {
                // Audit EH-4: these are structural invariants, not I/O errors.
                SpladeIndexPersistError::CorruptData(format!(
                    "chunk id exceeds u32::MAX bytes: {}",
                    id.len()
                ))
            })?;
            body.extend_from_slice(&len_u32.to_le_bytes());
            body.extend_from_slice(id.as_bytes());
        }

        // postings
        for (&token_id, posting_list) in &self.postings {
            body.extend_from_slice(&token_id.to_le_bytes());
            let count_u32: u32 = posting_list.len().try_into().map_err(|_| {
                SpladeIndexPersistError::CorruptData(format!(
                    "posting list for token {} exceeds u32::MAX entries: {}",
                    token_id,
                    posting_list.len()
                ))
            })?;
            body.extend_from_slice(&count_u32.to_le_bytes());
            for &(chunk_idx, weight) in posting_list {
                let idx_u32: u32 = chunk_idx.try_into().map_err(|_| {
                    SpladeIndexPersistError::CorruptData(format!(
                        "chunk_idx exceeds u32::MAX: {}",
                        chunk_idx
                    ))
                })?;
                body.extend_from_slice(&idx_u32.to_le_bytes());
                body.extend_from_slice(&weight.to_le_bytes());
            }
        }

        // Build the header FIRST (without the checksum), so we can include it
        // in the hash — audit RB-1: previously only the body was hashed, which
        // meant a single bit flip in the unhashed header `chunk_count` could
        // pass integrity checks and cause `Vec::with_capacity(usize::MAX)` to
        // panic inside `load()`. Now the hash covers bytes [0..32] of the
        // header AND the body, so any header corruption is detected at load
        // time. The hash field itself (bytes [32..64]) can't cover itself.
        let mut header = [0u8; SPLADE_INDEX_HEADER_LEN];
        header[0..4].copy_from_slice(SPLADE_INDEX_MAGIC);
        header[4..8].copy_from_slice(&SPLADE_INDEX_VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&generation.to_le_bytes());
        header[16..24].copy_from_slice(&(self.id_map.len() as u64).to_le_bytes());
        header[24..32].copy_from_slice(&(self.postings.len() as u64).to_le_bytes());

        // Hash header[0..32] || body in one go.
        let mut hasher = blake3::Hasher::new();
        hasher.update(&header[0..32]);
        hasher.update(&body);
        let combined_hash = hasher.finalize();
        header[32..64].copy_from_slice(combined_hash.as_bytes());

        // Atomic write: write to a same-directory temp file, fsync, rename.
        let parent = path.parent().ok_or_else(|| {
            SpladeIndexPersistError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("SPLADE index path has no parent: {}", path.display()),
            ))
        })?;
        std::fs::create_dir_all(parent)?;

        // Audit PB-NEW-9: use `to_string_lossy()` instead of
        // `to_str().unwrap_or(...)` so non-UTF-8 path components produce a
        // unique-ish temp name rather than collapsing to a shared fallback
        // that could collide across concurrent saves.
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_else(|| "splade.index".into());
        // Randomized suffix so two concurrent saves don't clobber each other's
        // temp file. Same pattern as the HNSW save path.
        let suffix = crate::temp_suffix();
        let tmp_path = parent.join(format!(".{}.{:016x}.tmp", file_name, suffix));

        {
            let file = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(0o600)
                        .open(&tmp_path)?
                }
                #[cfg(not(unix))]
                {
                    std::fs::File::create(&tmp_path)?
                }
            };
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(&header)?;
            writer.write_all(&body)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }

        // Atomic rename. Audit PB-NEW-3: the previous Windows branch did
        // `remove_file` then `rename`, which was (a) redundant — Rust's
        // `std::fs::rename` on Windows uses `MoveFileExW` with
        // `MOVEFILE_REPLACE_EXISTING` since 1.46, so rename-over-existing
        // works natively; (b) actively harmful — the remove+rename
        // sequence opened a crash window where neither the old nor the new
        // file existed, and a `SHARING_VIOLATION` on the remove (from
        // another process mmapping the target) broke the save entirely.
        // Deleted the whole `#[cfg(windows)]` block.
        //
        // Audit PB-NEW-4: cross-device rename fallback — on WSL 9P / Docker
        // overlayfs / NFS the rename can fail with `CrossesDevices` or
        // permission errors even within a single path. Mirror the
        // `src/hnsw/persist.rs:412-445` pattern: try rename first, fall
        // back to `fs::copy` + `set_permissions(0o600)` + remove(tmp) on
        // error.
        if let Err(rename_err) = std::fs::rename(&tmp_path, path) {
            tracing::warn!(
                error = %rename_err,
                from = %tmp_path.display(),
                to = %path.display(),
                "SPLADE index rename failed, attempting fs::copy fallback"
            );
            std::fs::copy(&tmp_path, path).map_err(|copy_err| {
                SpladeIndexPersistError::Io(std::io::Error::new(
                    copy_err.kind(),
                    format!(
                        "rename failed ({}) AND copy fallback failed ({})",
                        rename_err, copy_err
                    ),
                ))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            // Best-effort temp cleanup — we already got the target file in
            // place, so a leftover tmp is non-fatal.
            let _ = std::fs::remove_file(&tmp_path);
        }

        // Audit PB-NEW-5: fsync the parent directory on unix so the rename
        // is durable across a power cut. NTFS journals metadata with the
        // rename itself, so Windows doesn't need this. Best-effort; logged
        // on failure because the rebuild path handles lost persists.
        #[cfg(unix)]
        {
            if let Ok(dir) = std::fs::File::open(parent) {
                if let Err(e) = dir.sync_all() {
                    tracing::debug!(
                        error = %e,
                        parent = %parent.display(),
                        "parent dir fsync failed after SPLADE save — save is persisted but not \
                         guaranteed durable across power loss (rebuildable, so low severity)"
                    );
                }
            }
        }

        tracing::info!(
            path = %path.display(),
            bytes = SPLADE_INDEX_HEADER_LEN + body.len(),
            "SPLADE index persisted"
        );
        Ok(())
    }

    /// Attempt to load a persisted index from `path`.
    ///
    /// If the file is missing the function returns `Ok(None)`. If the file
    /// exists but is unreadable, corrupt, or stale relative to
    /// `expected_generation`, returns an `Err` describing the reason; the
    /// caller is expected to fall back to rebuild-from-SQLite and re-persist.
    ///
    /// Safety guards (audit cluster):
    /// - RB-2: file size capped at `CQS_SPLADE_MAX_INDEX_BYTES` (default 2 GB)
    ///   before `read_to_end`, so an attacker or corruption can't trigger an
    ///   unbounded allocation
    /// - RB-1: blake3 hash covers header[0..32] + body, so any header bit
    ///   flip (not just body) is detected before `Vec::with_capacity` is
    ///   called on chunk_count / token_count
    /// - RM-4: orphan temp files from previous crashed saves are cleaned up
    ///   at the top of `load()`, mirroring the HNSW pattern
    /// - EH-4 / API-8: corrupt-data conditions route through the dedicated
    ///   `CorruptData` variant, and `ChecksumMismatch` carries `path` /
    ///   `expected` / `actual` hex fields instead of a unit variant
    pub fn load(
        path: &Path,
        expected_generation: u64,
    ) -> Result<Option<Self>, SpladeIndexPersistError> {
        let _span = tracing::info_span!(
            "splade_index_load",
            path = %path.display(),
            expected_generation,
        )
        .entered();

        // Audit RM-4: clean up orphan `.splade.index.bin.*.tmp` temp files
        // left by previous crashed saves, mirroring HNSW's cleanup loop.
        // Best-effort: errors are logged but don't fail the load.
        Self::cleanup_orphan_temp_files(path);

        // Audit RB-2: cap file size BEFORE read_to_end. Env override
        // `CQS_SPLADE_MAX_INDEX_BYTES` for cases where a genuine 2+ GB
        // index is expected (huge corpus with SPLADE-Code 0.6B).
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("SPLADE index file absent, will rebuild");
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let file_size = metadata.len();
        let size_limit = splade_max_index_bytes();
        if file_size > size_limit {
            return Err(SpladeIndexPersistError::FileTooLarge {
                path: path.display().to_string(),
                size: file_size,
                limit: size_limit,
            });
        }
        if (file_size as usize) < SPLADE_INDEX_HEADER_LEN {
            return Err(SpladeIndexPersistError::Truncated(file_size));
        }

        let file = std::fs::File::open(path)?;
        let mut reader = std::io::BufReader::new(file);

        // Header.
        let mut header = [0u8; SPLADE_INDEX_HEADER_LEN];
        reader.read_exact(&mut header).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                SpladeIndexPersistError::Truncated(0)
            } else {
                SpladeIndexPersistError::Io(e)
            }
        })?;

        if &header[0..4] != SPLADE_INDEX_MAGIC {
            return Err(SpladeIndexPersistError::BadMagic);
        }
        let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if version != SPLADE_INDEX_VERSION {
            return Err(SpladeIndexPersistError::UnsupportedVersion(
                version,
                SPLADE_INDEX_VERSION,
            ));
        }
        let disk_generation = u64::from_le_bytes(header[8..16].try_into().unwrap());
        if disk_generation != expected_generation {
            return Err(SpladeIndexPersistError::GenerationMismatch {
                disk: disk_generation,
                store: expected_generation,
            });
        }
        let chunk_count = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let token_count = u64::from_le_bytes(header[24..32].try_into().unwrap());
        let stored_hash: [u8; 32] = header[32..64].try_into().unwrap();

        // Audit PF-4: pre-allocate the body Vec from known file size. The
        // previous `Vec::new()` caused ~log₂(59MB) reallocations on a typical
        // SPLADE-Code 0.6B index, ~100ms of wasted memcpy per warm query.
        let body_len = (file_size as usize).saturating_sub(SPLADE_INDEX_HEADER_LEN);
        let mut body = Vec::with_capacity(body_len);
        reader.read_to_end(&mut body)?;

        // Audit RB-1: hash covers header[0..32] + body. Previously only the
        // body was hashed, so flipping a bit in `chunk_count` (header bytes
        // [16..24]) passed the integrity check and reached
        // `Vec::with_capacity(usize::MAX)` → process panic.
        let mut hasher = blake3::Hasher::new();
        hasher.update(&header[0..32]);
        hasher.update(&body);
        let actual_hash = hasher.finalize();
        if actual_hash.as_bytes() != &stored_hash {
            // Use blake3::Hash::to_hex for the expected hex encoding so we
            // don't pull in the `hex` crate just for this one call.
            let expected_hex = blake3::Hash::from_bytes(stored_hash).to_hex().to_string();
            return Err(SpladeIndexPersistError::ChecksumMismatch {
                path: path.display().to_string(),
                expected: expected_hex,
                actual: actual_hash.to_hex().to_string(),
            });
        }

        // Parse body. After the combined-header-hash check above, both
        // chunk_count and token_count are known to be authentic from the
        // author's perspective — but we still apply a loose sanity bound
        // to defend against pre-v1.22.0 files that were written under the
        // old unhashed-header scheme and may have been corrupted in that
        // window. Every chunk consumes >= 4 bytes for its length prefix
        // and every token entry consumes >= 8 bytes, so these are hard
        // upper bounds on feasible counts given the body length.
        let chunk_count_usize: usize = chunk_count.try_into().map_err(|_| {
            SpladeIndexPersistError::CorruptData(format!(
                "chunk_count {} does not fit in usize",
                chunk_count
            ))
        })?;
        if chunk_count_usize > body.len() / 4 {
            return Err(SpladeIndexPersistError::CorruptData(format!(
                "chunk_count {} exceeds feasible bound from body length {}",
                chunk_count_usize,
                body.len()
            )));
        }
        let mut id_map: Vec<String> = Vec::with_capacity(chunk_count_usize);
        let mut cursor: usize = 0;

        fn need(body: &[u8], cursor: usize, n: usize) -> Result<(), SpladeIndexPersistError> {
            if cursor.saturating_add(n) > body.len() {
                Err(SpladeIndexPersistError::Truncated(cursor as u64))
            } else {
                Ok(())
            }
        }

        for _ in 0..chunk_count_usize {
            need(&body, cursor, 4)?;
            let len = u32::from_le_bytes(body[cursor..cursor + 4].try_into().unwrap()) as usize;
            cursor += 4;
            need(&body, cursor, len)?;
            // Audit PF-5: `.to_string()` here allocates an owned String from
            // a `&str` borrow into `body`. This is inherent — `id_map` owns
            // its strings and the source is a transient byte-slice reference.
            // `String::from` would be equivalent; there is no zero-copy path
            // because `body` is dropped after parsing completes.
            let id = std::str::from_utf8(&body[cursor..cursor + len])
                .map_err(|e| {
                    SpladeIndexPersistError::CorruptData(format!(
                        "chunk id is not valid utf-8: {}",
                        e
                    ))
                })?
                .to_string();
            cursor += len;
            id_map.push(id);
        }

        let token_count_usize: usize = token_count.try_into().map_err(|_| {
            SpladeIndexPersistError::CorruptData(format!(
                "token_count {} does not fit in usize",
                token_count
            ))
        })?;
        if token_count_usize > body.len() / 8 {
            return Err(SpladeIndexPersistError::CorruptData(format!(
                "token_count {} exceeds feasible bound from body length {}",
                token_count_usize,
                body.len()
            )));
        }
        let mut postings: HashMap<u32, Vec<(usize, f32)>> =
            HashMap::with_capacity(token_count_usize);

        for _ in 0..token_count_usize {
            need(&body, cursor, 8)?;
            let token_id = u32::from_le_bytes(body[cursor..cursor + 4].try_into().unwrap());
            cursor += 4;
            let posting_count =
                u32::from_le_bytes(body[cursor..cursor + 4].try_into().unwrap()) as usize;
            cursor += 4;
            need(&body, cursor, posting_count.saturating_mul(8))?;
            let mut postings_for_token: Vec<(usize, f32)> = Vec::with_capacity(posting_count);
            for _ in 0..posting_count {
                let chunk_idx =
                    u32::from_le_bytes(body[cursor..cursor + 4].try_into().unwrap()) as usize;
                cursor += 4;
                let weight = f32::from_le_bytes(body[cursor..cursor + 4].try_into().unwrap());
                cursor += 4;
                if chunk_idx >= id_map.len() {
                    return Err(SpladeIndexPersistError::CorruptData(format!(
                        "posting chunk_idx {} out of bounds for id_map len {}",
                        chunk_idx,
                        id_map.len()
                    )));
                }
                postings_for_token.push((chunk_idx, weight));
            }
            postings.insert(token_id, postings_for_token);
        }

        if cursor != body.len() {
            tracing::warn!(
                parsed = cursor,
                body_len = body.len(),
                "SPLADE index body has trailing bytes after parse — tolerating but format may be wrong"
            );
        }

        tracing::info!(
            chunks = id_map.len(),
            tokens = postings.len(),
            "SPLADE index loaded from disk"
        );
        Ok(Some(Self { postings, id_map }))
    }

    /// Audit RM-4: clean up `.splade.index.bin.*.tmp` orphan files left by
    /// crashed saves. Mirrors the HNSW cleanup at `hnsw/persist.rs:498-510`.
    /// Best-effort — errors are logged and not propagated, because a
    /// leftover tmp file is annoying but not fatal.
    fn cleanup_orphan_temp_files(path: &Path) {
        let parent = match path.parent() {
            Some(p) => p,
            None => return,
        };
        let target_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => return,
        };
        // Temp files are named `.<target>.<hex>.tmp`.
        let prefix = format!(".{}.", target_name);
        let entries = match std::fs::read_dir(parent) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    parent = %parent.display(),
                    "read_dir for orphan cleanup failed, skipping"
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue, // non-utf8 filename, leave it alone
            };
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                match std::fs::remove_file(entry.path()) {
                    Ok(_) => tracing::debug!(
                        orphan = %name,
                        "Removed orphan SPLADE temp file"
                    ),
                    Err(e) => tracing::debug!(
                        error = %e,
                        orphan = %name,
                        "Failed to remove orphan SPLADE temp file"
                    ),
                }
            }
        }
    }

    /// Convenience: load from disk if present and matching; otherwise build
    /// from the provided SQLite rows and persist. Returns the index and a
    /// flag indicating whether a rebuild happened.
    ///
    /// The caller is responsible for reading `expected_generation` from the
    /// store and passing the path next to the rest of the index files.
    pub fn load_or_build(
        path: &Path,
        expected_generation: u64,
        rows: impl FnOnce() -> Vec<(String, SparseVector)>,
    ) -> (Self, bool) {
        match Self::load(path, expected_generation) {
            Ok(Some(idx)) => return (idx, false),
            Ok(None) => {
                tracing::debug!("SPLADE index not on disk, building from store");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "SPLADE index on-disk load failed, rebuilding from store"
                );
            }
        }
        let vectors = rows();
        let idx = Self::build(vectors);
        // Best-effort persist; failure is logged and tolerated so search can
        // still proceed on the freshly-built in-memory index. Skip if the
        // index is empty — persisting an empty index creates a stub file
        // that gets reloaded as "no vectors" on next invocation, which is
        // correct but clutters the directory.
        if !idx.is_empty() {
            if let Err(e) = idx.save(path, expected_generation) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "SPLADE index persist failed, continuing with in-memory index only"
                );
            }
        }
        (idx, true)
    }

    /// Rough upper bound on the serialized body size so `save()` can allocate
    /// once. 4 bytes per chunk header + average id length (~60) +
    /// 8 bytes per posting + 8 bytes per token header.
    fn estimate_body_size(n_chunks: usize, n_postings: usize) -> usize {
        let id_estimate = n_chunks * (4 + 64);
        let postings_estimate = n_postings * 8 + n_chunks * 8;
        id_estimate + postings_estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_index() -> SpladeIndex {
        SpladeIndex::build(vec![
            ("chunk_a".to_string(), vec![(1, 0.5), (2, 0.3), (3, 0.8)]),
            ("chunk_b".to_string(), vec![(1, 0.7), (4, 0.6)]),
            ("chunk_c".to_string(), vec![(2, 0.9), (3, 0.1), (5, 0.4)]),
        ])
    }

    #[test]
    fn test_build_empty() {
        let index = SpladeIndex::build(vec![]);
        assert!(index.is_empty());
        assert_eq!(index.unique_tokens(), 0);
    }

    #[test]
    fn test_build_and_search() {
        let index = make_test_index();
        assert_eq!(index.len(), 3);

        // Query that matches token 1 (in chunk_a and chunk_b)
        let results = index.search(&vec![(1, 1.0)], 10);
        assert!(!results.is_empty());
        // chunk_b has weight 0.7 for token 1, chunk_a has 0.5
        assert_eq!(results[0].id, "chunk_b");
        assert_eq!(results[1].id, "chunk_a");
    }

    #[test]
    fn test_dot_product_correct() {
        let index = make_test_index();
        // Query: token 1 (w=1.0) + token 2 (w=1.0)
        // chunk_a: 1*0.5 + 1*0.3 = 0.8
        // chunk_b: 1*0.7 + 0 = 0.7
        // chunk_c: 0 + 1*0.9 = 0.9
        let results = index.search(&vec![(1, 1.0), (2, 1.0)], 10);
        assert_eq!(results[0].id, "chunk_c"); // 0.9
        assert!((results[0].score - 0.9).abs() < 1e-5);
        assert_eq!(results[1].id, "chunk_a"); // 0.8
        assert!((results[1].score - 0.8).abs() < 1e-5);
        assert_eq!(results[2].id, "chunk_b"); // 0.7
        assert!((results[2].score - 0.7).abs() < 1e-5);
    }

    #[test]
    fn test_search_filter() {
        let index = make_test_index();
        // Filter: only chunk_a
        let results = index.search_with_filter(&vec![(1, 1.0)], 10, &|id: &str| id == "chunk_a");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "chunk_a");
    }

    #[test]
    fn test_search_no_match() {
        let index = make_test_index();
        // Query with token not in index
        let results = index.search(&vec![(999, 1.0)], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_empty_query() {
        let index = make_test_index();
        let results = index.search(&vec![], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_respects_k() {
        let index = make_test_index();
        let results = index.search(&vec![(1, 1.0), (2, 1.0), (3, 1.0)], 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_persist_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let original = make_test_index();

        original.save(&path, 42).unwrap();
        let loaded = SpladeIndex::load(&path, 42).unwrap().unwrap();

        // Structural equivalence: id_map order + postings content.
        assert_eq!(loaded.id_map, original.id_map);
        assert_eq!(loaded.postings.len(), original.postings.len());
        for (token_id, postings) in &original.postings {
            let loaded_postings = loaded.postings.get(token_id).unwrap();
            assert_eq!(loaded_postings.len(), postings.len());
            // Each posting list is order-preserved within save/load so we can
            // compare element-wise.
            for (a, b) in loaded_postings.iter().zip(postings.iter()) {
                assert_eq!(a.0, b.0);
                assert!((a.1 - b.1).abs() < f32::EPSILON);
            }
        }

        // Query parity: running the same search on loaded vs original yields
        // identical results in both order and score.
        let q = vec![(1u32, 1.0f32), (2, 0.5)];
        let r_orig = original.search(&q, 10);
        let r_load = loaded.search(&q, 10);
        assert_eq!(r_orig.len(), r_load.len());
        for (a, b) in r_orig.iter().zip(r_load.iter()) {
            assert_eq!(a.id, b.id);
            assert!((a.score - b.score).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_persist_generation_mismatch_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let original = make_test_index();
        original.save(&path, 7).unwrap();

        match SpladeIndex::load(&path, 8) {
            Err(SpladeIndexPersistError::GenerationMismatch { disk, store }) => {
                assert_eq!(disk, 7);
                assert_eq!(store, 8);
            }
            Ok(_) => panic!("expected GenerationMismatch, got Ok"),
            Err(e) => panic!("expected GenerationMismatch, got {}", e),
        }

        // And a matching generation still loads.
        let reloaded = SpladeIndex::load(&path, 7).unwrap();
        assert!(reloaded.is_some());
    }

    #[test]
    fn test_persist_bad_magic_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        std::fs::write(&path, vec![0u8; SPLADE_INDEX_HEADER_LEN + 16]).unwrap();

        match SpladeIndex::load(&path, 0) {
            Err(SpladeIndexPersistError::BadMagic) => {}
            Ok(_) => panic!("expected BadMagic, got Ok"),
            Err(e) => panic!("expected BadMagic, got {}", e),
        }
    }

    #[test]
    fn test_persist_corrupt_body_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let original = make_test_index();
        original.save(&path, 1).unwrap();

        // Flip a byte in the body (past the header).
        let mut bytes = std::fs::read(&path).unwrap();
        let target = SPLADE_INDEX_HEADER_LEN + 4;
        bytes[target] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        match SpladeIndex::load(&path, 1) {
            Err(SpladeIndexPersistError::ChecksumMismatch { .. }) => {}
            Ok(_) => panic!("expected ChecksumMismatch, got Ok"),
            Err(e) => panic!("expected ChecksumMismatch, got {}", e),
        }
    }

    #[test]
    fn test_persist_missing_file_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.bin");
        let result = SpladeIndex::load(&path, 0).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_load_or_build_persists_on_first_call() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");

        // First call: no file exists, should build and persist.
        let (_idx1, rebuilt1) = SpladeIndex::load_or_build(&path, 5, || {
            vec![
                ("chunk_a".to_string(), vec![(1u32, 0.5f32)]),
                ("chunk_b".to_string(), vec![(1, 0.3), (2, 0.9)]),
            ]
        });
        assert!(rebuilt1, "first call should rebuild");
        assert!(path.exists(), "first call should persist the file");

        // Second call: file exists with matching generation, should load.
        let (idx2, rebuilt2) = SpladeIndex::load_or_build(&path, 5, || {
            panic!("closure should not run when the file is reusable")
        });
        assert!(!rebuilt2, "second call should load from disk");
        assert_eq!(idx2.len(), 2);

        // Third call with bumped generation: should rebuild from the closure.
        let rebuilt_called = std::sync::atomic::AtomicBool::new(false);
        let (_idx3, rebuilt3) = SpladeIndex::load_or_build(&path, 6, || {
            rebuilt_called.store(true, std::sync::atomic::Ordering::SeqCst);
            vec![("chunk_c".to_string(), vec![(3u32, 0.7f32)])]
        });
        assert!(rebuilt3);
        assert!(rebuilt_called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
