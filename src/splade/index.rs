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
use std::io::{Read, Seek, SeekFrom, Write};
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

/// Header prefix length in bytes: everything in the header EXCEPT the
/// blake3 checksum. The hash covers `header[0..HEADER_PREFIX_LEN] || body`
/// so a bit flip in the prefix (chunk_count, token_count, etc.) is
/// detected at load time; the checksum slot itself cannot hash itself.
const HEADER_PREFIX_LEN: usize = 32;

/// blake3-256 digest length in bytes, occupying bytes
/// `[HEADER_PREFIX_LEN..SPLADE_INDEX_HEADER_LEN]` of the on-disk header.
const CHECKSUM_LEN: usize = 32;

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

/// `true` if `CQS_SPLADE_NO_MMAP` is set to a truthy value (`1`, `true`,
/// `yes`, case-insensitive), forcing [`SpladeIndex::load`] onto the heap
/// `read_to_end` path even on a fast filesystem.
///
/// Escape hatch for environments where memory-mapping misbehaves but the
/// `is_slow_mmap_fs` mountinfo heuristic doesn't catch them (overlay/bind
/// mounts, exotic FUSE backends). Not cached — load is infrequent and the
/// var is cheap to read.
fn force_heap_read() -> bool {
    std::env::var("CQS_SPLADE_NO_MMAP")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
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

/// Tee-style wrapper that forwards writes to an inner writer while feeding
/// every byte through a blake3 hasher. Used by [`SpladeIndex::save`] so the
/// body can be streamed to disk and hashed in a single pass instead of being
/// materialized into a `Vec<u8>` — eliminates ~60-100MB peak-memory
/// duplication on SPLADE-Code 0.6B.
///
/// The hasher is pre-seeded with `header[0..32]` by the caller before any
/// body bytes are written, so the final hash matches the documented
/// invariant `blake3(header[0..32] || body)`.
struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: &'a mut blake3::Hasher,
}

impl<'a, W: Write> Write for HashingWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        // Only hash the bytes that were actually written. `write` is allowed
        // to perform a short write, and the invariant is that the hash
        // covers exactly the bytes that reach the underlying sink.
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// In-memory inverted index for sparse vector search.
///
/// Structure: `token_id → [(chunk_index, weight)]`. For each vocabulary
/// token, stores which chunks contain it and how important it is.
pub struct SpladeIndex {
    /// Inverted postings: token_id → [(chunk_index, weight)]
    postings: HashMap<u32, Vec<(usize, f32)>>,
    /// Sequential chunk ID map (chunk_index → chunk_id string).
    /// `Box<str>` rather than `String` saves 8 bytes per entry
    /// (24+len → 16+len) with zero ergonomic cost (`Box<str>` derefs to
    /// `&str` for read access). Build-once / read-many access pattern;
    /// `push` is the only mutator and always immediately followed by an
    /// `into_boxed_str()`-cheap conversion.
    id_map: Vec<Box<str>>,
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
            id_map.push(chunk_id.into_boxed_str());
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

        // Accumulate dot product scores per chunk.
        //
        // Pre-size to a sensible upper bound so we don't pay
        // 12-14 rehashes during accumulation. Bounded by both the corpus
        // (`id_map.len()`) and the query's reach (`query.len() * 256`,
        // assuming each query token's posting list rarely exceeds 256
        // matching docs in practice). For an 18k-chunk index with 100
        // query tokens this preallocates ~18k buckets vs growing from 0.
        let cap = self.id_map.len().min(query.len().saturating_mul(256));
        let mut scores: HashMap<usize, f32> = HashMap::with_capacity(cap);
        for &(token_id, query_weight) in query {
            if let Some(posting_list) = self.postings.get(&token_id) {
                for &(chunk_idx, doc_weight) in posting_list {
                    // Apply filter (direct indexing — idx always valid by construction)
                    if chunk_idx >= self.id_map.len() || !filter(&self.id_map[chunk_idx]) {
                        continue;
                    }
                    *scores.entry(chunk_idx).or_insert(0.0) += query_weight * doc_weight;
                }
            }
        }

        // Bounded heap keeps top-k in O(n log k) instead of the
        // full O(n log n) sort+truncate. `BoundedScoreHeap::into_sorted_vec`
        // applies the id tie-breaker so equal-score results are
        // deterministically ordered across process invocations (the HashMap
        // above iterates in random order).
        let mut heap = crate::search::scoring::BoundedScoreHeap::new(k);
        for (idx, score) in scores {
            // Gate the id clone behind a heap pre-flight so ~17,800 of
            // ~18,000 scored candidates skip the clone entirely when k=200.
            // Saves ~570 KB of String churn per search at 32-char chunk ids.
            if !heap.would_accept(score) {
                continue;
            }
            if let Some(id) = self.id_map.get(idx) {
                // `Box<str>` derefs to `&str`; one `String::from(&str)`
                // allocation per accepted candidate (gated by `would_accept`
                // above so most are skipped).
                heap.push(id.to_string(), score);
            }
        }
        let results: Vec<IndexResult> = heap
            .into_sorted_vec()
            .into_iter()
            .map(|(id, score)| IndexResult { id, score })
            .collect();

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
    /// Body bytes are streamed to the temp file as they are produced and
    /// hashed in-flight via [`HashingWriter`], keeping peak RSS bounded by
    /// the `BufWriter` capacity (~8 KiB) rather than materializing the whole
    /// body (~60-100MB for SPLADE-Code 0.6B on a cqs-sized project).
    ///
    /// `save()` first writes a placeholder header with a zero'd checksum
    /// field, streams the body, finalizes the hash of `header[0..32] || body`,
    /// then seeks back to offset 32 to stamp the real checksum into place.
    pub fn save(&self, path: &Path, generation: u64) -> Result<(), SpladeIndexPersistError> {
        let _span = tracing::info_span!(
            "splade_index_save",
            path = %path.display(),
            generation,
            chunks = self.id_map.len(),
            tokens = self.postings.len(),
        )
        .entered();

        // Build the fixed-prefix header (bytes [0..32]). The checksum at
        // [32..64] is stamped AFTER the body is streamed and hashed; we
        // seek back to the checksum offset at the very end.
        //
        // Audit RB-1: the hash covers bytes [0..32] of the header AND the
        // body, so any header corruption is detected at load time. The
        // hash field itself (bytes [32..64]) can't cover itself. This is
        // preserved here.
        let mut header_prefix = [0u8; HEADER_PREFIX_LEN];
        header_prefix[0..4].copy_from_slice(SPLADE_INDEX_MAGIC);
        header_prefix[4..8].copy_from_slice(&SPLADE_INDEX_VERSION.to_le_bytes());
        header_prefix[8..16].copy_from_slice(&generation.to_le_bytes());
        header_prefix[16..24].copy_from_slice(&(self.id_map.len() as u64).to_le_bytes());
        header_prefix[24..32].copy_from_slice(&(self.postings.len() as u64).to_le_bytes());

        // Atomic write: write to a same-directory temp file, fsync, rename.
        let parent = path.parent().ok_or_else(|| {
            SpladeIndexPersistError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("SPLADE index path has no parent: {}", path.display()),
            ))
        })?;
        std::fs::create_dir_all(parent)?;

        // Use `to_string_lossy()` instead of `to_str().unwrap_or(...)` so
        // non-UTF-8 path components produce a unique-ish temp name rather than
        // collapsing to a shared fallback that could collide across concurrent
        // saves.
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_else(|| "splade.index".into());
        // Randomized suffix so two concurrent saves don't clobber each other's
        // temp file. Same pattern as the HNSW save path.
        let suffix = crate::temp_suffix();
        let tmp_path = parent.join(format!(".{}.{:016x}.tmp", file_name, suffix));

        // Total bytes written (header + body) for the info log below. Needed
        // because we no longer hold `body.len()` after streaming.
        let total_bytes: u64;

        {
            let mut file = {
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

            // Phase 1: write the header prefix (bytes [0..32]) and a zero
            // placeholder for the checksum (bytes [32..64]) directly to the
            // raw file via a BufWriter. These bytes do NOT go through the
            // HashingWriter — the hasher is seeded with header_prefix
            // separately (below) and the checksum slot cannot hash itself.
            let mut writer = std::io::BufWriter::new(&mut file);
            writer.write_all(&header_prefix)?;
            writer.write_all(&[0u8; CHECKSUM_LEN])?;

            // Seed the hasher with the header prefix so the final digest
            // covers `header[0..32] || body`.
            let mut hasher = blake3::Hasher::new();
            hasher.update(&header_prefix);

            // Phase 2: stream the body through a HashingWriter that wraps
            // the BufWriter. Every subsequent `write_all` updates the
            // hasher with the exact bytes that land on disk. Nested scope
            // ensures the HashingWriter is dropped before we reach for the
            // underlying BufWriter again.
            let body_bytes = {
                let mut body_writer = HashingWriter {
                    inner: &mut writer,
                    hasher: &mut hasher,
                };
                Self::write_body(&mut body_writer, &self.id_map, &self.postings)?
            };

            // Flush the BufWriter and release it so `file.seek` below sees
            // every body byte before the checksum overwrite happens.
            writer.flush()?;
            drop(writer);

            // Phase 3: finalize the hash and stamp it into the checksum
            // slot at offset 32. seek() on the raw File is safe now that
            // the BufWriter is gone — no buffered bytes left behind.
            let combined_hash = hasher.finalize();
            file.seek(SeekFrom::Start(HEADER_PREFIX_LEN as u64))?;
            file.write_all(combined_hash.as_bytes())?;
            file.flush()?;
            file.sync_all()?;

            total_bytes = (SPLADE_INDEX_HEADER_LEN as u64).saturating_add(body_bytes);
        }

        // `.bak` rollback so a mid-save failure can't destroy the prior good
        // `splade.index.bin` (e.g. cross-device EXDEV running out of disk
        // after the source is promoted but before the rename completes). The
        // sequence:
        //   1. Refuse to save if a stale `.bak` already exists (crash recovery
        //      breadcrumb — operator must clear it manually).
        //   2. Rename the live `splade.index.bin` -> `splade.index.bin.bak`.
        //   3. fsync the parent directory so the `.bak` rename is durable.
        //   4. atomic_replace the tmp into place (the load-bearing write).
        //   5. On atomic_replace failure: restore the live name from `.bak`,
        //      fsync the parent, return the error.
        //   6. On success: remove `.bak` after a parent fsync.
        // Mirrors `src/hnsw/persist.rs`.
        let bak_path = {
            let file_name = path
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|| "splade.index".into());
            parent.join(format!("{}.bak", file_name))
        };

        // (1) Stale-`.bak` guard. A leftover means a previous save failed
        // mid-rollback and the operator has not cleared it; bail loudly so
        // we don't clobber the only live copy. Emit a structured warn before
        // the early return so journald records "SPLADE save refused — stale
        // .bak" independent of the error chain that propagates upward.
        if bak_path.exists() {
            tracing::warn!(
                bak_path = %bak_path.display(),
                live_path = %path.display(),
                "SPLADE save refused — stale .bak from prior failed save, manual recovery required"
            );
            let _ = std::fs::remove_file(&tmp_path);
            return Err(SpladeIndexPersistError::Io(std::io::Error::other(format!(
                "stale {} from prior failed save; manual recovery required \
                 (rename to {} or remove if confirmed bad) before retrying",
                bak_path.display(),
                path.display(),
            ))));
        }

        // (2) Back up the existing file so we can roll back if step 4 fails.
        // Skipped on first save (path doesn't exist yet — nothing to back up).
        let backed_up = if path.exists() {
            std::fs::rename(path, &bak_path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                SpladeIndexPersistError::Io(std::io::Error::other(format!(
                    "Failed to back up {} -> {} before save: {}",
                    path.display(),
                    bak_path.display(),
                    e,
                )))
            })?;
            true
        } else {
            false
        };

        // (3) fsync the parent directory so the `.bak` rename is durable
        // before atomic_replace proceeds. Best-effort: log at debug on
        // platforms that don't support directory fsync.
        if backed_up {
            if let Ok(f) = std::fs::File::open(parent) {
                if let Err(e) = f.sync_all() {
                    tracing::debug!(
                        error = %e,
                        dir = %parent.display(),
                        "fsync of SPLADE parent directory after backup failed (non-fatal)"
                    );
                }
            }
        }

        // (4) atomic_replace tmp -> path. This is the load-bearing write.
        // Atomic rename with cross-device fallback and parent-dir fsync.
        // The full sequence
        // lives in `cqs::fs::atomic_replace`. The BufWriter above already
        // flushed and sync_all'd the tmp file, but atomic_replace re-fsyncs
        // the path it reopens — effectively free.
        if let Err(e) = crate::fs::atomic_replace(&tmp_path, path) {
            // (5) Roll back: restore .bak -> live name. Best-effort cleanup
            // of our own tmp file on unexpected error before returning.
            let _ = std::fs::remove_file(&tmp_path);
            if backed_up {
                if let Err(restore_err) = std::fs::rename(&bak_path, path) {
                    tracing::error!(
                        path = %path.display(),
                        error = %restore_err,
                        "Failed to restore backup during SPLADE save rollback — \
                         manual recovery required (rename {}.bak to {})",
                        path.display(),
                        path.display(),
                    );
                    return Err(SpladeIndexPersistError::Io(std::io::Error::other(format!(
                        "SPLADE save failed and rollback failed: \
                         atomic_replace error={e}; restore error={restore_err}; \
                         manual recovery — rename {bak} to {path}",
                        bak = bak_path.display(),
                        path = path.display(),
                    ))));
                }
                // fsync after restore so the restore rename is durable.
                if let Ok(f) = std::fs::File::open(parent) {
                    let _ = f.sync_all();
                }
            }
            return Err(SpladeIndexPersistError::Io(e));
        }

        // (6) Successful save. Remove `.bak` and fsync the parent so the
        // unlink is durable. Both are best-effort — a leftover `.bak` is a
        // recovery breadcrumb, not a corruption risk.
        if backed_up {
            let _ = std::fs::remove_file(&bak_path);
            if let Ok(f) = std::fs::File::open(parent) {
                let _ = f.sync_all();
            }
        }

        tracing::info!(
            path = %path.display(),
            bytes = total_bytes,
            "SPLADE index persisted"
        );
        Ok(())
    }

    /// Stream the body (id_map + postings) through `writer`. Returns the
    /// number of body bytes written so the caller can log it.
    ///
    /// Splitting this out of [`SpladeIndex::save`] keeps the I/O logic
    /// (open file, write header, seek back to stamp checksum) focused on
    /// the layout and lets the serialization logic stay pure — no
    /// knowledge of the underlying file or hasher leaks in here. The
    /// `u64` byte counter uses `saturating_add` defensively, but the
    /// body size fits in `u64` for any realistic SPLADE index.
    fn write_body<W: Write>(
        writer: &mut W,
        // `&[Box<str>]` matches the in-memory storage. `Box<str>` derefs to
        // `&str` so `id.len()` and `id.as_bytes()` work below.
        id_map: &[Box<str>],
        postings: &HashMap<u32, Vec<(usize, f32)>>,
    ) -> Result<u64, SpladeIndexPersistError> {
        let mut bytes_written: u64 = 0;

        // id_map
        for id in id_map {
            let len_u32: u32 = id.len().try_into().map_err(|_| {
                // These are structural invariants, not I/O errors.
                SpladeIndexPersistError::CorruptData(format!(
                    "chunk id exceeds u32::MAX bytes: {}",
                    id.len()
                ))
            })?;
            writer.write_all(&len_u32.to_le_bytes())?;
            writer.write_all(id.as_bytes())?;
            bytes_written = bytes_written.saturating_add(4 + id.len() as u64);
        }

        // postings — HashMap iteration order is non-deterministic across
        // builds, but the in-flight hasher digests exactly the bytes that
        // reach the disk, so the checksum still matches the body we
        // actually wrote. Load-time parsing doesn't care about order.
        for (&token_id, posting_list) in postings {
            writer.write_all(&token_id.to_le_bytes())?;
            let count_u32: u32 = posting_list.len().try_into().map_err(|_| {
                SpladeIndexPersistError::CorruptData(format!(
                    "posting list for token {} exceeds u32::MAX entries: {}",
                    token_id,
                    posting_list.len()
                ))
            })?;
            writer.write_all(&count_u32.to_le_bytes())?;
            bytes_written = bytes_written.saturating_add(8);
            for &(chunk_idx, weight) in posting_list {
                let idx_u32: u32 = chunk_idx.try_into().map_err(|_| {
                    SpladeIndexPersistError::CorruptData(format!(
                        "chunk_idx exceeds u32::MAX: {}",
                        chunk_idx
                    ))
                })?;
                writer.write_all(&idx_u32.to_le_bytes())?;
                writer.write_all(&weight.to_le_bytes())?;
                bytes_written = bytes_written.saturating_add(8);
            }
        }

        Ok(bytes_written)
    }

    /// Attempt to load a persisted index from `path`.
    ///
    /// If the file is missing the function returns `Ok(None)`. If the file
    /// exists but is unreadable, corrupt, or stale relative to
    /// `expected_generation`, returns an `Err` describing the reason; the
    /// caller is expected to fall back to rebuild-from-SQLite and re-persist.
    ///
    /// The body is memory-mapped on fast filesystems and read into a heap
    /// `Vec` on slow ones (9P/NTFS/SMB/NFS, detected via the store's
    /// `is_slow_mmap_fs`) or when `CQS_SPLADE_NO_MMAP` is set. Either way the
    /// body is fully decoded into owned `id_map` / postings, so the mapping is
    /// transient and unmapped before this returns — it never escapes into
    /// `SpladeIndex`. mmap avoids the ~59MB+ heap copy `read_to_end` makes and
    /// holds through the decode, cutting the load-time peak (the steady-state
    /// resident size is unchanged).
    ///
    /// Safety guards (audit cluster):
    /// - file size capped at `CQS_SPLADE_MAX_INDEX_BYTES` (default 2 GB)
    ///   before any mmap or `read_to_end`, so an attacker or corruption can't
    ///   trigger an unbounded allocation / oversized mapping
    /// - blake3 hash covers header[0..32] + body, so any header bit
    ///   flip (not just body) is detected before `Vec::with_capacity` is
    ///   called on chunk_count / token_count
    /// - orphan temp files from previous crashed saves are cleaned up
    ///   at the top of `load()`, mirroring the HNSW pattern
    /// - corrupt-data conditions route through the dedicated
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

        // Cap file size BEFORE mapping or reading the body. Env override
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
        // The slice ranges are statically derived from the fixed
        // `SPLADE_INDEX_HEADER_LEN` (64 bytes); `read_exact` above guarantees
        // the buffer is full, so each `try_into` of an N-byte sub-range into
        // `[u8; N]` is structurally infallible. `expect` documents that and
        // surfaces the failure mode (a refactor that miscounts header bytes)
        // loudly instead of silently propagating an `unwrap()` panic.
        let version = u32::from_le_bytes(
            header[4..8]
                .try_into()
                .expect("invariant: header[4..8] is 4 bytes (SPLADE_INDEX_HEADER_LEN = 64)"),
        );
        if version != SPLADE_INDEX_VERSION {
            return Err(SpladeIndexPersistError::UnsupportedVersion(
                version,
                SPLADE_INDEX_VERSION,
            ));
        }
        let disk_generation = u64::from_le_bytes(
            header[8..16]
                .try_into()
                .expect("invariant: header[8..16] is 8 bytes"),
        );
        if disk_generation != expected_generation {
            return Err(SpladeIndexPersistError::GenerationMismatch {
                disk: disk_generation,
                store: expected_generation,
            });
        }
        let chunk_count = u64::from_le_bytes(
            header[16..24]
                .try_into()
                .expect("invariant: header[16..24] is 8 bytes"),
        );
        let token_count = u64::from_le_bytes(
            header[24..32]
                .try_into()
                .expect("invariant: header[24..32] is 8 bytes"),
        );
        let stored_hash: [u8; 32] = header[32..64]
            .try_into()
            .expect("invariant: header[32..64] is 32 bytes (CHECKSUM_LEN)");

        let body_len = (file_size as usize).saturating_sub(SPLADE_INDEX_HEADER_LEN);

        // Obtain the body bytes either by memory-mapping the file (fast path,
        // avoids the ~59MB+ transient heap copy that `read_to_end` makes and
        // holds alive through the entire decode) or by reading into a heap
        // `Vec` (fallback). Both yield a `&[u8]` view over the body region;
        // `verify_and_parse_body` fully decodes into owned `id_map` / postings,
        // so the body backing is dropped when this scope ends — mmap is a
        // transient-peak optimization, not a zero-copy one (`SpladeIndex` owns
        // its strings and posting vectors, no borrow into the file survives).
        //
        // Slow-FS guard: on 9P/NTFS/SMB/NFS (e.g. WSL `/mnt/c/`), mmap reads
        // fall back to per-page host/network round-trips and are slower than a
        // single sequential `read_to_end`. Reuse the store's filesystem
        // detection (`is_slow_mmap_fs`) — the same backends that hurt SQLite
        // mmap hurt this one. `CQS_SPLADE_NO_MMAP=1` forces the heap path.
        let use_mmap = !force_heap_read() && !crate::store::is_slow_mmap_fs(path);

        // Replace-while-mapped is safe: `save()` writes to a temp file and
        // atomically renames it into place (`crate::fs::atomic_replace`). On
        // Linux the old inode stays alive behind any live mapping and is
        // unlinked only when the last reference drops, so a concurrent watch
        // rebuild can't pull the bytes out from under a live mmap. The mapping
        // is confined to this function — it never escapes into `Self` — so it
        // is unmapped before `load` returns and the load-bearing invariant is
        // "don't truncate the file in place," which the writer never does.
        if use_mmap {
            // SAFETY: the file is opened read-only and mapped read-only. The
            // only writer (`save`) replaces the file atomically rather than
            // mutating it in place, so the mapped pages are stable for the
            // (short) lifetime of this mapping. The map covers the file's
            // current length; we reject the mapping below unless its length
            // equals the `file_size` the metadata stat captured, so any
            // concurrent grow/shrink between stat and map falls back to a
            // bounded heap read rather than parsing a torn view.
            match unsafe { memmap2::Mmap::map(reader.get_ref()) } {
                Ok(mmap) => {
                    if mmap.len() != file_size as usize {
                        // File changed size between stat and map — treat as a
                        // transient inconsistency and fall back to the heap
                        // read, which re-reads under the size cap.
                        tracing::warn!(
                            mapped = mmap.len(),
                            expected = file_size,
                            "SPLADE index mmap length mismatch, falling back to heap read"
                        );
                    } else {
                        let body = &mmap[SPLADE_INDEX_HEADER_LEN..];
                        return Self::verify_and_parse_body(
                            path,
                            &header,
                            &stored_hash,
                            chunk_count,
                            token_count,
                            body,
                        )
                        .map(Some);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "SPLADE index mmap failed, falling back to heap read"
                    );
                }
            }
        }

        // Heap fallback. Pre-allocate from the known file size to avoid
        // ~log₂(59MB) reallocations on a typical SPLADE-Code 0.6B index.
        let mut body = Vec::with_capacity(body_len);
        reader.read_to_end(&mut body)?;
        Self::verify_and_parse_body(path, &header, &stored_hash, chunk_count, token_count, &body)
            .map(Some)
    }

    /// Verify the body checksum and decode the body into owned
    /// `id_map` / postings.
    ///
    /// Split out of [`SpladeIndex::load`] so the mmap-backed and
    /// heap-backed read paths share one decode over a `&[u8]` view. The
    /// returned `SpladeIndex` owns all of its data, so `body` may be dropped
    /// (or unmapped) immediately after this returns.
    fn verify_and_parse_body(
        path: &Path,
        header: &[u8; SPLADE_INDEX_HEADER_LEN],
        stored_hash: &[u8; CHECKSUM_LEN],
        chunk_count: u64,
        token_count: u64,
        body: &[u8],
    ) -> Result<Self, SpladeIndexPersistError> {
        // Hash covers header[0..32] + body so flipping a bit in `chunk_count`
        // (header bytes [16..24]) fails the integrity check rather than
        // reaching `Vec::with_capacity(usize::MAX)` → process panic.
        let mut hasher = blake3::Hasher::new();
        hasher.update(&header[0..32]);
        hasher.update(body);
        let actual_hash = hasher.finalize();
        if actual_hash.as_bytes() != stored_hash {
            // Use blake3::Hash::to_hex for the expected hex encoding so we
            // don't pull in the `hex` crate just for this one call.
            let expected_hex = blake3::Hash::from_bytes(*stored_hash).to_hex().to_string();
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
        // `Vec<Box<str>>` matches the build-time storage.
        let mut id_map: Vec<Box<str>> = Vec::with_capacity(chunk_count_usize);
        let mut cursor: usize = 0;

        fn need(body: &[u8], cursor: usize, n: usize) -> Result<(), SpladeIndexPersistError> {
            if cursor.saturating_add(n) > body.len() {
                Err(SpladeIndexPersistError::Truncated(cursor as u64))
            } else {
                Ok(())
            }
        }

        for _ in 0..chunk_count_usize {
            need(body, cursor, 4)?;
            // `need(body, cursor, 4)?` above ensures the 4-byte slice
            // exists; `try_into` of a 4-byte slice into `[u8; 4]` is
            // structurally infallible. `expect` documents the invariant.
            let len = u32::from_le_bytes(
                body[cursor..cursor + 4]
                    .try_into()
                    .expect("invariant: need(_, cursor, 4)? guarantees 4-byte slice"),
            ) as usize;
            cursor += 4;
            need(body, cursor, len)?;
            // `.to_string()` allocates an owned String from a `&str` borrow
            // into `body`. Inherent — `id_map` owns its strings and `body` is
            // dropped after parsing, so there is no zero-copy path.
            let id = std::str::from_utf8(&body[cursor..cursor + len])
                .map_err(|e| {
                    SpladeIndexPersistError::CorruptData(format!(
                        "chunk id is not valid utf-8: {}",
                        e
                    ))
                })?
                .to_string();
            cursor += len;
            id_map.push(id.into_boxed_str());
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
            need(body, cursor, 8)?;
            // Each `try_into` is gated by the preceding
            // `need(body, cursor, N)?`; `expect` documents the invariant so a
            // future refactor that breaks the bound check fails loudly instead
            // of through `unwrap()`.
            let token_id = u32::from_le_bytes(
                body[cursor..cursor + 4]
                    .try_into()
                    .expect("invariant: need(_, cursor, 8)? guarantees 4-byte token_id slice"),
            );
            cursor += 4;
            let posting_count = u32::from_le_bytes(
                body[cursor..cursor + 4]
                    .try_into()
                    .expect("invariant: same need() covers the posting_count u32"),
            ) as usize;
            cursor += 4;
            need(body, cursor, posting_count.saturating_mul(8))?;
            let mut postings_for_token: Vec<(usize, f32)> = Vec::with_capacity(posting_count);
            for _ in 0..posting_count {
                let chunk_idx =
                    u32::from_le_bytes(body[cursor..cursor + 4].try_into().expect(
                        "invariant: need(_, cursor, posting_count*8)? covers chunk_idx u32",
                    )) as usize;
                cursor += 4;
                let weight = f32::from_le_bytes(
                    body[cursor..cursor + 4]
                        .try_into()
                        .expect("invariant: same need() covers the weight f32"),
                );
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
        Ok(Self { postings, id_map })
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

    /// TC-ADV-V1.33-3: `SpladeIndex::search` with `k=0` must return empty
    /// even on a populated index. `BoundedScoreHeap::new(0)` is tested
    /// elsewhere; this pins the SPLADE call path that passes `k` straight in.
    #[test]
    fn test_search_k_zero_returns_empty() {
        let index = make_test_index();
        // Use a query that would otherwise match all three chunks.
        let results = index.search(&vec![(1, 1.0), (2, 1.0), (3, 1.0)], 0);
        assert!(
            results.is_empty(),
            "k=0 must return empty, got {} results",
            results.len()
        );
    }

    /// TC-ADV-V1.33-3: a query SparseVector with NaN weights must not
    /// panic — the search loop's `query_weight * doc_weight` accumulator
    /// must remain panic-free under the hostile input. NaN is allowed to
    /// propagate into scores (current behaviour) but the call must complete.
    #[test]
    fn test_search_handles_nan_query_weights() {
        let index = make_test_index();
        // NaN on token 1 (which exists in the index for chunk_a + chunk_b).
        let results = index.search(&vec![(1, f32::NAN)], 10);
        // Contract: no panic. Today NaN propagates into the score comparator
        // via `total_cmp`, so results may or may not be present — the
        // load-bearing assertion is "did not crash".
        for r in &results {
            // Score may be NaN; just ensure id is a known chunk.
            assert!(
                r.id == "chunk_a" || r.id == "chunk_b",
                "unexpected id in results: {}",
                r.id
            );
        }
    }

    /// TC-ADV-V1.33-3: same panic-free contract for `f32::INFINITY`.
    #[test]
    fn test_search_handles_inf_query_weights() {
        let index = make_test_index();
        let results = index.search(&vec![(1, f32::INFINITY)], 10);
        // Inf * positive doc_weight = Inf — sortable; first should be the
        // matching chunk. We don't pin score values; just no crash + no
        // unexpected ids.
        for r in &results {
            assert!(
                r.id == "chunk_a" || r.id == "chunk_b",
                "unexpected id in results: {}",
                r.id
            );
        }
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

    /// Streaming `save()` must produce a file `load()` can round-trip. Uses
    /// the multi-token `make_test_index()` fixture so HashMap iteration order
    /// is exercised (only the total bytes + checksum matter — the order of
    /// postings on disk is allowed to vary across runs).
    #[test]
    fn test_streaming_save_roundtrips_through_load() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let original = make_test_index();

        original.save(&path, 99).unwrap();
        let loaded = SpladeIndex::load(&path, 99).unwrap().unwrap();

        // Same chunks in the same order: id_map is deterministic across save.
        assert_eq!(loaded.id_map, original.id_map);
        // Same tokens, same postings (content, not map iteration order).
        assert_eq!(loaded.postings.len(), original.postings.len());
        for (tok, postings) in &original.postings {
            let round_trip = loaded
                .postings
                .get(tok)
                .unwrap_or_else(|| panic!("lost token {} on round-trip", tok));
            assert_eq!(round_trip.len(), postings.len());
            for (a, b) in round_trip.iter().zip(postings.iter()) {
                assert_eq!(a.0, b.0);
                assert!((a.1 - b.1).abs() < f32::EPSILON);
            }
        }
    }

    /// Pins the exact blake3 hex for a fully-deterministic 1-chunk /
    /// 1-token / 1-posting fixture. If the header layout or write ordering
    /// changes, this test fails so on-disk format drift is caught.
    ///
    /// The expected hex was computed out-of-band against the documented
    /// hash invariant `blake3(header[0..32] || body)` with:
    ///   header[0..4]   = b"SPDX"
    ///   header[4..8]   = version 1 LE
    ///   header[8..16]  = generation 42 LE
    ///   header[16..24] = chunk_count 1 LE
    ///   header[24..32] = token_count 1 LE
    ///   body           = u32 7, b"chunk_a", u32 42, u32 1, u32 0, f32 0.5
    ///
    /// A single chunk and a single token removes HashMap iteration order
    /// from the equation — the serialized bytes are fully deterministic.
    #[test]
    fn test_streaming_save_on_disk_format_byte_identical() {
        // Must match the fixture documented above exactly — otherwise the
        // hardcoded hex is meaningless.
        let original = SpladeIndex::build(vec![("chunk_a".to_string(), vec![(42u32, 0.5f32)])]);
        assert_eq!(original.id_map.len(), 1);
        assert_eq!(original.postings.len(), 1);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        original.save(&path, 42).unwrap();

        // Read the full file back and pick off the checksum slot.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            bytes.len(),
            // header(64) + u32 id len(4) + "chunk_a"(7)
            //            + u32 token(4) + u32 count(4) + u32 idx(4) + f32 wt(4)
            64 + 4 + 7 + 4 + 4 + 4 + 4,
            "fixture file size does not match expected layout"
        );

        // Header magic + version + generation + counts must be pinned too;
        // otherwise the hex would only prove that SOMETHING was hashed, not
        // that the expected bytes were hashed.
        assert_eq!(&bytes[0..4], b"SPDX");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 42);
        assert_eq!(u64::from_le_bytes(bytes[16..24].try_into().unwrap()), 1);
        assert_eq!(u64::from_le_bytes(bytes[24..32].try_into().unwrap()), 1);

        let checksum = &bytes[32..64];
        let actual_hex = blake3::Hash::from_bytes(checksum.try_into().unwrap())
            .to_hex()
            .to_string();

        // Hardcoded hex pinning the on-disk format and the
        // `blake3(header[0..32] || body)` invariant.
        let expected_hex = "8cdea25d4b34ce371cf1a8189fc0af7fd99f963f4de2da2c7ea3aff935db3a53";
        assert_eq!(
            actual_hex, expected_hex,
            "#917 streaming save produced unexpected on-disk checksum — \
             format drift? header layout or write ordering may have changed"
        );

        // And cross-check: feeding the documented prefix + body back
        // through blake3 yields the same hex. Catches bugs where the
        // checksum LUCKILY lines up but the body on disk is different.
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes[0..32]);
        hasher.update(&bytes[64..]);
        assert_eq!(hasher.finalize().to_hex().to_string(), expected_hex);
    }

    // ====== `.bak` rollback pattern ======

    /// First save (no prior file) leaves no `.bak` — there's nothing to
    /// back up. Pins the "skipped backup" branch.
    #[test]
    fn save_no_prior_file_leaves_no_bak() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let bak = dir.path().join("splade.index.bin.bak");
        assert!(!path.exists());
        assert!(!bak.exists());

        make_test_index().save(&path, 1).unwrap();

        assert!(path.exists(), "save must create the live file");
        assert!(
            !bak.exists(),
            "first save (no prior file) must not leave a .bak behind"
        );
    }

    /// Successful save with a prior live file: live file is replaced and
    /// the `.bak` cleanup step removes the backup. Pins the success path
    /// of the new rollback machinery.
    #[test]
    fn save_with_prior_file_replaces_and_cleans_up_bak() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let bak = dir.path().join("splade.index.bin.bak");

        // First save — establishes a prior live file.
        let v1 = SpladeIndex::build(vec![("chunk_v1".to_string(), vec![(7u32, 0.25f32)])]);
        v1.save(&path, 1).unwrap();
        let v1_bytes = std::fs::read(&path).unwrap();

        // Second save — must rename existing -> .bak, then clean up .bak
        // after success. The `.bak` is never visible to other callers.
        let v2 = SpladeIndex::build(vec![("chunk_v2".to_string(), vec![(8u32, 0.5f32)])]);
        v2.save(&path, 2).unwrap();
        let v2_bytes = std::fs::read(&path).unwrap();

        assert_ne!(v1_bytes, v2_bytes, "second save must replace the file");
        assert!(
            !bak.exists(),
            "successful save must clean up .bak; left behind: {}",
            bak.display(),
        );

        // The post-save file is a valid index loadable at the new generation.
        let loaded = SpladeIndex::load(&path, 2).unwrap().unwrap();
        // id_map is Vec<Box<str>>; deref each entry to compare against the
        // expected literal.
        assert_eq!(loaded.id_map.len(), 1);
        assert_eq!(&*loaded.id_map[0], "chunk_v2");
    }

    /// Stale `.bak` left over from a prior failed save must block the next
    /// save with an actionable error so the operator notices and clears it
    /// rather than clobbering the live file (the only good copy).
    #[test]
    fn save_refuses_when_stale_bak_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let bak = dir.path().join("splade.index.bin.bak");

        // First save establishes a live file.
        make_test_index().save(&path, 1).unwrap();
        // Simulate a prior crashed save: a stale `.bak` lingers next to it.
        std::fs::write(&bak, b"stale-bak-from-prior-crash").unwrap();

        // The next save MUST refuse with an error mentioning recovery.
        let err = make_test_index().save(&path, 2).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("stale") && msg.contains("manual recovery"),
            "stale-bak guard must surface an actionable error, got: {msg}"
        );

        // The live file must remain untouched (still loadable at gen=1).
        let loaded = SpladeIndex::load(&path, 1).unwrap();
        assert!(
            loaded.is_some(),
            "live file must survive intact when save bails on stale .bak"
        );
    }

    /// Assert two indexes are structurally identical (id_map order + every
    /// posting list, content not map-iteration order). Used by the
    /// mmap-vs-heap parity tests below.
    fn assert_indexes_equal(a: &SpladeIndex, b: &SpladeIndex) {
        assert_eq!(a.id_map, b.id_map, "id_map mismatch");
        assert_eq!(a.postings.len(), b.postings.len(), "posting count mismatch");
        for (tok, list_a) in &a.postings {
            let list_b = b
                .postings
                .get(tok)
                .unwrap_or_else(|| panic!("token {tok} missing in other index"));
            assert_eq!(list_a.len(), list_b.len(), "posting list len mismatch");
            for (x, y) in list_a.iter().zip(list_b.iter()) {
                assert_eq!(x.0, y.0, "chunk_idx mismatch");
                assert!((x.1 - y.1).abs() < f32::EPSILON, "weight mismatch");
            }
        }
    }

    /// The mmap-backed load and the forced-heap load of the SAME on-disk file
    /// must produce byte-for-byte identical decoded indexes. Serial because it
    /// toggles the `CQS_SPLADE_NO_MMAP` process env var.
    #[test]
    #[serial_test::serial]
    fn test_load_mmap_matches_heap() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let original = make_test_index();
        original.save(&path, 3).unwrap();

        let prev = std::env::var("CQS_SPLADE_NO_MMAP").ok();

        // Default path: tempdir is tmpfs/ext4 (fast), so this exercises mmap.
        std::env::remove_var("CQS_SPLADE_NO_MMAP");
        let via_mmap = SpladeIndex::load(&path, 3).unwrap().unwrap();

        // Forced-heap path via the env escape hatch.
        std::env::set_var("CQS_SPLADE_NO_MMAP", "1");
        let via_heap = SpladeIndex::load(&path, 3).unwrap().unwrap();

        match prev {
            Some(v) => std::env::set_var("CQS_SPLADE_NO_MMAP", v),
            None => std::env::remove_var("CQS_SPLADE_NO_MMAP"),
        }

        assert_indexes_equal(&via_mmap, &original);
        assert_indexes_equal(&via_heap, &original);
        assert_indexes_equal(&via_mmap, &via_heap);
    }

    /// `CQS_SPLADE_NO_MMAP` truthy values all select the heap path; the load
    /// still succeeds and round-trips. This pins the env-parse branch in
    /// `force_heap_read` without reaching into filesystem detection.
    #[test]
    #[serial_test::serial]
    fn test_force_heap_read_env_values() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");
        let original = make_test_index();
        original.save(&path, 5).unwrap();

        let prev = std::env::var("CQS_SPLADE_NO_MMAP").ok();
        for v in ["1", "true", "YES", "True"] {
            std::env::set_var("CQS_SPLADE_NO_MMAP", v);
            assert!(force_heap_read(), "{v} should force heap read");
            let loaded = SpladeIndex::load(&path, 5).unwrap().unwrap();
            assert_indexes_equal(&loaded, &original);
        }
        for v in ["0", "false", "no", ""] {
            std::env::set_var("CQS_SPLADE_NO_MMAP", v);
            assert!(!force_heap_read(), "{v:?} should not force heap read");
        }
        std::env::remove_var("CQS_SPLADE_NO_MMAP");
        assert!(!force_heap_read(), "unset should not force heap read");

        match prev {
            Some(v) => std::env::set_var("CQS_SPLADE_NO_MMAP", v),
            None => std::env::remove_var("CQS_SPLADE_NO_MMAP"),
        }
    }

    /// Writer contract under a live mapping: open an index (default = mmap on
    /// tmpfs), then atomically replace the on-disk file with a NEW index via
    /// `save()`, then reload. The first handle keeps reading the old bytes
    /// (it owns decoded data; the mapping, if any, is already dropped after
    /// load returns), and the reload sees the new content. This pins the
    /// atomic-replace-vs-mmap invariant documented in `load`: the writer
    /// renames a temp file into place rather than truncating in place, so a
    /// concurrent rebuild can't corrupt a live reader.
    #[test]
    fn test_load_after_atomic_replace() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("splade.index.bin");

        let v1 = SpladeIndex::build(vec![("chunk_v1".to_string(), vec![(7u32, 0.25f32)])]);
        v1.save(&path, 1).unwrap();

        // Hold a live handle loaded from the v1 file. SpladeIndex owns its
        // data, so this stays valid regardless of what happens to the file.
        let live = SpladeIndex::load(&path, 1).unwrap().unwrap();
        assert_eq!(&*live.id_map[0], "chunk_v1");

        // Atomically replace the file with a v2 index while `live` is held.
        let v2 = SpladeIndex::build(vec![
            ("chunk_v2a".to_string(), vec![(8u32, 0.5f32)]),
            ("chunk_v2b".to_string(), vec![(9u32, 0.75f32)]),
        ]);
        v2.save(&path, 2).unwrap();

        // The pre-existing handle is unaffected (decoded data is owned).
        assert_eq!(live.id_map.len(), 1);
        assert_eq!(&*live.id_map[0], "chunk_v1");

        // Reloading at the new generation sees the replaced content.
        let reloaded = SpladeIndex::load(&path, 2).unwrap().unwrap();
        assert_eq!(reloaded.id_map.len(), 2);
        assert_eq!(&*reloaded.id_map[0], "chunk_v2a");
        assert_eq!(&*reloaded.id_map[1], "chunk_v2b");
        // The stale-generation load against the new file is rejected, proving
        // the replace actually swapped bytes (not a no-op).
        assert!(matches!(
            SpladeIndex::load(&path, 1),
            Err(SpladeIndexPersistError::GenerationMismatch { disk: 2, store: 1 })
        ));
    }

    // ====== Property-based round-trip: save -> load is structural identity ======
    //
    // The hand-written round-trip tests (`test_persist_roundtrip`,
    // `test_streaming_save_roundtrips_through_load`) each pin one fixed
    // 3-chunk / handful-of-token index and compare weights with
    // `(a - b).abs() < f32::EPSILON`. That comparison structurally CANNOT
    // distinguish:
    //   - `+0.0` from `-0.0` (they compare equal under subtraction),
    //   - one NaN bit pattern from another (NaN - NaN = NaN, `< EPSILON` is
    //     false — the approx compare would actually FALSE-FAIL on NaN, so the
    //     fixtures avoid NaN entirely and never test it),
    //   - a weight that decoded to a nearby-but-different f32.
    // The codec's contract is BIT-EXACT (`f32::to_le_bytes` /
    // `f32::from_le_bytes`), so the property asserts `to_bits()` identity —
    // a strictly sharper invariant the example suite cannot express.
    //
    // Invariant (one line):
    //   for every valid index i:  load(save(i)).id_map == i.id_map
    //                       AND   load(save(i)).postings ≡_bits i.postings
    mod roundtrip_proptest {
        use super::*;
        use proptest::prelude::*;

        /// A weight strategy whose coverage claim is: hits every f32 class the
        /// LE codec must survive bit-exactly — ordinary finite values, the two
        /// signed zeros, the f32 extrema, the smallest subnormal, and the
        /// non-finite values (±inf, a NaN). `prop_oneof` keeps the boundary
        /// classes at non-trivial probability instead of relying on
        /// `prop::num::f32::ANY` (which emits NaN/inf only rarely).
        ///
        /// DISTRUST: `ANY` alone would almost never sample `-0.0` or a
        /// subnormal, so the boundary that actually bites a float codec
        /// (sign-of-zero, NaN bit pattern) would go unexercised. The explicit
        /// `just(...)` arms force them.
        fn weight_strategy() -> impl Strategy<Value = f32> {
            prop_oneof![
                4 => prop::num::f32::ANY,
                1 => Just(0.0_f32),
                1 => Just(-0.0_f32),
                1 => Just(f32::MIN),
                1 => Just(f32::MAX),
                1 => Just(f32::MIN_POSITIVE),
                1 => Just(f32::from_bits(1)), // smallest positive subnormal
                1 => Just(f32::INFINITY),
                1 => Just(f32::NEG_INFINITY),
                1 => Just(f32::NAN),
            ]
        }

        /// A chunk-id strategy. Coverage claim: empty string, ASCII, and
        /// multi-byte UTF-8 (so `id.len()` byte-length prefix vs char count
        /// can't silently diverge), bounded to 0..12 chars so shrinking stays
        /// fast. The `\\PC*` class is proptest's "any non-control unicode
        /// scalar", which reaches astral-plane codepoints (4-byte UTF-8).
        fn id_strategy() -> impl Strategy<Value = String> {
            prop_oneof![
                1 => Just(String::new()),
                3 => "[a-zA-Z0-9_:./-]{0,12}",
                2 => "\\PC{0,8}",
            ]
        }

        /// A single chunk's sparse vector. Coverage claim: empty vector
        /// (a chunk that contributed no tokens), up to 6 (token_id, weight)
        /// pairs, token_ids spanning the full u32 range via `prop::num::u32::ANY`
        /// (so 0 and u32::MAX are reachable), weights from `weight_strategy`.
        /// Allows DUPLICATE token_ids within one vector on purpose — `build`
        /// pushes each occurrence as its own posting, and the codec must
        /// preserve that multiplicity and order.
        fn sparse_strategy() -> impl Strategy<Value = Vec<(u32, f32)>> {
            prop::collection::vec((prop::num::u32::ANY, weight_strategy()), 0..6)
        }

        /// A whole index input: 0..8 chunks (0 covers the empty-index codec
        /// path that no fixture exercises). chunk_ids are NOT required unique —
        /// `build` doesn't dedup, so two identical ids are two id_map slots,
        /// and the codec must round-trip both.
        fn index_input_strategy() -> impl Strategy<Value = Vec<(String, Vec<(u32, f32)>)>> {
            prop::collection::vec((id_strategy(), sparse_strategy()), 0..8)
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 256,
                // Deterministic: a CI failure reproduces from the seed printed
                // in the panic + the committed `.proptest-regressions` file.
                ..ProptestConfig::default()
            })]

            /// load(save(idx)) is a bit-exact structural identity for EVERY
            /// validly-built index. The fixture tests can express the
            /// approximate version of this for one hand-picked index; the
            /// property expresses the exact version for the whole input space,
            /// including empty indexes, unicode/empty ids, duplicate ids,
            /// duplicate tokens, and the full f32 boundary set.
            #[test]
            fn prop_splade_save_load_roundtrip_bit_exact(
                input in index_input_strategy(),
                generation in prop::num::u64::ANY,
            ) {
                let original = SpladeIndex::build(
                    input.iter().map(|(id, sv)| (id.clone(), sv.clone())).collect(),
                );

                let dir = tempfile::TempDir::new().unwrap();
                let path = dir.path().join("splade.index.bin");

                original.save(&path, generation)?;
                let loaded = SpladeIndex::load(&path, generation)?
                    .ok_or_else(|| {
                        TestCaseError::fail("load returned Ok(None) for a file we just wrote")
                    })?;

                // id_map is insertion-ordered on both sides — exact equality.
                prop_assert_eq!(
                    &loaded.id_map, &original.id_map,
                    "id_map diverged across save/load"
                );

                // Same token set.
                prop_assert_eq!(
                    loaded.postings.len(), original.postings.len(),
                    "unique-token count diverged across save/load"
                );

                // Per token: same posting multiplicity, same order, same
                // chunk_idx, and BIT-EXACT weight (the assertion the fixtures
                // cannot make).
                for (token_id, orig_postings) in &original.postings {
                    let loaded_postings = loaded.postings.get(token_id).ok_or_else(|| {
                        TestCaseError::fail(format!("token {token_id} lost on round-trip"))
                    })?;
                    prop_assert_eq!(
                        loaded_postings.len(), orig_postings.len(),
                        "posting-list length diverged for token {}", token_id
                    );
                    for (a, b) in loaded_postings.iter().zip(orig_postings.iter()) {
                        prop_assert_eq!(a.0, b.0, "chunk_idx diverged for token {}", token_id);
                        prop_assert!(
                            a.1.to_bits() == b.1.to_bits(),
                            "weight bit pattern diverged for token {}: \
                             loaded {:#010x} != original {:#010x} \
                             (loaded={}, original={})",
                            token_id, a.1.to_bits(), b.1.to_bits(), a.1, b.1
                        );
                    }
                }
            }

            /// The two read backings (mmap vs forced-heap) of the SAME on-disk
            /// file must decode to bit-identical indexes for EVERY valid input.
            /// `test_load_mmap_matches_heap` asserts this for one fixture with
            /// an approximate weight compare; the property generalizes it over
            /// the input space with bit-exact weights. Serial because it
            /// toggles `CQS_SPLADE_NO_MMAP`.
            #[test]
            #[serial_test::serial]
            fn prop_splade_mmap_matches_heap_bit_exact(
                input in index_input_strategy(),
            ) {
                // Skip the empty case: an all-empty index produces a 64-byte
                // header-only file whose mmap/heap paths are trivially equal
                // and the env toggle adds no coverage. Non-empty inputs
                // exercise the body decode under both backings.
                prop_assume!(!input.is_empty());

                let original = SpladeIndex::build(
                    input.iter().map(|(id, sv)| (id.clone(), sv.clone())).collect(),
                );
                let dir = tempfile::TempDir::new().unwrap();
                let path = dir.path().join("splade.index.bin");
                original.save(&path, 1)?;

                let prev = std::env::var("CQS_SPLADE_NO_MMAP").ok();

                std::env::remove_var("CQS_SPLADE_NO_MMAP");
                let via_mmap = SpladeIndex::load(&path, 1)?
                    .ok_or_else(|| TestCaseError::fail("mmap load returned None"))?;

                std::env::set_var("CQS_SPLADE_NO_MMAP", "1");
                let via_heap = SpladeIndex::load(&path, 1)?
                    .ok_or_else(|| TestCaseError::fail("heap load returned None"))?;

                match prev {
                    Some(v) => std::env::set_var("CQS_SPLADE_NO_MMAP", v),
                    None => std::env::remove_var("CQS_SPLADE_NO_MMAP"),
                }

                prop_assert_eq!(&via_mmap.id_map, &via_heap.id_map, "mmap/heap id_map diverged");
                prop_assert_eq!(
                    via_mmap.postings.len(), via_heap.postings.len(),
                    "mmap/heap token count diverged"
                );
                for (token_id, mmap_list) in &via_mmap.postings {
                    let heap_list = via_heap.postings.get(token_id).ok_or_else(|| {
                        TestCaseError::fail(format!("token {token_id} present in mmap, absent in heap"))
                    })?;
                    prop_assert_eq!(mmap_list.len(), heap_list.len(), "mmap/heap posting len diverged");
                    for (a, b) in mmap_list.iter().zip(heap_list.iter()) {
                        prop_assert_eq!(a.0, b.0, "mmap/heap chunk_idx diverged");
                        prop_assert!(
                            a.1.to_bits() == b.1.to_bits(),
                            "mmap/heap weight bits diverged: {:#010x} != {:#010x}",
                            a.1.to_bits(), b.1.to_bits()
                        );
                    }
                }
            }
        }
    }
}
