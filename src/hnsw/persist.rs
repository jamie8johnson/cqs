//! HNSW index persistence (save/load)

use std::cell::UnsafeCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use hnsw_rs::hnswio::HnswIo;

use crate::index::{DistanceMetric, VectorIndex};

use super::{HnswError, HnswGraph, HnswIndex, HnswInner, HnswIoCell, LoadedHnsw};

/// Configurable HNSW graph file size limit via `CQS_HNSW_MAX_GRAPH_BYTES`
/// env var. Defaults to 500MB. Cached in OnceLock for single parse.
/// Parse/warn/default goes through the shared `crate::limits::parse_env_u64`.
fn hnsw_max_graph_bytes() -> u64 {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| crate::limits::parse_env_u64("CQS_HNSW_MAX_GRAPH_BYTES", 500 * 1024 * 1024))
}

/// Configurable HNSW data file size limit via `CQS_HNSW_MAX_DATA_BYTES`
/// env var. Defaults to 1GB. Cached in OnceLock for single parse.
/// Parse/warn/default goes through the shared `crate::limits::parse_env_u64`.
fn hnsw_max_data_bytes() -> u64 {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| crate::limits::parse_env_u64("CQS_HNSW_MAX_DATA_BYTES", 1024 * 1024 * 1024))
}

/// Configurable HNSW ID map file size limit via `CQS_HNSW_MAX_ID_MAP_BYTES`
/// env var. Defaults to 500MB. Cached in OnceLock for single parse.
/// Parse/warn/default goes through the shared `crate::limits::parse_env_u64`.
fn hnsw_max_id_map_bytes() -> u64 {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| {
        crate::limits::parse_env_u64("CQS_HNSW_MAX_ID_MAP_BYTES", 500 * 1024 * 1024)
    })
}

/// Whether the WSL advisory locking warning has been emitted (once per process)
static WSL_LOCK_WARNED: AtomicBool = AtomicBool::new(false);

/// Emit a one-time warning about advisory-only file locking on WSL/NTFS mounts.
/// Delegates the `/mnt/<letter>/` detection to `config::is_wsl_drvfs_path`.
fn warn_wsl_advisory_locking(dir: &Path) {
    if crate::config::is_wsl()
        && crate::config::is_wsl_drvfs_path(dir)
        && !WSL_LOCK_WARNED.swap(true, Ordering::Relaxed)
    {
        tracing::warn!(
            "HNSW file locking is advisory-only on WSL/NTFS — avoid concurrent index operations"
        );
    }
}

/// Per-generation HNSW artifact extensions. Each successful save writes a
/// fresh generation `{basename}.gen<N>.hnsw.{graph,data,ids}`; the manifest
/// pointer flip (see [`HnswManifest`]) is the single atomic commit point.
const HNSW_ARTIFACT_EXTS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"];

/// Legacy pre-manifest sidecar extensions. A fixed-name five-file set
/// (`{basename}.hnsw.{graph,data,ids,meta,checksum}`) is what every index
/// built before the manifest-generation scheme carries. HNSW is derived
/// data, so the loader treats a legacy set (no manifest) as absent and lets
/// the rebuild path fire; the first successful manifest-format save deletes
/// these files under the exclusive lock.
const LEGACY_HNSW_EXTENSIONS: &[&str] = &[
    "hnsw.graph",
    "hnsw.data",
    "hnsw.ids",
    "hnsw.meta",
    "hnsw.checksum",
];

/// Magic string for the `{basename}.hnsw.manifest` JSON pointer. A mismatch
/// is a typed load error, not a silent default.
const HNSW_MANIFEST_MAGIC: &str = "CQSHNSWMANIFEST";

/// Version of the manifest schema. Bump when adding fields an older binary
/// must not misinterpret; the load path rejects versions newer than this
/// binary understands.
const HNSW_MANIFEST_VERSION: u32 = 1;

/// The manifest pointer persisted next to the generation artifact files.
///
/// This is the single atomic commit point of a save: `atomic_replace` of the
/// manifest either fully lands (making generation `N` current) or leaves the
/// previous manifest in place — never a torn state. It carries everything the
/// load path needs before it can instantiate the right `Hnsw<f32, D>`:
///
/// - `generation`: which `{basename}.gen<N>.hnsw.*` artifact set is current.
/// - `metric`: the distance metric ([`DistanceMetric::as_str`]), needed
///   before the graph type parameter can be chosen.
/// - the store-state stamp (`chunk_count` + `splade_generation`) the artifacts
///   reflect — the dirty-flag self-heal discriminator.
/// - `checksums`: blake3 of the current generation's graph/data/ids files.
///
/// # Integrity model
///
/// Artifact corruption (bit rot, truncation) is caught by the inline
/// `checksums`. The manifest itself is NOT self-checksummed: `atomic_replace`
/// rules out partial writes, and a manifest that fails JSON validation falls
/// into the rebuild path rather than being trusted. Deliberate tampering is
/// out of the documented threat model (see [`verify_hnsw_checksums`]) — an
/// editor who can rewrite the manifest can already rewrite the artifacts and
/// their recorded checksums. A tampered `metric` is still caught on load:
/// hnsw_rs cross-checks the distance name embedded in the graph file.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
struct HnswManifest {
    /// Format magic. See [`HNSW_MANIFEST_MAGIC`].
    magic: String,
    /// Manifest schema version. See [`HNSW_MANIFEST_VERSION`].
    version: u32,
    /// Current generation: the `{basename}.gen<N>.hnsw.*` set this manifest
    /// points at.
    generation: u64,
    /// [`DistanceMetric::as_str`] value the index was built with.
    metric: String,
    /// `Store::chunk_count()` of the store state this generation was built
    /// from. `None` on a store-less `save()`; a stamp-less manifest cannot
    /// prove it postdates the last chunk write, so the dirty-flag self-heal
    /// treats it as a mismatch. Clean (non-dirty) loads are unaffected.
    #[serde(default)]
    chunk_count: Option<u64>,
    /// `Store::splade_generation()` of the store state this generation was
    /// built from. The counter is bumped by every sparse-vector write batch,
    /// by the chunk-delete trigger, and by dense-embedding rewrites
    /// (`update_embeddings_with_hashes_batch`, the enrichment pass), so it
    /// advances with every vector-affecting write the index pipeline
    /// performs. Same `None` semantics as `chunk_count`.
    #[serde(default)]
    splade_generation: Option<u64>,
    /// blake3 checksums of the current generation's artifact files.
    checksums: ManifestChecksums,
}

/// blake3 hex checksums of a generation's three artifact files, inlined into
/// the manifest so the read path detects artifact corruption without a
/// separate checksum sidecar.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq, Default)]
struct ManifestChecksums {
    graph: String,
    data: String,
    ids: String,
}

impl HnswManifest {
    /// The store-state stamp recorded at save, if this manifest has one.
    /// Both fields are written together; a half-present pair (hand-edited
    /// manifest) counts as no stamp.
    fn store_stamp(&self) -> Option<StoreStamp> {
        match (self.chunk_count, self.splade_generation) {
            (Some(chunk_count), Some(splade_generation)) => Some(StoreStamp {
                chunk_count,
                splade_generation,
            }),
            _ => None,
        }
    }

    /// The recorded checksum for one of the three artifact extensions.
    fn checksum_for(&self, ext: &str) -> Option<&str> {
        match ext {
            "hnsw.graph" => Some(&self.checksums.graph),
            "hnsw.data" => Some(&self.checksums.data),
            "hnsw.ids" => Some(&self.checksums.ids),
            _ => None,
        }
    }
}

/// The hnsw_rs basename for a given generation. The library's `file_dump` /
/// `HnswIo::new` append `.hnsw.graph` / `.hnsw.data`, so a generation
/// basename of `index.gen42` yields `index.gen42.hnsw.graph` on both sides.
fn gen_basename(basename: &str, generation: u64) -> String {
    format!("{basename}.gen{generation}")
}

/// Path of the manifest pointer for `basename`.
fn manifest_path(dir: &Path, basename: &str) -> std::path::PathBuf {
    dir.join(format!("{basename}.hnsw.manifest"))
}

/// Extract the generation number from an artifact filename, or `None` if the
/// name is not a `{basename}.gen<N>.hnsw.<ext>` artifact for THIS basename.
/// The basename guard keeps `index` and `index_base` artifacts (which share a
/// directory) from being mistaken for each other.
fn parse_generation(name: &str, basename: &str) -> Option<u64> {
    let rest = name.strip_prefix(&format!("{basename}.gen"))?;
    let digits = rest.split(".hnsw.").next()?;
    digits.parse::<u64>().ok()
}

/// Highest generation number present among `basename`'s artifact files on
/// disk, ignoring the manifest. A fresh save picks a generation above this so
/// it never reuses a number an orphaned (crashed-save) artifact still holds.
fn max_disk_generation(dir: &Path, basename: &str) -> Option<u64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut max: Option<u64> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(generation) = parse_generation(&name, basename) {
            max = Some(max.map_or(generation, |m| m.max(generation)));
        }
    }
    max
}

/// blake3 hex of a file's contents, streamed so the whole file never sits in
/// memory at once.
fn hash_file(path: &Path) -> Result<String, HnswError> {
    let file = std::fs::File::open(path).map_err(|e| {
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
    Ok(hasher.finalize().to_hex().to_string())
}

/// Best-effort fsync of a directory so recently created / renamed entries are
/// durable against power loss. A no-op on platforms that don't support opening
/// a directory for fsync (Windows journals renames in its own metadata).
fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    match std::fs::File::open(dir) {
        Ok(f) => {
            if let Err(e) = f.sync_all() {
                tracing::debug!(
                    error = %e,
                    dir = %dir.display(),
                    "fsync of HNSW directory failed (non-fatal)"
                );
            }
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                dir = %dir.display(),
                "could not open HNSW directory for fsync"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Remove every generation artifact for `basename` except `keep_gen`, plus any
/// legacy fixed-name sidecars and `.bak` debris. Must run under the exclusive
/// save lock (no concurrent saver can be mid-flight writing a generation).
/// Best-effort: a failed unlink is logged, not fatal — the next save GCs it.
fn gc_stale_generations(dir: &Path, basename: &str, keep_gen: u64) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(generation) = parse_generation(&name, basename) {
                if generation != keep_gen {
                    if let Err(e) = std::fs::remove_file(entry.path()) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            tracing::debug!(
                                path = %entry.path().display(),
                                error = %e,
                                "Failed to GC stale HNSW generation artifact"
                            );
                        }
                    }
                }
            }
        }
    }
    for ext in LEGACY_HNSW_EXTENSIONS {
        let _ = std::fs::remove_file(dir.join(format!("{}.{}", basename, ext)));
        let _ = std::fs::remove_file(dir.join(format!("{}.{}.bak", basename, ext)));
    }
}

/// Resolved on-disk paths for a persisted HNSW generation's artifacts, plus
/// the manifest pointer. Returned by [`HnswIndex::artifact_paths`] for callers
/// that report on or manipulate the raw files.
#[derive(Debug, Clone)]
pub struct HnswArtifacts {
    /// The current generation number the manifest points at.
    pub generation: u64,
    /// `{basename}.gen<N>.hnsw.graph`.
    pub graph: std::path::PathBuf,
    /// `{basename}.gen<N>.hnsw.data`.
    pub data: std::path::PathBuf,
    /// `{basename}.gen<N>.hnsw.ids`.
    pub ids: std::path::PathBuf,
    /// `{basename}.hnsw.manifest`.
    pub manifest: std::path::PathBuf,
}

/// Store state an HNSW sidecar set reflects, captured at save time.
///
/// The pair plays the same role CAGRA's `chunk_count` + `splade_generation`
/// sidecar fields play (`src/cagra.rs`): a cheap discriminator that lets the
/// load path ask "does this index reflect the live store?" instead of only
/// "are these files internally consistent?". `splade_generation` advances on
/// every sparse-vector write batch, on every chunk delete (SQLite trigger),
/// and on every dense-embedding rewrite
/// (`update_embeddings_with_hashes_batch` — the enrichment pass — bumps it
/// explicitly so enrichment-only reindexes move the stamp); `chunk_count`
/// catches insert-only paths that might not touch sparse rows. Every
/// vector-affecting write path therefore moves the stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreStamp {
    /// `Store::chunk_count()` at capture time.
    pub chunk_count: u64,
    /// `Store::splade_generation()` at capture time.
    pub splade_generation: u64,
}

impl StoreStamp {
    /// Capture the current stamp from a live store.
    pub fn read<Mode>(store: &crate::store::Store<Mode>) -> Result<Self, crate::store::StoreError> {
        Ok(Self {
            chunk_count: store.chunk_count()?,
            splade_generation: store.splade_generation()?,
        })
    }
}

/// What a stamped save actually did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The sidecar set on disk now reflects this index.
    Saved,
    /// The save was discarded under the exclusive lock: the live store moved
    /// past the snapshot this index was built from, so persisting it would
    /// overwrite a newer index with stale contents. Nothing on disk changed.
    /// Callers must NOT clear the `hnsw_dirty` flag on this outcome.
    DiscardedStale,
}

/// Stamp context for [`HnswIndex::save_stamped`]: the snapshot the index
/// contents reflect, plus a re-reader invoked under the exclusive save lock.
struct SaveStamp<'a> {
    /// Re-reads the live store stamp. Called only while the exclusive save
    /// lock is held, so the comparison against `snapshot` cannot race a
    /// concurrent in-process saver.
    live: &'a dyn Fn() -> Result<StoreStamp, HnswError>,
    /// Store state the index contents were built from (including any delta
    /// replayed on top of the build snapshot).
    snapshot: StoreStamp,
}

/// Read and validate the `{basename}.hnsw.manifest` pointer, if present.
///
/// - Missing manifest → `Ok(None)` (no index, or a legacy pre-manifest set).
/// - Present but malformed / unknown magic / newer version → typed error.
fn read_manifest(dir: &Path, basename: &str) -> Result<Option<HnswManifest>, HnswError> {
    let path = manifest_path(dir, basename);
    if !path.exists() {
        return Ok(None);
    }

    // Size cap: the manifest is a small JSON object; anything beyond a few KB
    // is corruption or tampering.
    const MAX_MANIFEST_SIZE: u64 = 64 * 1024;
    let size = std::fs::metadata(&path)
        .map_err(|e| {
            HnswError::Internal(format!(
                "Failed to stat HNSW manifest {}: {}",
                path.display(),
                e
            ))
        })?
        .len();
    if size > MAX_MANIFEST_SIZE {
        return Err(HnswError::Internal(format!(
            "HNSW manifest {} is {} bytes, exceeds {} byte limit",
            path.display(),
            size,
            MAX_MANIFEST_SIZE
        )));
    }

    let raw = std::fs::read(&path).map_err(|e| {
        HnswError::Internal(format!(
            "Failed to read HNSW manifest {}: {}",
            path.display(),
            e
        ))
    })?;
    let manifest: HnswManifest = serde_json::from_slice(&raw).map_err(|e| {
        HnswError::Internal(format!(
            "Failed to parse HNSW manifest {}: {}",
            path.display(),
            e
        ))
    })?;

    if manifest.magic != HNSW_MANIFEST_MAGIC {
        return Err(HnswError::Internal(format!(
            "HNSW manifest {} has unexpected magic {:?} (want {:?})",
            path.display(),
            manifest.magic,
            HNSW_MANIFEST_MAGIC
        )));
    }
    if manifest.version > HNSW_MANIFEST_VERSION {
        return Err(HnswError::Internal(format!(
            "HNSW manifest {} has version {} — newer than this binary understands ({}). \
             Run 'cqs index --force' or upgrade cqs.",
            path.display(),
            manifest.version,
            HNSW_MANIFEST_VERSION
        )));
    }

    Ok(Some(manifest))
}

/// Like [`read_manifest`] but collapses any read / parse / validation error to
/// `None`. Used by the save path (a corrupt manifest must never block a new
/// save — that is exactly the persistence wedge this scheme removes) and by
/// the infallible accessors (`exists`, `count_vectors`, `artifact_paths`).
fn read_manifest_lenient(dir: &Path, basename: &str) -> Option<HnswManifest> {
    match read_manifest(dir, basename) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, basename, "Ignoring unreadable HNSW manifest");
            None
        }
    }
}

/// Read the persisted distance metric for an index, if any.
///
/// - Missing manifest → `Ok(Cosine)`: every index predating the manifest was
///   built with `DistCosine` by construction, and the build default is cosine.
/// - Present but malformed / unknown magic / newer version / unknown metric
///   → typed error. Never a silent cosine fallback: misreading a dot-product
///   index as cosine produces wrong distances, not a crash.
fn read_manifest_metric(dir: &Path, basename: &str) -> Result<DistanceMetric, HnswError> {
    let Some(manifest) = read_manifest(dir, basename)? else {
        tracing::debug!(
            dir = %dir.display(),
            basename,
            "No HNSW manifest (legacy/absent index) — metric is cosine"
        );
        return Ok(DistanceMetric::Cosine);
    };
    manifest.metric.parse::<DistanceMetric>().map_err(|e| {
        HnswError::Internal(format!(
            "HNSW manifest for {}/{} has invalid metric: {}",
            dir.display(),
            basename,
            e
        ))
    })
}

/// Verify that a persisted HNSW generation is both intact AND reflects the
/// live store. This is the dirty-flag self-heal check: the `hnsw_dirty` flag
/// is set before chunk writes and cleared after a successful HNSW save, so a
/// set flag means "chunks may have been committed after the last save". A
/// checksum pass alone cannot refute that — the manifest is written by the
/// same save it describes, so a complete previous-generation set always
/// verifies. The store-state stamp is what distinguishes "died after the
/// save landed" (heal) from "died after the chunk commit, before the save"
/// (stay dirty, brute-force until rebuild).
///
/// A manifest without a stamp (a store-less `save()`) fails this check: it
/// cannot prove it postdates the last chunk write. Clean (non-dirty) loads of
/// such an index are unaffected — this function is only on the dirty path.
pub fn verify_hnsw_current<Mode>(
    dir: &Path,
    basename: &str,
    store: &crate::store::Store<Mode>,
) -> Result<(), HnswError> {
    verify_hnsw_checksums(dir, basename)?;
    let live = StoreStamp::read(store).map_err(|e| {
        HnswError::Internal(format!(
            "Failed to read live store stamp for HNSW staleness check: {}",
            e
        ))
    })?;
    let stamped = read_manifest(dir, basename)?.and_then(|m| m.store_stamp());
    match stamped {
        Some(s) if s == live => Ok(()),
        Some(s) => Err(HnswError::Internal(format!(
            "HNSW manifest stamp (chunks={}, generation={}) does not match the live store \
             (chunks={}, generation={}) — the index predates the last chunk write",
            s.chunk_count, s.splade_generation, live.chunk_count, live.splade_generation
        ))),
        None => Err(HnswError::Internal(
            "HNSW manifest has no store-state stamp — cannot prove the index postdates \
             the last chunk write"
                .to_string(),
        )),
    }
}

/// Verify the current HNSW generation's artifact checksums using blake3.
///
/// Reads the manifest to resolve the current generation, then hashes each of
/// its `{basename}.gen<N>.hnsw.{graph,data,ids}` artifacts and compares
/// against the checksums the manifest recorded.
///
/// # Security Model
///
/// **WARNING:** These checksums detect accidental corruption only (disk errors,
/// incomplete writes). They do NOT provide tamper-detection or authenticity
/// guarantees - an attacker with filesystem access can update both the
/// artifacts and the recorded checksums. For tamper-proofing, the manifest
/// would need to be signed or stored separately in a trusted location.
///
/// # Errors
///
/// - No manifest → `Internal` ("No HNSW manifest").
/// - A referenced artifact is missing → `NotFound` (the partial-save /
///   corrupt-generation case; the loader turns this into the rebuild path).
/// - An artifact's hash disagrees with the manifest → `ChecksumMismatch`.
pub fn verify_hnsw_checksums(dir: &Path, basename: &str) -> Result<(), HnswError> {
    // Entry span so operators can see this on the startup path. Hashing
    // potentially hundreds of MB of HNSW artifacts is the kind of thing that
    // ends up at the top of "why is `cqs index` slow on cold start"
    // investigations; an untimed pass is opaque.
    let started = std::time::Instant::now();
    let _span = tracing::info_span!(
        "verify_hnsw_checksums",
        dir = %dir.display(),
        basename
    )
    .entered();

    let Some(manifest) = read_manifest(dir, basename)? else {
        return Err(HnswError::Internal(
            "No HNSW manifest for index — run 'cqs index --force' to regenerate".to_string(),
        ));
    };
    let gen_base = gen_basename(basename, manifest.generation);

    for ext in HNSW_ARTIFACT_EXTS {
        let expected = manifest.checksum_for(ext).ok_or_else(|| {
            HnswError::Internal(format!("HNSW manifest is missing the checksum for {}", ext))
        })?;
        let path = dir.join(format!("{}.{}", gen_base, ext));
        // A missing artifact referenced by the manifest is a hard failure —
        // the partial-save / corrupt-generation case. Surfaced as NotFound so
        // the load path falls into rebuild, never a wedge.
        if !path.exists() {
            return Err(HnswError::NotFound(format!(
                "HNSW manifest references missing artifact: {} (partial save?)",
                path.display()
            )));
        }
        // Stream the file through blake3 to avoid loading it all into memory.
        let file = std::fs::File::open(&path).map_err(|e| {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                kind = ?e.kind(),
                "verify_hnsw_checksums IO failure"
            );
            HnswError::Internal(format!(
                "Failed to open {} for checksum: {}",
                path.display(),
                e
            ))
        })?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut std::io::BufReader::new(file), &mut hasher).map_err(|e| {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                kind = ?e.kind(),
                "verify_hnsw_checksums IO failure"
            );
            HnswError::Internal(format!(
                "Failed to read {} for checksum: {}",
                path.display(),
                e
            ))
        })?;
        let actual = hasher.finalize().to_hex().to_string();
        if actual != *expected {
            return Err(HnswError::ChecksumMismatch {
                file: path.display().to_string(),
                expected: expected.to_string(),
                actual,
            });
        }
    }

    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        generation = manifest.generation,
        "HNSW checksums verified"
    );
    Ok(())
}

/// Test-only fault injection seam for the save path. Thread-local so parallel
/// tests can't observe each other's injected faults (saves run synchronously
/// on the calling thread).
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;

    thread_local! {
        /// When set, `save_impl` writes and fsyncs the new generation's
        /// artifact files, then returns an error WITHOUT committing the
        /// manifest — simulating a crash in the window between artifact
        /// completion and the atomic manifest commit. The prior generation
        /// (or its absence) is left intact on disk.
        pub(crate) static FAIL_BEFORE_MANIFEST_COMMIT: RefCell<bool> =
            const { RefCell::new(false) };
    }
}

impl HnswIndex {
    /// Save the index to disk as a new generation.
    ///
    /// Writes a fresh generation into the directory:
    /// - `{basename}.gen<N>.hnsw.graph` - HNSW graph structure
    /// - `{basename}.gen<N>.hnsw.data` - Vector data
    /// - `{basename}.gen<N>.hnsw.ids` - Chunk ID mapping
    ///
    /// then flips the pointer with a single atomic commit:
    /// - `{basename}.hnsw.manifest` - JSON manifest (current generation,
    ///   distance metric, store-state stamp, per-artifact blake3 checksums)
    ///
    /// # Crash safety
    /// The generation number `N` is chosen above every artifact already on
    /// disk, so writing the new generation never touches the live set. The
    /// manifest is committed via `atomic_replace` (fsync + rename), the single
    /// point at which generation `N` becomes current — a crash before that
    /// leaves the prior generation (or its absence) intact and loadable, and
    /// leaves the new generation as an orphan the next save's post-commit GC
    /// removes. There is no `.bak` shuffle and no save that a prior crash can
    /// wedge.
    ///
    /// This variant writes no store-state stamp into the manifest, so the
    /// dirty-flag self-heal will refuse the resulting generation (see
    /// [`verify_hnsw_current`]). Production callers that have a `Store` in
    /// scope must use [`save_stamped`](Self::save_stamped); this remains for
    /// tests and store-less callers.
    pub fn save(&self, dir: &Path, basename: &str) -> Result<(), HnswError> {
        self.save_impl(dir, basename, None).map(|_| ())
    }

    /// Save with a store-state stamp, discarding the save if the store has
    /// moved past `snapshot`.
    ///
    /// `snapshot` is the [`StoreStamp`] of the store state this index's
    /// contents reflect (the build snapshot, plus any delta replayed on top).
    /// Under the exclusive save lock the live stamp is re-read and compared:
    /// a mismatch means a writer this index never observed committed chunks
    /// after the snapshot — persisting would overwrite a newer on-disk index
    /// with stale contents while stamping it current. The save is discarded
    /// instead ([`SaveOutcome::DiscardedStale`]), and even if a stale save
    /// raced in through the remaining window between the caller's snapshot
    /// capture and this lock, its stamp would be the old generation — the
    /// dirty-path check ([`verify_hnsw_current`]) refuses it at next load.
    pub fn save_stamped<Mode>(
        &self,
        dir: &Path,
        basename: &str,
        store: &crate::store::Store<Mode>,
        snapshot: StoreStamp,
    ) -> Result<SaveOutcome, HnswError> {
        let live = || {
            StoreStamp::read(store).map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to re-read store stamp during HNSW save: {}",
                    e
                ))
            })
        };
        self.save_impl(
            dir,
            basename,
            Some(SaveStamp {
                live: &live,
                snapshot,
            }),
        )
    }

    /// Shared save body. See [`save`](Self::save) for the layout and
    /// crash-safety contract, [`save_stamped`](Self::save_stamped) for the
    /// stamp semantics.
    fn save_impl(
        &self,
        dir: &Path,
        basename: &str,
        stamp: Option<SaveStamp<'_>>,
    ) -> Result<SaveOutcome, HnswError> {
        let _span = tracing::debug_span!("hnsw_save", dir = %dir.display(), basename).entered();
        tracing::info!(dir = %dir.display(), basename, "Saving HNSW index");

        // Verify ID map matches HNSW vector count before saving.
        let hnsw_count = self.inner.with_hnsw(|h| h.get_nb_point());
        if hnsw_count != self.id_map.len() {
            return Err(HnswError::Internal(format!(
                "HNSW/ID map count mismatch on save: HNSW has {} vectors but id_map has {}. This is a bug.",
                hnsw_count,
                self.id_map.len()
            )));
        }

        // Ensure the target directory exists.
        std::fs::create_dir_all(dir).map_err(|e| {
            HnswError::Internal(format!(
                "Failed to create directory {}: {}",
                dir.display(),
                e
            ))
        })?;

        // Acquire the exclusive save lock. It also serializes the post-commit
        // generation GC below against concurrent savers.
        // NOTE: File locking is advisory only on WSL over 9P — it prevents
        // concurrent cqs processes from racing, but not external Windows
        // process modifications.
        let lock_path = dir.join(format!("{}.hnsw.lock", basename));
        #[allow(clippy::suspicious_open_options)] // Intentional: create if missing, don't truncate
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;
        lock_file.lock().map_err(HnswError::Io)?;
        warn_wsl_advisory_locking(dir);
        tracing::debug!(lock_path = %lock_path.display(), "Acquired HNSW save lock");

        // Live store stamp, read once under the exclusive lock, for the
        // staleness discard below.
        let live_stamp: Option<StoreStamp> = match &stamp {
            Some(s) => Some((s.live)()?),
            None => None,
        };

        // Staleness discard: if the store moved past the snapshot this index
        // was built from, persisting would replace a newer on-disk index with
        // stale contents stamped as current. Nothing has been written yet, so
        // discarding here is side-effect-free.
        //
        // There is deliberately NO stale-debris guard preceding this. The
        // generation scheme makes every prior on-disk state — a committed
        // generation, a half-written orphan generation, legacy sidecars, even
        // leftover `.bak` debris — safe to write past, so a crashed previous
        // save can never wedge this one.
        if let (Some(s), Some(live)) = (&stamp, live_stamp) {
            if live != s.snapshot {
                tracing::warn!(
                    basename,
                    snapshot_chunks = s.snapshot.chunk_count,
                    snapshot_generation = s.snapshot.splade_generation,
                    live_chunks = live.chunk_count,
                    live_generation = live.splade_generation,
                    "Discarding HNSW save: store moved past the build snapshot — \
                     an unobserved writer committed chunks; the on-disk index is \
                     left untouched and the dirty flag must stay set"
                );
                return Ok(SaveOutcome::DiscardedStale);
            }
        }

        // Choose a generation number above the current manifest AND every
        // artifact already on disk, so writing the new generation can never
        // overwrite the live set or a not-yet-GC'd orphan. The manifest read
        // is lenient — a corrupt manifest must not block the save.
        let current_gen = read_manifest_lenient(dir, basename).map(|m| m.generation);
        let disk_gen = max_disk_generation(dir, basename);
        let generation = current_gen
            .unwrap_or(0)
            .max(disk_gen.unwrap_or(0))
            .saturating_add(1);
        let gen_base = gen_basename(basename, generation);

        // Dump the graph + data for the new generation directly into `dir`.
        // The generation suffix is the isolation: the live set is a different
        // generation, so a crash mid-dump leaves an orphan the loader ignores
        // (the manifest still points at the prior generation). No temp dir or
        // promote pass is needed.
        self.inner
            .with_hnsw(|g| g.file_dump(dir, &gen_base))
            .map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to dump HNSW to {}/{}: {}",
                    dir.display(),
                    gen_base,
                    e
                ))
            })?;

        // Tighten permissions on the library-written graph/data (it uses the
        // process umask) and fsync each so its bytes are durable before the
        // manifest that records their checksums is committed.
        for ext in ["hnsw.graph", "hnsw.data"] {
            let path = dir.join(format!("{}.{}", gen_base, ext));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) =
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                {
                    tracing::debug!(path = %path.display(), error = %e, "Failed to set HNSW artifact permissions");
                }
            }
            let f = std::fs::File::open(&path).map_err(|e| {
                HnswError::Internal(format!("Failed to open {} to fsync: {}", path.display(), e))
            })?;
            f.sync_all().map_err(|e| {
                HnswError::Internal(format!("Failed to fsync {}: {}", path.display(), e))
            })?;
        }

        // Stream the ID map to the generation's `.ids` file (mode 0600), then
        // fsync so a power cut can't leave the graph without its string IDs.
        let ids_path = dir.join(format!("{}.hnsw.ids", gen_base));
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
                        .open(&ids_path)
                }
                #[cfg(not(unix))]
                {
                    std::fs::File::create(&ids_path)
                }
            }
            .map_err(|e| {
                HnswError::Internal(format!("Failed to create {}: {}", ids_path.display(), e))
            })?;
            let mut writer = std::io::BufWriter::new(file);
            serde_json::to_writer(&mut writer, &self.id_map)
                .map_err(|e| HnswError::Internal(format!("Failed to serialize ID map: {}", e)))?;
            use std::io::Write;
            writer.flush().map_err(|e| {
                HnswError::Internal(format!("Failed to flush {}: {}", ids_path.display(), e))
            })?;
            writer.get_ref().sync_all().map_err(|e| {
                HnswError::Internal(format!("Failed to fsync {}: {}", ids_path.display(), e))
            })?;
        }

        // fsync the parent directory so all three artifact directory entries
        // are durable before the manifest commit.
        fsync_dir(dir);

        // Test seam: simulate a crash in the window between artifact
        // completion and the manifest commit. The prior generation is intact;
        // the just-written generation is left as an orphan.
        #[cfg(test)]
        if test_hooks::FAIL_BEFORE_MANIFEST_COMMIT.with(|h| *h.borrow()) {
            return Err(HnswError::Internal(
                "injected: crash before HNSW manifest commit (test hook)".to_string(),
            ));
        }

        // Hash the three artifacts for the manifest's integrity record.
        let checksums = ManifestChecksums {
            graph: hash_file(&dir.join(format!("{}.hnsw.graph", gen_base)))?,
            data: hash_file(&dir.join(format!("{}.hnsw.data", gen_base)))?,
            ids: hash_file(&ids_path)?,
        };

        // Build the manifest for the new generation.
        let manifest = HnswManifest {
            magic: HNSW_MANIFEST_MAGIC.to_string(),
            version: HNSW_MANIFEST_VERSION,
            generation,
            metric: self.metric().as_str().to_string(),
            chunk_count: stamp.as_ref().map(|s| s.snapshot.chunk_count),
            splade_generation: stamp.as_ref().map(|s| s.snapshot.splade_generation),
            checksums,
        };
        let manifest_json = serde_json::to_vec(&manifest).map_err(|e| {
            HnswError::Internal(format!("Failed to serialize HNSW manifest: {}", e))
        })?;

        // Write the manifest to a temp file (mode 0600 + fsync), then commit it
        // with `atomic_replace` — THE single point at which generation `N`
        // becomes current. A crash before this leaves the previous manifest
        // (or none); a crash after leaves the complete new one.
        let suffix = crate::temp_suffix();
        let manifest_tmp = dir.join(format!(".{}.hnsw.manifest.{:016x}.tmp", basename, suffix));
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
                        .open(&manifest_tmp)
                }
                #[cfg(not(unix))]
                {
                    std::fs::File::create(&manifest_tmp)
                }
            }
            .map_err(|e| {
                HnswError::Internal(format!(
                    "Failed to create {}: {}",
                    manifest_tmp.display(),
                    e
                ))
            })?;
            use std::io::Write;
            file.write_all(&manifest_json).map_err(|e| {
                HnswError::Internal(format!("Failed to write {}: {}", manifest_tmp.display(), e))
            })?;
            file.sync_all().map_err(|e| {
                HnswError::Internal(format!("Failed to fsync {}: {}", manifest_tmp.display(), e))
            })?;
        }
        crate::fs::atomic_replace(&manifest_tmp, &manifest_path(dir, basename)).map_err(|e| {
            // Clean up the temp manifest before surfacing the failure so it
            // can't accumulate as debris; the live generation is unchanged.
            let _ = std::fs::remove_file(&manifest_tmp);
            HnswError::Internal(format!("Failed to commit HNSW manifest: {}", e))
        })?;

        // Post-commit GC under the still-held lock: drop every superseded /
        // orphaned generation and any legacy fixed-name sidecars (the one-time
        // migration cleanup). Best-effort — a failed unlink is logged, not
        // fatal; the next save GCs it.
        gc_stale_generations(dir, basename, generation);

        tracing::info!(
            generation,
            "HNSW index saved: {} vectors (generation {})",
            self.id_map.len(),
            generation
        );

        Ok(SaveOutcome::Saved)
    }

    /// Load an index from disk
    ///
    /// Verifies blake3 checksums before loading to mitigate bincode deserialization risks.
    /// Memory is properly freed when the HnswIndex is dropped.
    pub fn load_with_dim(dir: &Path, basename: &str, dim: usize) -> Result<Self, HnswError> {
        let _span = tracing::debug_span!("hnsw_load", dir = %dir.display(), basename).entered();

        // The stale-temp cleanup, the manifest read, and the existence check
        // all happen INSIDE the shared-lock critical section.
        //
        // Cleanup before the lock would race a concurrent `save()`: pre-lock,
        // a loader could `remove_dir_all` a leftover temp while a saver is
        // mid-flight. Once the shared lock is held, no exclusive saver is
        // in-flight, so any surviving `.tmp` entry is genuinely stale.
        //
        // Reading the manifest and checking existence before the lock would
        // open a TOCTOU window: a concurrent `save()` could commit a new
        // manifest generation and GC the old artifacts out from under a
        // pre-lock reader. The manifest commit (`atomic_replace`) and the
        // generation GC both require the exclusive lock, so holding the shared
        // lock across resolve-then-load pins one generation for the load.
        //
        // The generation scheme closes the old half-replaced-set hazard
        // outright: the current generation is whatever the manifest points at,
        // its artifacts are written under a fresh generation name before the
        // pointer flips, and superseded artifacts are GC'd only after. A load
        // never observes a partially-replaced set — only a whole generation or
        // none.

        // Acquire shared lock for the read phase only (released before
        // return). Protects the on-disk artifact files from being replaced by
        // a concurrent `save()` while this function is reading them — once the
        // data is in memory the copy is independent and the lock is no longer
        // needed.
        //
        // NOTE: File locking is advisory only on WSL over 9P. The lock
        // is best-effort cross-process; an external Windows tool can
        // still modify the files.
        //
        // **Why the lock must NOT outlive the read phase**: keeping the
        // shared lock alive for the index's entire lifetime (by storing
        // it on the returned `HnswIndex`) deadlocks the daemon. The same
        // process's HNSW rebuild thread calls `save()` from a *second*
        // file descriptor on the same lock path; Linux `flock(2)`'s
        // exclusive-waits-for-shared-even-self semantics block the
        // rebuild thread permanently. Dropping the lock at the bottom of
        // this function makes the load and save paths use disjoint open
        // descriptions so the in-process self-deadlock can't form.
        // Cross-process exclusion during runtime is theoretical anyway —
        // concurrent writers race on the SQLite writer lock first.
        let lock_path = dir.join(format!("{}.hnsw.lock", basename));
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        // Bound the wait with try-lock-and-retry so a wedged peer process
        // holding the exclusive save lock doesn't hang every reader
        // indefinitely. 5 attempts × 200 ms (with jitter) = ~1s ceiling;
        // daemons can retry the load and CLI users see a clear error
        // instead of a silent hang. Without this, a paused `cqs index`
        // (debugger / SIGSTOP) takes down all readers.
        let lock_acquired = (0..5).any(|attempt| {
            if attempt > 0 {
                let jitter = std::time::Duration::from_millis(200 + (attempt * 37) as u64);
                std::thread::sleep(jitter);
            }
            lock_file.try_lock_shared().is_ok()
        });
        if !lock_acquired {
            return Err(HnswError::Internal(format!(
                "HNSW load lock contended after 5 retries — concurrent save in progress at {}",
                lock_path.display()
            )));
        }
        warn_wsl_advisory_locking(dir);
        tracing::debug!(lock_path = %lock_path.display(), "Acquired HNSW load lock (shared, released after read)");

        // Clean up stale temps from interrupted saves. This runs under the
        // shared lock: a live `save()` holds the exclusive lock while it
        // writes, so any `.tmp` entry observed here is from a crashed/killed
        // save and safe to remove. Temps carry an unpredictable suffix, so
        // match by prefix+suffix. Two shapes: a legacy staging directory, and
        // the manifest-commit temp file left by a crash mid-`atomic_replace`.
        if let Ok(entries) = std::fs::read_dir(dir) {
            let prefix = format!(".{}.", basename);
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(".tmp") {
                    tracing::info!(dir = %entry.path().display(), "Cleaning up interrupted HNSW save");
                    if entry.path().is_dir() {
                        let _ = std::fs::remove_dir_all(entry.path());
                    } else {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }

        // Resolve the current generation from the manifest. No manifest → the
        // index is absent or legacy (pre-manifest); either way NotFound sends
        // the caller to the rebuild path.
        let manifest = read_manifest(dir, basename)?
            .ok_or_else(|| HnswError::NotFound(dir.display().to_string()))?;
        let gen_base = gen_basename(basename, manifest.generation);
        let graph_path = dir.join(format!("{}.hnsw.graph", gen_base));
        let data_path = dir.join(format!("{}.hnsw.data", gen_base));
        let id_map_path = dir.join(format!("{}.hnsw.ids", gen_base));

        // Existence check under the shared lock. A concurrent `save()` that
        // wanted to change the current generation needs the exclusive lock —
        // which it can't get while we hold the shared lock — and it only flips
        // the manifest (an atomic replace) after its artifacts are durable. So
        // the generation we resolved and the files we observe here are stable
        // for the duration of the load.
        if !graph_path.exists() || !data_path.exists() || !id_map_path.exists() {
            return Err(HnswError::NotFound(dir.display().to_string()));
        }

        tracing::info!(
            dir = %dir.display(),
            basename,
            generation = manifest.generation,
            "Loading HNSW index"
        );
        verify_hnsw_checksums(dir, basename)?;

        // Resolve the distance metric. The stored value (manifest; legacy
        // indexes without one are cosine) always wins — but an *explicit*
        // conflicting CQS_DISTANCE_METRIC is a typed error rather than a silent
        // reinterpretation of distances.
        let stored_metric = read_manifest_metric(dir, basename)?;
        if let Some(requested) = DistanceMetric::from_env().map_err(HnswError::Internal)? {
            if requested != stored_metric {
                return Err(HnswError::MetricMismatch {
                    stored: stored_metric,
                    requested,
                });
            }
        }

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
        // Deserialize as `Vec<String>` (serde's default), then convert in
        // place to `Vec<Box<str>>`. The wire format is JSON `[String, ...]`.
        // `String::into_boxed_str()` is zero-copy (shrinks the existing heap
        // allocation, drops the `cap` field).
        let id_map_strings: Vec<String> = serde_json::from_reader(id_map_reader)
            .map_err(|e| HnswError::Internal(format!("Failed to parse ID map: {}", e)))?;
        let id_map: Vec<Box<str>> = id_map_strings
            .into_iter()
            .map(String::into_boxed_str)
            .collect();

        // Cap element count to prevent memory exhaustion from crafted id_map files.
        // 10M entries at ~64 bytes average ID = ~640MB — well above any real codebase.
        const MAX_ID_MAP_ENTRIES: usize = 10_000_000;
        if id_map.len() > MAX_ID_MAP_ENTRIES {
            return Err(HnswError::Internal(format!(
                "ID map has {} entries, exceeding {} limit — possible corruption",
                id_map.len(),
                MAX_ID_MAP_ENTRIES
            )));
        }

        // Validate data file size against id_map before bincode deserialization.
        // A crafted file could claim more vectors than the id_map supports, causing
        // unbounded allocation during deserialization. Each vector is `dim` f32s,
        // with 2x headroom for HNSW graph overhead (neighbor lists, metadata).
        //
        // Use checked_mul so a pathological `dim` argument (e.g. a model with
        // a huge embedding dimension) can't overflow usize silently on 32-bit
        // targets. On 64-bit the product fits for any realistic corpus, but
        // defense-in-depth is cheap here.
        if !id_map.is_empty() {
            let expected_max_data = id_map
                .len()
                .checked_mul(dim)
                .and_then(|v| v.checked_mul(std::mem::size_of::<f32>()))
                .and_then(|v| v.checked_mul(2))
                .ok_or_else(|| {
                    HnswError::Internal(format!(
                        "expected_max_data overflow: id_map={} dim={}",
                        id_map.len(),
                        dim
                    ))
                })?;
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
        let hnsw_io_cell = Box::new(HnswIoCell(UnsafeCell::new(HnswIo::new(dir, &gen_base))));

        let loaded = LoadedHnsw::try_new(hnsw_io_cell, |cell| {
            // SAFETY: Exclusive access during construction — no other references exist.
            // After this closure returns, the UnsafeCell is never accessed again directly.
            let io = unsafe { &mut *cell.0.get() };
            // Instantiate the dist type recorded in the manifest. hnsw_rs
            // additionally cross-checks the distance name embedded in the
            // graph file, so a forged manifest metric cannot misread distances.
            HnswGraph::load(io, stored_metric)
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

        tracing::info!(count = id_map.len(), metric = %stored_metric, "HNSW index loaded");

        // Release the load-phase shared lock before returning. The
        // in-memory `Loaded(...)` variant is fully self-contained
        // (vectors copied into rust-allocated buffers via `bincode`,
        // not mmap'd), so the on-disk file can be modified or removed
        // without affecting this instance. Keeping the lock alive
        // across the return would self-deadlock the daemon's rebuild
        // thread on its next `save()` (Linux flock; see the lock
        // acquisition site above).
        drop(lock_file);

        Ok(Self {
            inner: HnswInner::Loaded(loaded),
            id_map,
            ef_search: super::ef_search(),
            dim,
            _lock_file: None,
        })
    }

    /// Read the stored distance metric for a persisted index without
    /// loading it. Missing manifest → `Cosine` (legacy indexes, and the build
    /// default when no index exists yet). Used by derived backends (CAGRA
    /// rebuild path) to stay consistent with the slot's HNSW metric when
    /// `CQS_DISTANCE_METRIC` is unset.
    pub fn stored_metric(dir: &Path, basename: &str) -> Result<DistanceMetric, HnswError> {
        read_manifest_metric(dir, basename)
    }

    /// Check if an HNSW index exists at the given path.
    ///
    /// True iff a readable manifest is present AND its current generation's
    /// three artifact files are on disk. A legacy pre-manifest set reads as
    /// absent (no manifest), which is what routes it to the rebuild path.
    pub fn exists(dir: &Path, basename: &str) -> bool {
        let Some(manifest) = read_manifest_lenient(dir, basename) else {
            return false;
        };
        let gen_base = gen_basename(basename, manifest.generation);
        HNSW_ARTIFACT_EXTS
            .iter()
            .all(|ext| dir.join(format!("{}.{}", gen_base, ext)).exists())
    }

    /// Resolved on-disk paths for the current generation's artifacts, or
    /// `None` when no readable manifest is present. Lets callers that report
    /// on or manipulate the raw files (stats sizing, corruption tests) find
    /// them without duplicating the generation-resolution logic.
    pub fn artifact_paths(dir: &Path, basename: &str) -> Option<HnswArtifacts> {
        let manifest = read_manifest_lenient(dir, basename)?;
        let gen_base = gen_basename(basename, manifest.generation);
        Some(HnswArtifacts {
            generation: manifest.generation,
            graph: dir.join(format!("{}.hnsw.graph", gen_base)),
            data: dir.join(format!("{}.hnsw.data", gen_base)),
            ids: dir.join(format!("{}.hnsw.ids", gen_base)),
            manifest: manifest_path(dir, basename),
        })
    }

    /// Remove all on-disk HNSW state for `basename` — the manifest, every
    /// generation's artifacts, and any legacy fixed-name sidecars / `.bak`
    /// debris — under the exclusive lock. Idempotent (a no-op when nothing is
    /// present). Used by GC to force a clean rebuild: with the manifest gone,
    /// concurrent loads see NotFound and fall back to brute-force search.
    pub fn purge_artifacts(dir: &Path, basename: &str) -> Result<(), HnswError> {
        let _span = tracing::info_span!("hnsw_purge", dir = %dir.display(), basename).entered();
        if !dir.exists() {
            return Ok(());
        }
        let lock_path = dir.join(format!("{}.hnsw.lock", basename));
        #[allow(clippy::suspicious_open_options)] // Intentional: create if missing, don't truncate
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;
        lock_file.lock().map_err(HnswError::Io)?;
        warn_wsl_advisory_locking(dir);

        // Remove the manifest first so a concurrent load resolves NotFound
        // even if an artifact unlink below lags.
        if let Err(e) = std::fs::remove_file(manifest_path(dir, basename)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(HnswError::Internal(format!(
                    "Failed to remove HNSW manifest for {}: {}",
                    basename, e
                )));
            }
        }
        // Remove every generation's artifacts (u64::MAX keeps nothing) plus
        // legacy sidecars and `.bak` debris.
        gc_stale_generations(dir, basename, u64::MAX);
        Ok(())
    }

    /// Get vector count without loading the full index (fast, for stats).
    ///
    /// Uses `BufReader` + `serde_json::from_reader` to avoid reading the entire
    /// id map file into a String first. The file is a JSON array of chunk ID strings.
    pub fn count_vectors(dir: &Path, basename: &str) -> Option<usize> {
        // Resolve the current generation's id map via the manifest; no
        // manifest → no index → no count.
        let manifest = read_manifest_lenient(dir, basename)?;
        let gen_base = gen_basename(basename, manifest.generation);
        let id_map_path = dir.join(format!("{}.hnsw.ids", gen_base));
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
        // Best-effort shared lock on the id-map file itself (advisory). This
        // is NOT the save/load `.hnsw.lock` — count_vectors is a cheap stats
        // read that only needs the id-map bytes to be internally consistent,
        // not full serialization against a concurrent generation swap.
        if let Err(e) = file.lock_shared() {
            tracing::debug!(error = %e, "Could not lock HNSW id map");
            return None;
        }
        // Guard against oversized id map files.
        //
        // 1 GB cap aligns with the hard-load path's MAX_ID_MAP_ENTRIES =
        // 10M × ~64 byte strings = ~640 MB. A lower cap would make
        // `cqs stats` / health report "unknown vector count" for large
        // corpora well below the 1M+ scaling target.
        const MAX_ID_MAP_SIZE: u64 = 1024 * 1024 * 1024; // 1GB
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
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an array")
            }
            /// Count sequence elements without materializing them.
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

    /// Load the base (non-enriched) HNSW index.
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

    /// blake3 hex of a file on disk (test-side mirror of `hash_file`).
    fn hash_test_file(path: &Path) -> String {
        let mut hasher = blake3::Hasher::new();
        let mut file = std::fs::File::open(path).unwrap();
        std::io::copy(&mut file, &mut hasher).unwrap();
        hasher.finalize().to_hex().to_string()
    }

    /// Fabricate a manifest for `basename` at `generation`, hashing whatever
    /// bytes currently sit at `{basename}.gen{generation}.hnsw.{graph,data,ids}`.
    /// The test-side equivalent of the old `write_checksums`: it makes a
    /// hand-built or mutated generation loadable by pinning matching checksums.
    fn write_manifest(
        dir: &Path,
        basename: &str,
        generation: u64,
        metric: &str,
        stamp: Option<StoreStamp>,
    ) {
        let gb = gen_basename(basename, generation);
        let checksums = ManifestChecksums {
            graph: hash_test_file(&dir.join(format!("{gb}.hnsw.graph"))),
            data: hash_test_file(&dir.join(format!("{gb}.hnsw.data"))),
            ids: hash_test_file(&dir.join(format!("{gb}.hnsw.ids"))),
        };
        let (chunk_count, splade_generation) = match stamp {
            Some(s) => (Some(s.chunk_count), Some(s.splade_generation)),
            None => (None, None),
        };
        let manifest = HnswManifest {
            magic: HNSW_MANIFEST_MAGIC.to_string(),
            version: HNSW_MANIFEST_VERSION,
            generation,
            metric: metric.to_string(),
            chunk_count,
            splade_generation,
            checksums,
        };
        std::fs::write(
            manifest_path(dir, basename),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    /// Write the three artifact files for a fabricated generation `1`, with
    /// `graph`/`data` sized as requested (sparse) and `ids` = `ids_json`, then
    /// pin a matching manifest so the load path reaches the size/parse checks.
    fn fabricate_generation(dir: &Path, graph_len: u64, data_len: u64, ids_json: &[u8]) {
        let gb = gen_basename("test", 1);
        let graph = dir.join(format!("{gb}.hnsw.graph"));
        let data = dir.join(format!("{gb}.hnsw.data"));
        let ids = dir.join(format!("{gb}.hnsw.ids"));
        std::fs::File::create(&graph)
            .unwrap()
            .set_len(graph_len)
            .unwrap();
        std::fs::File::create(&data)
            .unwrap()
            .set_len(data_len)
            .unwrap();
        std::fs::write(&ids, ids_json).unwrap();
        write_manifest(dir, "test", 1, "cosine", None);
    }

    #[test]
    fn test_load_rejects_oversized_graph_file() {
        let tmp = TempDir::new().unwrap();
        // Oversized graph (just over the 500MB limit), sparse so no real I/O.
        fabricate_generation(tmp.path(), 501 * 1024 * 1024, 5, b"[]");

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
        // Oversized data (just over the 1GB limit).
        fabricate_generation(tmp.path(), 5, 1025 * 1024 * 1024, b"[]");

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
        // id_map claims 2 vectors, but the data file is far larger than
        // 2 * 768 * 4 * 2 = 12,288 bytes would allow.
        fabricate_generation(tmp.path(), 5, 1_000_000, br#"["a","b"]"#);

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

    /// A generation set present but with no manifest pointer reads as absent:
    /// `load_with_dim` returns `NotFound`, which routes the caller to rebuild.
    /// This is the migration path (a legacy index has artifacts but no
    /// manifest) and the "manifest lost" corruption case in one.
    #[test]
    fn test_load_without_manifest_is_not_found() {
        let tmp = TempDir::new().unwrap();
        let gb = gen_basename("test", 1);
        std::fs::write(tmp.path().join(format!("{gb}.hnsw.graph")), b"data").unwrap();
        std::fs::write(tmp.path().join(format!("{gb}.hnsw.data")), b"data").unwrap();
        std::fs::write(tmp.path().join(format!("{gb}.hnsw.ids")), b"[]").unwrap();
        // No manifest.

        match HnswIndex::load_with_dim(tmp.path(), "test", crate::EMBEDDING_DIM) {
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("not found") || msg.contains("NotFound"),
                    "Expected NotFound error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected NotFound for a manifest-less generation set"),
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

    /// Regression for the in-process flock self-deadlock between
    /// `load_with_dim`'s shared lock and a subsequent `save()`'s
    /// exclusive lock. If the load held a `shared` flock for the entire
    /// lifetime of the returned `HnswIndex` via the `_lock_file` field,
    /// the daemon's HNSW rebuild thread would open a *second* fd on the
    /// same lock path inside `save()` and call `lock()` (exclusive),
    /// which blocks forever on Linux because `flock(2)` exclusive-waits
    /// for *all* shared holders, including ones held by the same process
    /// via a different open description. That manifests as a permanently
    /// `state == rebuilding` daemon with one `cqs-hnsw-rebuild` thread
    /// parked in `locks_lock_inode_wait`.
    ///
    /// `load_with_dim` releases the load lock at the end (the in-memory
    /// `Loaded(...)` is independent of the on-disk files), so a
    /// same-process `save()` after a `load()` can proceed. We model this
    /// by loading the on-disk index, then saving a *fresh* `HnswIndex`
    /// (built in memory) to the same directory while the loaded handle is
    /// still alive. We bound the save in a worker thread with a 5-second
    /// timeout so the test fails fast on regression instead of hanging the
    /// suite.
    #[test]
    fn load_does_not_block_subsequent_save_in_same_process() {
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::Duration;

        let tmp = TempDir::new().unwrap();
        let basename = "deadlock_repro";

        let initial = HnswIndex::build_with_dim(
            vec![
                ("chunk1".to_string(), make_embedding(1)),
                ("chunk2".to_string(), make_embedding(2)),
            ],
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        initial.save(tmp.path(), basename).unwrap();

        // Step 2: load the on-disk HNSW. The shared flock used during the
        // read phase is released before this returns.
        let _loaded = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
            .expect("load should succeed");

        // Step 3: build a fresh HnswIndex in memory and save it to the
        // same directory while the loaded handle is still alive. This
        // re-enters `save()`, which opens a second fd on the same lock
        // path and calls `lock()` (exclusive) — the deadlock surface.
        let dir = tmp.path().to_path_buf();
        let basename_owned = basename.to_string();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let new_index = HnswIndex::build_with_dim(
                vec![("chunk3".to_string(), make_embedding(3))],
                crate::EMBEDDING_DIM,
            )
            .unwrap();
            // Re-save the same basename; each save writes a fresh generation,
            // so this exercises the load-then-save lock surface without any
            // debris concern.
            let result = new_index.save(&dir, &basename_owned);
            let _ = tx.send(result);
        });

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => { /* save completed cleanly — fix is in place */ }
            Ok(Err(e)) => panic!("save failed: {e:?}"),
            Err(RecvTimeoutError::Timeout) => panic!(
                "save deadlocked — regression: load's shared flock is still held while save \
                 tries to acquire exclusive. Verify `load_with_dim` drops `lock_file` before \
                 returning."
            ),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("save thread panicked without sending a result")
            }
        }
    }

    #[test]
    fn test_load_cleans_stale_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let basename = "test_index";
        let temp_dir = dir.path().join(format!(".{}.tmp", basename));
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Load should clean up the temp dir even though no index exists.
        // The cleanup happens after the shared lock is acquired (no saver
        // is mid-flight here, so the load proceeds straight through).
        let result = HnswIndex::load_with_dim(dir.path(), basename, crate::EMBEDDING_DIM);
        assert!(result.is_err()); // no index to load
        assert!(!temp_dir.exists()); // but temp dir should be cleaned
    }

    /// Pins the cleanup-under-lock ordering: while a saver holds the
    /// exclusive lock, its staging directory is LIVE and a concurrent load
    /// must not delete it. If the cleanup ever moves back in front of the
    /// shared-lock acquisition, this test fails because the staging dir
    /// gets removed while the "saver" still holds the exclusive lock.
    #[test]
    fn test_load_does_not_clean_staging_dir_while_saver_holds_lock() {
        let dir = tempfile::tempdir().unwrap();
        let basename = "test_index";

        // Simulate a mid-flight save: staging dir exists and the exclusive
        // lock is held (flock is per open file description, so this blocks
        // the load's shared lock even within the same process).
        let staging = dir
            .path()
            .join(format!(".{}.{:016x}.tmp", basename, 0xdeadbeefu64));
        std::fs::create_dir_all(&staging).unwrap();
        let lock_path = dir.path().join(format!("{}.hnsw.lock", basename));
        #[allow(clippy::suspicious_open_options)]
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        lock_file.lock().unwrap();

        // Load must fail to acquire the shared lock (bounded retries) and
        // must NOT have touched the live staging directory.
        let result = HnswIndex::load_with_dim(dir.path(), basename, crate::EMBEDDING_DIM);
        let err = format!(
            "{}",
            result.err().expect("load must fail under lock contention")
        );
        assert!(
            err.contains("lock contended"),
            "load should fail on lock contention, got: {}",
            err
        );
        assert!(
            staging.exists(),
            "live staging dir must survive a load that never got the shared lock"
        );
    }

    /// List the generation numbers with at least one artifact on disk.
    fn generations_on_disk(dir: &Path, basename: &str) -> Vec<u64> {
        let mut gens = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(g) = parse_generation(&name, basename) {
                gens.insert(g);
            }
        }
        gens.into_iter().collect()
    }

    /// Headline crash-state (a): a crash in the window between artifact
    /// completion and the manifest commit leaves the PRIOR generation intact
    /// and loadable. The injected fault writes generation 2's artifacts, then
    /// dies before flipping the pointer; the loader still serves generation 1.
    #[test]
    fn crash_before_manifest_commit_serves_old_generation() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        // Good v1 index: manifest points at generation 1.
        let v1: Vec<(String, crate::embedder::Embedding)> = (1..=2)
            .map(|i| (format!("vec{}", i), make_embedding(i)))
            .collect();
        HnswIndex::build_with_dim(v1, crate::EMBEDDING_DIM)
            .unwrap()
            .save(tmp.path(), basename)
            .unwrap();

        // v2 save crashes just before the manifest commit.
        let v2: Vec<(String, crate::embedder::Embedding)> = (1..=3)
            .map(|i| (format!("newvec{}", i), make_embedding(i + 10)))
            .collect();
        let v2_idx = HnswIndex::build_with_dim(v2, crate::EMBEDDING_DIM).unwrap();
        test_hooks::FAIL_BEFORE_MANIFEST_COMMIT.with(|h| *h.borrow_mut() = true);
        let result = v2_idx.save(tmp.path(), basename);
        test_hooks::FAIL_BEFORE_MANIFEST_COMMIT.with(|h| *h.borrow_mut() = false);
        assert!(
            result.is_err(),
            "the injected crash must surface as an error"
        );

        // The manifest still points at generation 1, whose artifacts are
        // intact — the loader serves the OLD generation, not a half-written v2.
        let loaded = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
            .expect("prior generation must remain loadable after a mid-save crash");
        assert_eq!(
            loaded.len(),
            2,
            "the served index must be v1 (generation 1)"
        );
        assert!(
            generations_on_disk(tmp.path(), basename).contains(&2),
            "the crashed save's generation-2 artifacts should be present as orphans"
        );
    }

    /// Headline crash-state (b) — the availability fix: after a crash state, a
    /// subsequent save SUCCEEDS. This is RED against the old stale-`.bak`
    /// guard, which refused every save after a partial one ("manual recovery
    /// required"). The next save also GCs the orphaned generation.
    #[test]
    fn save_succeeds_after_crash_before_manifest_commit() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        HnswIndex::build_with_dim(
            (1..=2)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();

        // Crash a v2 save before the commit.
        let crashed = HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("mid{}", i), make_embedding(i + 20)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        test_hooks::FAIL_BEFORE_MANIFEST_COMMIT.with(|h| *h.borrow_mut() = true);
        let _ = crashed.save(tmp.path(), basename);
        test_hooks::FAIL_BEFORE_MANIFEST_COMMIT.with(|h| *h.borrow_mut() = false);

        // The subsequent save must SUCCEED (no wedge) and become current.
        let v3 = HnswIndex::build_with_dim(
            (1..=4)
                .map(|i| (format!("final{}", i), make_embedding(i + 40)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        v3.save(tmp.path(), basename)
            .expect("a save after a crashed save must succeed — the wedge is gone");
        let loaded = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).unwrap();
        assert_eq!(loaded.len(), 4, "the current index must be the v3 save");
        assert_eq!(
            generations_on_disk(tmp.path(), basename).len(),
            1,
            "post-commit GC must leave exactly the current generation"
        );
    }

    /// Crash-state (c): a committed manifest with a not-yet-GC'd predecessor
    /// generation loads the NEW generation, and the next save GCs the stragglers.
    #[test]
    fn committed_with_stale_generations_loads_new_then_next_save_gcs() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        HnswIndex::build_with_dim(
            (1..=2)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();

        // Fabricate a not-GC'd predecessor generation 0 by copying gen 1's
        // artifacts under gen-0 names.
        let g1 = gen_basename(basename, 1);
        let g0 = gen_basename(basename, 0);
        for ext in HNSW_ARTIFACT_EXTS {
            std::fs::copy(
                tmp.path().join(format!("{g1}.{ext}")),
                tmp.path().join(format!("{g0}.{ext}")),
            )
            .unwrap();
        }
        assert!(generations_on_disk(tmp.path(), basename).contains(&0));

        // Load still serves the current generation (1).
        assert_eq!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
                .unwrap()
                .len(),
            2
        );

        // The next save GCs every non-current generation.
        HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("next{}", i), make_embedding(i + 5)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();
        assert_eq!(
            generations_on_disk(tmp.path(), basename).len(),
            1,
            "the next save must GC the stale generations, leaving only the current"
        );
    }

    /// Crash-state (d): a manifest pointing at a generation whose artifacts are
    /// missing (or corrupt) fails to load — routed to rebuild, never a wedge —
    /// and a subsequent save recovers cleanly.
    #[test]
    fn manifest_referencing_missing_artifact_falls_to_rebuild() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        HnswIndex::build_with_dim(
            (1..=2)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();

        // Remove the current generation's graph artifact, leaving the manifest.
        let paths = HnswIndex::artifact_paths(tmp.path(), basename).unwrap();
        std::fs::remove_file(&paths.graph).unwrap();

        // load errors; try_load routes to brute-force (None), never a wedge.
        assert!(HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).is_err());
        assert!(HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM).is_none());

        // A subsequent save recovers.
        let v2 = HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("recovered{}", i), make_embedding(i + 7)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        v2.save(tmp.path(), basename).unwrap();
        assert_eq!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn test_save_and_load() {
        // This test pins the save → exists → load roundtrip. The recall check
        // (chunk1 reachable post-load) goes through the shared
        // `assert_self_match_reachable` helper, which retries the build to
        // absorb the rare degenerate concurrent-build graph (see the helper
        // docs in hnsw/mod.rs). The roundtrip itself (exists/len) is asserted
        // unconditionally on every build inside the closure.
        let build = || {
            let tmp = TempDir::new().unwrap();
            let embeddings = vec![
                ("chunk1".to_string(), make_embedding(1)),
                ("chunk2".to_string(), make_embedding(2)),
            ];
            let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
            index.save(tmp.path(), "index").unwrap();
            assert!(HnswIndex::exists(tmp.path(), "index"));
            let loaded =
                HnswIndex::load_with_dim(tmp.path(), "index", crate::EMBEDDING_DIM).unwrap();
            assert_eq!(loaded.len(), 2);
            // TempDir drops here; the loaded index is self-contained.
            loaded
        };
        crate::hnsw::assert_self_match_reachable(build, &make_embedding(1), "chunk1", 2);
    }

    // ===== multi-model dim-threading (HNSW persist) =====

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
        // Save a 1024-dim HNSW index, load with load_with_dim(1024),
        // verify it loads and searches correctly.
        //
        // The save/load roundtrip (dim + len) is asserted on every build
        // inside the closure. The recall check (vec1 reachable post-load)
        // goes through the shared `assert_self_match_reachable` helper, which
        // retries the build to absorb the rare degenerate concurrent-build
        // graph (see the helper docs in hnsw/mod.rs): the five sin-derived
        // vectors are near-parallel (adjacent seeds ~0.995 cosine), making
        // them especially exposed to it.
        let basename = "test_1024";
        let build = || {
            let tmp = TempDir::new().unwrap();
            let embeddings: Vec<(String, crate::embedder::Embedding)> = (1..=5)
                .map(|i| (format!("vec{}", i), make_embedding_dim(i, 1024)))
                .collect();
            let index = HnswIndex::build_with_dim(embeddings, 1024).unwrap();
            assert_eq!(index.dim, 1024);
            index.save(tmp.path(), basename).unwrap();

            let loaded = HnswIndex::load_with_dim(tmp.path(), basename, 1024).unwrap();
            assert_eq!(loaded.len(), 5, "Loaded index should have 5 vectors");
            assert_eq!(loaded.dim, 1024, "Loaded index dim should be 1024");
            // TempDir drops here; the loaded index is self-contained.
            loaded
        };
        crate::hnsw::assert_self_match_reachable(build, &make_embedding_dim(1, 1024), "vec1", 3);
    }

    #[test]
    fn tc31_load_with_wrong_dim_data_size_rejected() {
        // Build with dim=1024, try to load with a much smaller dim.
        // The size check: expected_max_data = id_map.len() * dim * sizeof(f32) * 2
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

    /// `try_load_base_with_ef` returns `None` when the index_base files
    /// don't exist. The caller treats this as "fall back to enriched index".
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

    /// `try_load_base_with_ef` succeeds when index_base files exist.
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

    /// A successful save leaves a clean generation layout: exactly the current
    /// generation's three artifacts plus the manifest — no `.bak` debris and no
    /// legacy fixed-name sidecars.
    #[test]
    fn test_save_leaves_clean_generation_layout() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();
        // A second save supersedes the first; the layout must stay clean.
        HnswIndex::build_with_dim(
            (1..=5)
                .map(|i| (format!("vec{}", i + 10), make_embedding(i + 10)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();

        assert_eq!(
            generations_on_disk(tmp.path(), basename).len(),
            1,
            "exactly one generation should remain after a supersede"
        );
        assert!(
            manifest_path(tmp.path(), basename).exists(),
            "manifest must be present"
        );
        for ext in LEGACY_HNSW_EXTENSIONS {
            assert!(!tmp.path().join(format!("{basename}.{ext}")).exists());
            assert!(!tmp.path().join(format!("{basename}.{ext}.bak")).exists());
        }
    }

    /// The availability fix, positively pinned: leftover debris from the
    /// retired scheme (a `.bak`, a legacy sidecar) does NOT block a save. The
    /// old stale-`.bak` guard returned "manual recovery required" here; now the
    /// save proceeds and the post-commit GC cleans the debris. RED against the
    /// old code.
    #[test]
    fn test_save_succeeds_despite_stale_bak() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        std::fs::write(tmp.path().join("index.hnsw.graph.bak"), b"stale").unwrap();
        std::fs::write(tmp.path().join("index.hnsw.checksum"), b"stale").unwrap();

        let idx = HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        idx.save(tmp.path(), basename)
            .expect("a save must NOT be wedged by leftover .bak / legacy debris");
        assert!(!tmp.path().join("index.hnsw.graph.bak").exists());
        assert!(!tmp.path().join("index.hnsw.checksum").exists());
        assert_eq!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
                .unwrap()
                .len(),
            3
        );
    }

    /// Migration: a fabricated legacy five-file set (no manifest) reads as
    /// absent (NotFound → rebuild), and the first successful new-format save
    /// removes the legacy files under the lock.
    #[test]
    fn test_legacy_five_file_set_migrates_on_first_save() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        for ext in LEGACY_HNSW_EXTENSIONS {
            std::fs::write(tmp.path().join(format!("{basename}.{ext}")), b"legacy").unwrap();
        }
        assert!(HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).is_err());
        assert!(!HnswIndex::exists(tmp.path(), basename));

        HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();
        for ext in LEGACY_HNSW_EXTENSIONS {
            assert!(
                !tmp.path().join(format!("{basename}.{ext}")).exists(),
                "legacy {ext} must be removed on first new-format save"
            );
        }
        assert!(HnswIndex::exists(tmp.path(), basename));
        assert_eq!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
                .unwrap()
                .len(),
            3
        );
    }

    // ===== frozen legacy-state (pre-manifest) guards =====
    //
    // Every fixture built by the current code is born at the manifest-
    // generation layout; it can never construct the fixed-name sidecar set a
    // pre-manifest binary wrote (`{basename}.hnsw.{graph,data,ids,meta,
    // checksum}` with no `.hnsw.manifest`), nor the old staging directory, nor
    // the legacy `.hnsw.meta` header. The guards below hand-build those
    // old-world shapes faithfully and pin how the current reader/migrator
    // handles each: absent → rebuild, GC on the next save, never a misparse and
    // never a wedge. The single existing migration test only exercises the
    // COMPLETE five-file happy path (and with placeholder bytes); these cover
    // the partial-crash, mixed, staging-debris, legacy-header, and read-only
    // (reference-dir) shapes it does not.

    /// The faithful `.hnsw.meta` JSON a post-metric-header, pre-manifest binary
    /// wrote (magic `CQSHNSW`, version 1, a `metric` string, optional stamp).
    /// Reproduced by hand because the current code has no encoder for it.
    fn legacy_meta_json(metric: &str, stamp: Option<(u64, u64)>) -> Vec<u8> {
        match stamp {
            Some((cc, sg)) => format!(
                r#"{{"magic":"CQSHNSW","version":1,"metric":"{metric}","chunk_count":{cc},"splade_generation":{sg}}}"#
            )
            .into_bytes(),
            None => {
                format!(r#"{{"magic":"CQSHNSW","version":1,"metric":"{metric}"}}"#).into_bytes()
            }
        }
    }

    /// The faithful `.hnsw.checksum` manifest a pre-manifest binary wrote:
    /// `{ext}:{blake3hex}` lines over the three data files. Content is not
    /// load-bearing to the current reader (nothing opens this file anymore) —
    /// written for fixture fidelity so the shape on disk is the real one.
    fn legacy_checksum_text(dir: &Path, basename: &str) -> String {
        let mut lines = Vec::new();
        for ext in ["hnsw.graph", "hnsw.data", "hnsw.ids"] {
            let path = dir.join(format!("{basename}.{ext}"));
            if path.exists() {
                lines.push(format!("{ext}:{}", hash_test_file(&path)));
            }
        }
        lines.join("\n")
    }

    /// Write a faithful pre-manifest ("legacy") sidecar set for `basename`.
    /// `exts` selects which of the five fixed-name files exist (so partial-
    /// crash shapes are expressible); `meta_metric`, when `Some`, controls the
    /// `.hnsw.meta` header's metric string.
    fn write_legacy_set(dir: &Path, basename: &str, exts: &[&str], meta_metric: Option<&str>) {
        for ext in exts {
            let path = dir.join(format!("{basename}.{ext}"));
            match *ext {
                "hnsw.meta" => {
                    let metric = meta_metric.unwrap_or("cosine");
                    std::fs::write(&path, legacy_meta_json(metric, Some((3, 5)))).unwrap();
                }
                "hnsw.checksum" => { /* written last, after the data files exist */ }
                _ => std::fs::write(&path, b"legacy-hnsw-bytes").unwrap(),
            }
        }
        if exts.contains(&"hnsw.checksum") {
            std::fs::write(
                dir.join(format!("{basename}.hnsw.checksum")),
                legacy_checksum_text(dir, basename),
            )
            .unwrap();
        }
    }

    /// Partial legacy set — a crash mid old-scheme promote left graph+data+ids
    /// but no meta/checksum. Every current read path must treat it as absent
    /// (no manifest), routing to rebuild — never a partial-parse or a wedge.
    #[test]
    fn legacy_partial_set_reads_as_absent() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";
        write_legacy_set(
            tmp.path(),
            basename,
            &["hnsw.graph", "hnsw.data", "hnsw.ids"],
            None,
        );

        assert!(
            read_manifest(tmp.path(), basename).unwrap().is_none(),
            "a partial legacy set has no manifest"
        );
        assert!(!HnswIndex::exists(tmp.path(), basename));
        assert!(matches!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM),
            Err(HnswError::NotFound(_))
        ));
        assert!(HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM).is_none());
        assert!(
            HnswIndex::count_vectors(tmp.path(), basename).is_none(),
            "count_vectors must not read a legacy id-map without a manifest"
        );
    }

    /// A COMPLETE legacy set carrying a real `.hnsw.meta` header and a real
    /// `.hnsw.checksum` manifest must NOT be misparsed by any current lenient
    /// reader: `read_manifest` sees no manifest, `stored_metric` returns the
    /// no-manifest default (cosine) rather than tripping over the legacy meta,
    /// and `exists` is false. Pins that the retired sidecars are inert to the
    /// current readers, not accidentally consumed as current metadata.
    #[test]
    fn legacy_meta_and_checksum_are_not_read_as_current_metadata() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";
        write_legacy_set(tmp.path(), basename, LEGACY_HNSW_EXTENSIONS, Some("cosine"));

        assert!(
            read_manifest(tmp.path(), basename).unwrap().is_none(),
            "a legacy .hnsw.meta / .hnsw.checksum must not read as a .hnsw.manifest"
        );
        assert_eq!(
            HnswIndex::stored_metric(tmp.path(), basename).unwrap(),
            DistanceMetric::Cosine,
            "no manifest → cosine; the legacy meta header must not be parsed here"
        );
        assert!(!HnswIndex::exists(tmp.path(), basename));
    }

    /// The legacy-metric drop, pinned. A pre-manifest binary that built with
    /// `CQS_DISTANCE_METRIC=dot` wrote `.hnsw.meta` with `"metric":"dot"`, and
    /// the README promised "Stored in the index" (honored on load without the
    /// env var). The manifest reader does not read `.hnsw.meta`, so on upgrade
    /// the persisted `dot` is dropped: `stored_metric` returns cosine, and the
    /// index reads as absent → rebuilds with the env-resolved metric. This
    /// guard PINS that current behavior (the legacy dot header is inert). It is
    /// the observable half of a real migration lossiness — see the audit report
    /// (silent metric downgrade when un-normalized embeddings + a
    /// [metric-header, manifest) build window + an unset env var coincide).
    #[test]
    fn legacy_dot_metric_header_is_dropped_by_manifest_reader() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";
        write_legacy_set(tmp.path(), basename, LEGACY_HNSW_EXTENSIONS, Some("dot"));

        // The legacy header says dot; the current reader can't see it.
        assert!(read_manifest(tmp.path(), basename).unwrap().is_none());
        assert_eq!(
            HnswIndex::stored_metric(tmp.path(), basename).unwrap(),
            DistanceMetric::Cosine,
            "legacy .hnsw.meta metric=dot is silently dropped — stored_metric can only \
             report cosine because it reads the (absent) manifest, not the legacy header"
        );
        assert!(!HnswIndex::exists(tmp.path(), basename));
    }

    /// Mixed state: a valid new-format index (real build + manifest) with
    /// legacy fixed-name sidecars still on disk — the shape a crash between the
    /// manifest commit and the post-commit GC leaves. The load must serve the
    /// NEW generation (not the legacy bytes), and the next save's GC must sweep
    /// the legacy sidecars even though a manifest already exists.
    #[test]
    fn mixed_manifest_and_legacy_sidecars_load_new_gen_then_gc() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";

        HnswIndex::build_with_dim(
            (1..=4)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();
        // A committed manifest exists; drop legacy fixed-name debris beside it.
        write_legacy_set(tmp.path(), basename, LEGACY_HNSW_EXTENSIONS, Some("dot"));

        // The load resolves the manifest generation, not the legacy bytes.
        assert_eq!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
                .unwrap()
                .len(),
            4,
            "the manifest generation must win over coexisting legacy sidecars"
        );

        // The next save GCs the legacy sidecars under the lock.
        HnswIndex::build_with_dim(
            (1..=6)
                .map(|i| (format!("next{}", i), make_embedding(i + 9)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();
        for ext in LEGACY_HNSW_EXTENSIONS {
            assert!(
                !tmp.path().join(format!("{basename}.{ext}")).exists(),
                "GC must sweep legacy {ext} even when a manifest is already present"
            );
        }
        assert_eq!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
                .unwrap()
                .len(),
            6
        );
    }

    /// Old-scheme staging directory left by a crashed pre-manifest save:
    /// `.{basename}.<rand>.tmp/` holding the in-flight fixed-name files. Pins
    /// the ACTUAL production cleanup timing: `try_load_*` short-circuits on
    /// `exists()` for a legacy-only dir, so the staging dir survives the first
    /// (brute-force) query; once a real save writes a manifest, the next
    /// `try_load_*` reaches `load_with_dim`, whose stale-temp sweep removes it.
    #[test]
    fn legacy_staging_tmp_dir_cleaned_after_migration_not_before() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";
        // Faithful old staging dir name: `.{basename}.<16 hex>.tmp/` with the
        // in-flight fixed-name files inside.
        let staging = tmp.path().join(format!(".{basename}.0123456789abcdef.tmp"));
        std::fs::create_dir_all(&staging).unwrap();
        for ext in ["hnsw.graph", "hnsw.data", "hnsw.ids"] {
            std::fs::write(staging.join(format!("{basename}.{ext}")), b"staged").unwrap();
        }
        write_legacy_set(tmp.path(), basename, LEGACY_HNSW_EXTENSIONS, Some("cosine"));

        // Production entry on a legacy-only dir: brute-force fallback (None),
        // and the staging dir is NOT yet cleaned (load_with_dim never runs).
        assert!(HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM).is_none());
        assert!(
            staging.exists(),
            "a legacy-only dir short-circuits before load_with_dim, so the old \
             staging dir is not cleaned on the pre-migration query"
        );

        // Migrate: first new-format save.
        HnswIndex::build_with_dim(
            (1..=3)
                .map(|i| (format!("vec{}", i), make_embedding(i)))
                .collect(),
            crate::EMBEDDING_DIM,
        )
        .unwrap()
        .save(tmp.path(), basename)
        .unwrap();

        // Now exists() is true → try_load reaches load_with_dim → stale-temp
        // sweep removes the old staging dir.
        assert!(HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM).is_some());
        assert!(
            !staging.exists(),
            "load_with_dim's stale-temp sweep must remove the old staging dir \
             once the index is migrated"
        );
    }

    /// Reference-dir read-only shape: a reference points at a project whose
    /// `.cqs/` still carries a legacy set and is never re-indexed by the
    /// current binary. `try_load_with_ef` (the exact reference load call) must
    /// return None (brute-force fallback) WITHOUT mutating the read-only dir —
    /// no misparse, no crash, and the legacy files stay put (a read never GCs).
    #[test]
    fn legacy_reference_dir_loads_none_without_mutating() {
        let tmp = TempDir::new().unwrap();
        let basename = "index";
        write_legacy_set(tmp.path(), basename, LEGACY_HNSW_EXTENSIONS, Some("cosine"));

        assert!(
            HnswIndex::try_load_with_ef(tmp.path(), None, crate::EMBEDDING_DIM).is_none(),
            "a legacy reference index reads as absent → brute-force fallback"
        );
        // A read must not touch the on-disk legacy files (no GC on the read path).
        for ext in LEGACY_HNSW_EXTENSIONS {
            assert!(
                tmp.path().join(format!("{basename}.{ext}")).exists(),
                "read-only load must not delete legacy {ext} in a reference dir"
            );
        }
    }

    // ===== distance metric persistence =====

    /// Roundtrip: build with DotProduct → save → load → metric preserved
    /// and search still finds the self-match with a near-1 score (the
    /// embeddings are unit-norm, so self-dot ≈ 1 and `1 − dist` ≈ 1).
    /// Uses the shared retry helper so the rare degenerate concurrent-build
    /// graph can't flake the assertion; the metric roundtrip itself is
    /// asserted on every build inside the closure.
    #[test]
    fn test_metric_dot_save_load_roundtrip() {
        use crate::index::DistanceMetric;
        let basename = "test_dot_metric";
        let build = || {
            let tmp = TempDir::new().unwrap();
            let embeddings = vec![
                ("chunk1".to_string(), make_embedding(1)),
                ("chunk2".to_string(), make_embedding(2)),
            ];
            let index = HnswIndex::build_with_dim_and_metric(
                embeddings,
                crate::EMBEDDING_DIM,
                DistanceMetric::DotProduct,
            )
            .unwrap();
            assert_eq!(index.metric(), DistanceMetric::DotProduct);
            index.save(tmp.path(), basename).unwrap();

            // The manifest must be on disk (it carries the metric).
            assert!(
                manifest_path(tmp.path(), basename).exists(),
                "save must write the .hnsw.manifest pointer"
            );

            let loaded =
                HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).unwrap();
            assert_eq!(
                loaded.metric(),
                DistanceMetric::DotProduct,
                "metric must survive the save/load roundtrip"
            );
            assert_eq!(loaded.len(), 2);
            // TempDir drops here; the loaded index is self-contained.
            loaded
        };
        let score =
            crate::hnsw::assert_self_match_reachable(build, &make_embedding(1), "chunk1", 2);
        assert!(
            score > 0.9,
            "unit-norm self-match under DistDot should score ≈ 1, got {score}"
        );
    }

    /// With no manifest present, `stored_metric` defaults to cosine — every
    /// legacy index was built with `DistCosine`, and that keeps the CAGRA
    /// rebuild path consistent when `CQS_DISTANCE_METRIC` is unset.
    #[test]
    fn test_stored_metric_defaults_to_cosine_without_manifest() {
        use crate::index::DistanceMetric;
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            HnswIndex::stored_metric(tmp.path(), "absent").unwrap(),
            DistanceMetric::Cosine,
            "no manifest → cosine default"
        );
    }

    /// Loading with `CQS_DISTANCE_METRIC` explicitly set to a different
    /// metric than the stored one is a typed `MetricMismatch` error, never
    /// a silent reinterpretation.
    ///
    /// Test discipline: the env var is only ever set to "cosine" —
    /// a concurrent unlocked test loading a cosine index then still sees
    /// `requested == stored` and is unaffected. The conflict is created by
    /// the *stored* side (a DotProduct index built via the explicit-metric
    /// API). Held under `HNSW_ENV_LOCK` for the var's whole lifetime.
    #[test]
    fn test_metric_mismatch_is_typed_error() {
        use crate::hnsw::{EnvVarGuard, HNSW_ENV_LOCK};
        use crate::index::DistanceMetric;

        let tmp = TempDir::new().unwrap();
        let basename = "test_metric_mismatch";

        let _lock = HNSW_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let embeddings = vec![
            ("a".to_string(), make_embedding(1)),
            ("b".to_string(), make_embedding(2)),
        ];
        let index = HnswIndex::build_with_dim_and_metric(
            embeddings.clone(),
            crate::EMBEDDING_DIM,
            DistanceMetric::DotProduct,
        )
        .unwrap();
        index.save(tmp.path(), basename).unwrap();

        let _var = EnvVarGuard::set("CQS_DISTANCE_METRIC", "cosine");

        // Explicit conflicting request → typed error.
        match HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM) {
            Err(crate::hnsw::HnswError::MetricMismatch { stored, requested }) => {
                assert_eq!(stored, DistanceMetric::DotProduct);
                assert_eq!(requested, DistanceMetric::Cosine);
            }
            Err(other) => panic!("expected MetricMismatch, got: {other}"),
            Ok(_) => panic!("conflicting metric must not load"),
        }

        // Matching explicit request → loads fine. Same env value ("cosine"),
        // conflict-free because this index IS cosine.
        let cosine_index = HnswIndex::build_with_dim_and_metric(
            embeddings,
            crate::EMBEDDING_DIM,
            DistanceMetric::Cosine,
        )
        .unwrap();
        let basename2 = "test_metric_match";
        cosine_index.save(tmp.path(), basename2).unwrap();
        let loaded = HnswIndex::load_with_dim(tmp.path(), basename2, crate::EMBEDDING_DIM)
            .expect("matching explicit metric must load");
        assert_eq!(loaded.metric(), DistanceMetric::Cosine);
        // _var's Drop removes the env var before the lock releases.
    }

    /// A manifest whose `metric` is edited to disagree with the graph must not
    /// silently misread distances. The manifest is not self-checksummed (its
    /// integrity is the atomic commit), so the artifact checksums still pass;
    /// the guard is hnsw_rs's cross-check of the distance name embedded in the
    /// graph file, which makes the load fail loudly.
    #[test]
    fn test_tampered_manifest_metric_fails_load() {
        let tmp = TempDir::new().unwrap();
        let basename = "test_metric_tamper";

        // Build a cosine index, then rewrite the manifest to claim "dot"
        // (keeping generation + artifact checksums valid).
        let index = HnswIndex::build_with_dim(
            vec![
                ("a".to_string(), make_embedding(1)),
                ("b".to_string(), make_embedding(2)),
            ],
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        index.save(tmp.path(), basename).unwrap();

        let manifest = read_manifest(tmp.path(), basename).unwrap().unwrap();
        write_manifest(
            tmp.path(),
            basename,
            manifest.generation,
            "dot",
            manifest.store_stamp(),
        );

        // The metric now says dot, the graph says cosine → hnsw_rs rejects it.
        assert!(
            HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM).is_err(),
            "a manifest metric that disagrees with the graph must not load"
        );
    }

    // ===== id_map edge cases =====
    //
    // Three id_map shapes exercise rare but reachable states after a corrupt
    // save, a user-crafted index, or a bincode deserialisation glitch:
    //
    // * duplicate string entries — the id_map is not a set, so duplicates
    //   are accepted. Pins that behaviour so a future "dedup on load"
    //   refactor is deliberate.
    // * empty string entries — the store uses non-empty chunk IDs, but the
    //   loader has no minimum-length check, so `""` in the id_map is
    //   accepted and eventually returned by `search()` as a bogus ID.
    // * NUL-byte string entries — JSON preserves NUL (` `), serde_json
    //   decodes it to a real NUL in the `String`. Pins that the loader
    //   accepts such ids today.

    /// Build a valid on-disk HNSW index, then rewrite the current generation's
    /// `.ids` file with a crafted id_map while keeping graph + data intact. The
    /// manifest is re-pinned over the mutated artifacts, or `verify_hnsw_checksums`
    /// would reject the index before the id_map is touched.
    fn rewrite_id_map_and_manifest(dir: &Path, basename: &str, ids: &[&str]) {
        let manifest = read_manifest(dir, basename).unwrap().unwrap();
        let gb = gen_basename(basename, manifest.generation);
        let id_map_path = dir.join(format!("{gb}.hnsw.ids"));
        std::fs::write(&id_map_path, serde_json::to_string(ids).unwrap()).unwrap();
        write_manifest(
            dir,
            basename,
            manifest.generation,
            &manifest.metric,
            manifest.store_stamp(),
        );
    }

    /// Duplicate id_map entries are NOT rejected. `load_with_dim` only
    /// asserts `id_map.len() == hnsw_count`, not that the entries are
    /// unique. This test pins the current behaviour — a duplicate id_map
    /// survives load and is returned from search as-is.
    #[test]
    fn test_load_accepts_duplicate_id_map_entries() {
        let tmp = TempDir::new().unwrap();
        let basename = "test_dup";

        // Build an index with 3 unique vectors, so hnsw_count == 3.
        let embeddings = vec![
            ("a".to_string(), make_embedding(1)),
            ("b".to_string(), make_embedding(2)),
            ("c".to_string(), make_embedding(3)),
        ];
        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), basename).unwrap();

        // Rewrite id_map so two slots point at the same chunk id. The
        // hnsw_count still matches 3, so the mismatch check doesn't fire.
        rewrite_id_map_and_manifest(tmp.path(), basename, &["a", "a", "c"]);

        let loaded = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
            .expect("duplicate id_map entries must not cause load failure");
        assert_eq!(loaded.len(), 3);
        // AUDIT-FOLLOWUP: if a future dedup/validation pass rejects
        // duplicates, update this assertion accordingly.
    }

    /// Empty-string id_map entries are accepted. The id_map carries
    /// `Vec<String>` and there is no "must be non-empty" check. Search
    /// eventually returns the bogus empty id to the caller.
    #[test]
    fn test_load_accepts_empty_string_id_map_entries() {
        let tmp = TempDir::new().unwrap();
        let basename = "test_empty_id";

        let embeddings = vec![
            ("a".to_string(), make_embedding(1)),
            ("b".to_string(), make_embedding(2)),
        ];
        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), basename).unwrap();

        rewrite_id_map_and_manifest(tmp.path(), basename, &["", "b"]);

        let loaded = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
            .expect("empty-string id_map entries must not cause load failure");
        assert_eq!(loaded.len(), 2);
        // AUDIT-FOLLOWUP: once a non-empty-string guard lands, change to
        // `assert!(result.is_err())` with the rejection message.
    }

    /// NUL-byte id_map entries are preserved verbatim. JSON encodes NUL as
    /// ` ` and serde_json decodes it to a real NUL in `String`. The
    /// loader has no sanitation pass.
    #[test]
    fn test_load_accepts_nul_byte_id_map_entries() {
        let tmp = TempDir::new().unwrap();
        let basename = "test_nul_id";

        let embeddings = vec![
            ("a".to_string(), make_embedding(1)),
            ("b".to_string(), make_embedding(2)),
        ];
        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        index.save(tmp.path(), basename).unwrap();

        // A chunk id containing a NUL byte. Write via serde_json::to_string so
        // the NUL gets its ` ` escape — we can't hand-hardcode a
        // JSON-with-real-NUL byte sequence into the file.
        let nul_id = String::from("has\0nul");
        let normal_id = String::from("b");
        let ids: Vec<&str> = vec![nul_id.as_str(), normal_id.as_str()];
        rewrite_id_map_and_manifest(tmp.path(), basename, &ids);

        let loaded = HnswIndex::load_with_dim(tmp.path(), basename, crate::EMBEDDING_DIM)
            .expect("NUL-byte id_map entries must not cause load failure");
        assert_eq!(loaded.len(), 2);
        // AUDIT-FOLLOWUP: accepting NUL in chunk ids is a downstream hazard
        // (SQL queries, log lines). Once a reject-NUL guard lands, flip this
        // to `result.is_err()`.
    }

    // ===== store-state stamp: save_stamped / verify_hnsw_current =====

    /// Build a tiny saveable index for the stamp tests.
    fn small_index() -> HnswIndex {
        HnswIndex::build_with_dim(
            vec![
                ("chunk1".to_string(), make_embedding(1)),
                ("chunk2".to_string(), make_embedding(2)),
            ],
            crate::EMBEDDING_DIM,
        )
        .unwrap()
    }

    /// Advance the store's state stamp by inserting a chunk: `chunk_count`
    /// changes, so any earlier `StoreStamp` no longer matches.
    fn advance_store(store: &crate::Store, id: &str) {
        use crate::parser::{Chunk, ChunkType, Language};
        let content = format!("fn {}() {{ }}", id);
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: id.to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: id.to_string(),
            signature: format!("fn {}()", id),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            byte_start: 0,
            content_hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunks_batch(&[(chunk, make_embedding(42))], Some(0))
            .expect("seed chunk");
    }

    /// Dirty-path contract: a stamped save whose stamp still matches the
    /// live store verifies (died after the save → heal); once the store
    /// moves past the stamp, verification refuses (died after the chunk
    /// commit, before the save → stay dirty / brute-force).
    #[test]
    fn verify_current_distinguishes_save_landed_from_save_missed() {
        let (store, tmp) = crate::test_helpers::setup_store();
        let index = small_index();
        let snapshot = StoreStamp::read(&store).unwrap();
        let outcome = index
            .save_stamped(tmp.path(), "index", &store, snapshot)
            .unwrap();
        assert_eq!(outcome, SaveOutcome::Saved);

        verify_hnsw_current(tmp.path(), "index", &store)
            .expect("stamp matches the live store — the dirty flag was a false positive");

        advance_store(&store, "late_chunk");
        let err = verify_hnsw_current(tmp.path(), "index", &store)
            .expect_err("store moved past the sidecar stamp — must refuse");
        assert!(
            format!("{err}").contains("does not match the live store"),
            "want stamp-mismatch error, got: {err}"
        );
    }

    /// Enrichment-style false-heal scenario: an embedding-only rewrite
    /// (`update_embeddings_with_hashes_batch`) changes no chunk rows and no
    /// sparse vectors — `chunk_count` is unchanged — so the stamp moves only
    /// because the update bumps the write generation. A crash between that
    /// commit and the HNSW save (dirty flag set, sidecars from before the
    /// rewrite) must refuse the self-heal; healing would serve the
    /// pre-enrichment vectors as current.
    #[test]
    fn verify_current_refuses_after_embedding_only_update() {
        let (store, tmp) = crate::test_helpers::setup_store();
        // Seed a chunk first so the sidecar stamp includes it — the later
        // embedding rewrite must be the ONLY thing that moves the stamp.
        advance_store(&store, "enriched_chunk");
        let chunks_at_save = store.chunk_count().unwrap();

        let index = small_index();
        let snapshot = StoreStamp::read(&store).unwrap();
        assert_eq!(
            index
                .save_stamped(tmp.path(), "index", &store, snapshot)
                .unwrap(),
            SaveOutcome::Saved
        );
        verify_hnsw_current(tmp.path(), "index", &store).expect("precondition: stamp matches");

        // The enrichment pass: rewrite the chunk's embedding in place.
        store
            .update_embeddings_with_hashes_batch(&[(
                "enriched_chunk".to_string(),
                make_embedding(77),
                Some("enrichment_hash".to_string()),
            )])
            .unwrap();
        assert_eq!(
            store.chunk_count().unwrap(),
            chunks_at_save,
            "fixture invariant: the rewrite must not change chunk_count — \
             only the write generation may move"
        );

        let err = verify_hnsw_current(tmp.path(), "index", &store)
            .expect_err("sidecars predate the enrichment rewrite — must refuse");
        assert!(
            format!("{err}").contains("does not match the live store"),
            "want stamp-mismatch error, got: {err}"
        );
    }

    /// A sidecar set without a stamp (plain `save()`, or pre-stamp binaries)
    /// cannot prove it postdates the last chunk write — the dirty path must
    /// refuse it even though checksums pass.
    #[test]
    fn verify_current_refuses_unstamped_sidecars() {
        let (store, tmp) = crate::test_helpers::setup_store();
        small_index().save(tmp.path(), "index").unwrap();

        let err = verify_hnsw_current(tmp.path(), "index", &store)
            .expect_err("unstamped sidecars must refuse on the dirty path");
        assert!(
            format!("{err}").contains("no store-state stamp"),
            "want missing-stamp error, got: {err}"
        );
    }

    /// The stale-rebuild seam: store generation bumps between build-snapshot
    /// capture and save → the save is discarded under the lock and nothing
    /// lands on disk.
    #[test]
    fn save_stamped_discards_when_store_moves_past_snapshot() {
        let (store, tmp) = crate::test_helpers::setup_store();
        let index = small_index();
        let snapshot = StoreStamp::read(&store).unwrap();

        // The unobserved writer: commits a chunk after the snapshot was
        // captured but before the save runs.
        advance_store(&store, "raced_in");

        let outcome = index
            .save_stamped(tmp.path(), "index", &store, snapshot)
            .unwrap();
        assert_eq!(
            outcome,
            SaveOutcome::DiscardedStale,
            "a save built from a superseded snapshot must be discarded"
        );
        assert!(
            !HnswIndex::exists(tmp.path(), "index"),
            "a discarded save must not write a manifest"
        );
    }

    /// The manifest roundtrips the stamp for a stamped save, and a plain
    /// (store-less) `save()` writes a stamp-less manifest whose
    /// `store_stamp()` is `None` (so the dirty path refuses it).
    #[test]
    fn manifest_stamp_roundtrip_and_unstamped() {
        let (store, tmp) = crate::test_helpers::setup_store();
        let snapshot = StoreStamp::read(&store).unwrap();
        small_index()
            .save_stamped(tmp.path(), "index", &store, snapshot)
            .unwrap();
        let manifest = read_manifest(tmp.path(), "index")
            .unwrap()
            .expect("manifest written");
        assert_eq!(manifest.store_stamp(), Some(snapshot));

        // A store-less plain save writes no stamp.
        let tmp2 = TempDir::new().unwrap();
        small_index().save(tmp2.path(), "index").unwrap();
        let unstamped = read_manifest(tmp2.path(), "index")
            .unwrap()
            .expect("manifest written");
        assert_eq!(unstamped.store_stamp(), None);
        assert_eq!(
            read_manifest_metric(tmp2.path(), "index").unwrap(),
            DistanceMetric::Cosine
        );
    }

    // ===== property tests: HnswManifest save→load codec + the stamp/version
    // contracts =====
    //
    // The example tests above pin hand-picked values: one fixed stamp from
    // `setup_store()`, the (Some,Some) and (None,None) presence shapes, the
    // `dot`/`cosine` metrics, `version == 1`. The structural null is the rest
    // of the field space — boundary stamp values (0, 1, u64::MAX), the two
    // *half-present* presence shapes (the hand-edited-manifest case the
    // `store_stamp()` doc calls out), version boundaries around
    // `HNSW_MANIFEST_VERSION` (including 0 and u32::MAX), and arbitrary metric /
    // magic strings. The properties below drive the REAL functions
    // (`serde_json` serialize == the save path's encoder; `read_manifest` /
    // `read_manifest_metric` == the load path's decoder + validators) over a
    // generator that lands deliberately on those boundaries.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Serialize a typed `HnswManifest` exactly as `save_impl` does
        /// (`serde_json::to_vec`) and write it to the basename's manifest path.
        fn write_manifest_json(dir: &Path, basename: &str, manifest: &HnswManifest) {
            let bytes = serde_json::to_vec(manifest).expect("HnswManifest serializes");
            std::fs::write(manifest_path(dir, basename), bytes).expect("write manifest");
        }

        /// Generator coverage claim: every (chunk_count, splade_generation)
        /// presence pairing — (None,None), (Some,None), (None,Some),
        /// (Some,Some) — at stamp values drawn from the boundary set
        /// {0, 1, u64::MAX} unioned with a uniform u64. Distrust: proptest
        /// weights the explicit boundary set so the two half-present shapes
        /// (which `store_stamp()` must collapse to `None`) and the u64::MAX
        /// edge are not left to uniform luck.
        fn stamp_field() -> impl Strategy<Value = Option<u64>> {
            prop_oneof![
                Just(None),
                prop_oneof![Just(0u64), Just(1u64), Just(u64::MAX), any::<u64>(),].prop_map(Some),
            ]
        }

        /// Generator coverage claim: `version` lands on the validation
        /// boundary deliberately — 0 (below current), HNSW_MANIFEST_VERSION
        /// (the accept/reject seam), HNSW_MANIFEST_VERSION+1 (first rejected),
        /// u32::MAX (max) — plus a uniform u32 so no in-between value is
        /// unreachable.
        fn version_field() -> impl Strategy<Value = u32> {
            prop_oneof![
                Just(0u32),
                Just(HNSW_MANIFEST_VERSION),
                Just(HNSW_MANIFEST_VERSION.saturating_add(1)),
                Just(u32::MAX),
                any::<u32>(),
            ]
        }

        /// Generator coverage claim: `magic` is the correct constant about
        /// half the time and arbitrary unicode (incl. the empty string and
        /// near-misses) otherwise, so the magic-gate's accept and reject arms
        /// both get hit.
        fn magic_field() -> impl Strategy<Value = String> {
            prop_oneof![
                Just(HNSW_MANIFEST_MAGIC.to_string()),
                Just(String::new()),
                Just("cqshnswmanifest".to_string()), // case near-miss
                Just("CQSHNSWMANIFEST ".to_string()), // trailing-space near-miss
                ".*",                                // arbitrary, incl. unicode
            ]
        }

        /// Generator coverage claim: `metric` covers every accepted spelling
        /// `FromStr` recognizes (so the metric round-trips through
        /// `read_manifest_metric`), the canonical persisted spellings, and
        /// arbitrary garbage (so the typed-error arm is exercised).
        fn metric_field() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("cosine".to_string()),
                Just("dot".to_string()),
                Just("dotproduct".to_string()),
                Just("inner_product".to_string()),
                Just("ip".to_string()),
                Just("  Cosine  ".to_string()), // whitespace + case (FromStr trims/lowercases)
                ".*",                           // arbitrary, incl. unicode garbage
            ]
        }

        /// A fully-arbitrary, always-well-formed `HnswManifest` (correct magic
        /// + supported version + parseable metric). Used by the round-trip and
        /// stamp properties where the validators must ACCEPT every input, so
        /// the generator stays inside the valid region but still sweeps the
        /// full stamp boundary set. Checksums are irrelevant to these
        /// decode/validate properties, so they stay empty.
        fn valid_manifest() -> impl Strategy<Value = HnswManifest> {
            (
                prop_oneof![Just("cosine".to_string()), Just("dot".to_string())],
                0u32..=HNSW_MANIFEST_VERSION, // supported versions only
                any::<u64>(),                 // generation
                stamp_field(),
                stamp_field(),
            )
                .prop_map(
                    |(metric, version, generation, chunk_count, splade_generation)| HnswManifest {
                        magic: HNSW_MANIFEST_MAGIC.to_string(),
                        version,
                        generation,
                        metric,
                        chunk_count,
                        splade_generation,
                        checksums: ManifestChecksums::default(),
                    },
                )
        }

        proptest! {
            // Determinism: proptest's default RNG is seeded and a falsifier is
            // pinned in `.proptest-regressions`; we additionally pin the
            // boundary cases below as explicit `#[test]`s.

            /// Invariant (round-trip / codec):
            ///   read(write(manifest)) ≡ manifest   ∀ well-formed manifest.
            /// The save path encodes with `serde_json::to_vec`; the load path
            /// decodes with `serde_json::from_slice` inside `read_manifest`.
            /// This drives both real functions and asserts byte-faithful field
            /// reconstruction — the `#[serde(default)] Option<u64>` fields are
            /// the bite risk (a `None` that decodes as `Some(0)`, or a stamp
            /// dropped, would silently break the staleness guard).
            #[test]
            fn prop_manifest_round_trips(manifest in valid_manifest()) {
                let tmp = TempDir::new().unwrap();
                write_manifest_json(tmp.path(), "rt", &manifest);
                let loaded = read_manifest(tmp.path(), "rt")
                    .expect("well-formed manifest must read")
                    .expect("manifest file exists");
                prop_assert_eq!(loaded, manifest);
            }

            /// Invariant (stamp pairing):
            ///   store_stamp() = Some{cc,sg}  iff  chunk_count=Some(cc) ∧ splade_generation=Some(sg)
            ///                 = None          otherwise (incl. both half-present shapes).
            /// Driven through the persisted form so the `#[serde(default)]`
            /// decode of half-present pairs is the actual subject. The example
            /// suite only covers (Some,Some) at one fixed stamp and (None,None).
            #[test]
            fn prop_store_stamp_full_pairing(
                cc in stamp_field(),
                sg in stamp_field(),
            ) {
                let manifest = HnswManifest {
                    magic: HNSW_MANIFEST_MAGIC.to_string(),
                    version: HNSW_MANIFEST_VERSION,
                    generation: 1,
                    metric: "cosine".to_string(),
                    chunk_count: cc,
                    splade_generation: sg,
                    checksums: ManifestChecksums::default(),
                };
                let tmp = TempDir::new().unwrap();
                write_manifest_json(tmp.path(), "sp", &manifest);
                let loaded = read_manifest(tmp.path(), "sp").unwrap().unwrap();
                let expected = match (cc, sg) {
                    (Some(chunk_count), Some(splade_generation)) => {
                        Some(StoreStamp { chunk_count, splade_generation })
                    }
                    _ => None,
                };
                prop_assert_eq!(loaded.store_stamp(), expected);
            }

            /// Invariant (version/magic gate — the generation-manifest seam):
            ///   read_manifest accepts  iff  magic == HNSW_MANIFEST_MAGIC ∧ version ≤ HNSW_MANIFEST_VERSION.
            /// A manifest from a *newer* binary (version > this one) must be
            /// REJECTED, never silently used as-current — that is exactly the
            /// manifest-from-a-future-version class. Both arms are generated.
            #[test]
            fn prop_version_magic_gate(
                magic in magic_field(),
                version in version_field(),
                metric in metric_field(),
                cc in stamp_field(),
                sg in stamp_field(),
            ) {
                let manifest = HnswManifest {
                    magic: magic.clone(),
                    version,
                    generation: 1,
                    metric,
                    chunk_count: cc,
                    splade_generation: sg,
                    checksums: ManifestChecksums::default(),
                };
                let tmp = TempDir::new().unwrap();
                write_manifest_json(tmp.path(), "vg", &manifest);
                let result = read_manifest(tmp.path(), "vg");
                let should_accept = magic == HNSW_MANIFEST_MAGIC && version <= HNSW_MANIFEST_VERSION;
                if should_accept {
                    prop_assert!(
                        result.is_ok() && result.as_ref().unwrap().is_some(),
                        "magic+version valid → must accept; got {:?}",
                        result.err()
                    );
                } else {
                    // A mismatched magic or future version is a typed error —
                    // never a silent Ok that the load path would treat as a
                    // current manifest.
                    prop_assert!(
                        result.is_err(),
                        "magic={:?} version={} should reject (magic_ok={}, ver_ok={})",
                        magic,
                        version,
                        magic == HNSW_MANIFEST_MAGIC,
                        version <= HNSW_MANIFEST_VERSION,
                    );
                }
            }

            /// Invariant (metric round-trip):
            ///   read_manifest_metric(write(manifest)) = manifest.metric.parse::<DistanceMetric>()
            /// whenever magic+version are valid. A parseable metric survives;
            /// an unparseable one is a typed error (never a silent cosine).
            #[test]
            fn prop_metric_round_trip(metric in metric_field()) {
                let manifest = HnswManifest {
                    magic: HNSW_MANIFEST_MAGIC.to_string(),
                    version: HNSW_MANIFEST_VERSION,
                    generation: 1,
                    metric: metric.clone(),
                    chunk_count: None,
                    splade_generation: None,
                    checksums: ManifestChecksums::default(),
                };
                let tmp = TempDir::new().unwrap();
                write_manifest_json(tmp.path(), "mr", &manifest);
                let got = read_manifest_metric(tmp.path(), "mr");
                match metric.parse::<DistanceMetric>() {
                    Ok(expected) => {
                        prop_assert_eq!(
                            got.expect("parseable metric must read"),
                            expected
                        );
                    }
                    Err(_) => {
                        prop_assert!(
                            got.is_err(),
                            "unparseable metric {:?} must be a typed error, not a silent cosine",
                            metric
                        );
                    }
                }
            }

            /// Invariant (generation-mismatch self-heal — the headline target):
            ///   verify_hnsw_current Ok  iff  the persisted full stamp == the live store stamp.
            /// For ANY (saved, live) stamp pair and ANY presence shape, the
            /// dirty-path check accepts ONLY when a complete stamp is present
            /// AND equals the live store. A persisted stamp from a *different*
            /// generation (saved ≠ live) must be refused — silently accepting
            /// it is the manifest-from-previous-generation bug. The example
            /// suite drives this with the store's *actual* generation; here we
            /// forge the persisted stamp directly so saved>live, saved<live,
            /// saved==live, and the half/empty shapes are all reachable.
            #[test]
            fn prop_verify_current_accepts_iff_stamp_equals_live(
                saved_cc in stamp_field(),
                saved_sg in stamp_field(),
            ) {
                let (store, tmp) = crate::test_helpers::setup_store();
                let live = StoreStamp::read(&store).unwrap();

                // Build+save a real tiny index (real artifacts + manifest), then
                // overwrite ONLY the manifest's stamp with the forged pair,
                // keeping the REAL checksums so `verify_hnsw_checksums` still
                // passes and the stamp comparison is the sole discriminator.
                small_index().save(tmp.path(), "index").unwrap();
                let real = read_manifest(tmp.path(), "index").unwrap().unwrap();
                let forged = HnswManifest {
                    magic: HNSW_MANIFEST_MAGIC.to_string(),
                    version: HNSW_MANIFEST_VERSION,
                    generation: real.generation,
                    metric: "cosine".to_string(),
                    chunk_count: saved_cc,
                    splade_generation: saved_sg,
                    checksums: real.checksums,
                };
                write_manifest_json(tmp.path(), "index", &forged);

                let result = verify_hnsw_current(tmp.path(), "index", &store);

                let saved_stamp = match (saved_cc, saved_sg) {
                    (Some(c), Some(g)) => Some(StoreStamp {
                        chunk_count: c,
                        splade_generation: g,
                    }),
                    _ => None,
                };
                let should_accept = saved_stamp == Some(live);
                if should_accept {
                    prop_assert!(
                        result.is_ok(),
                        "stamp equals live → must heal; got {:?}",
                        result.err()
                    );
                } else {
                    prop_assert!(
                        result.is_err(),
                        "saved stamp {:?} ≠ live {:?} (or absent) → must refuse, not silently heal",
                        saved_stamp,
                        live,
                    );
                }
            }
        }

        // ===== pinned boundary regressions =====
        //
        // The boundary cases the properties sweep, pinned as explicit examples
        // so a generator change can never silently stop covering them. These
        // are the values a hand-written suite would have to enumerate by hand;
        // they are kept here as the durable floor under the generated search.

        /// version == HNSW_MANIFEST_VERSION is the accept/reject seam: it must
        /// be accepted (the boundary is `>`, not `>=`).
        #[test]
        fn boundary_version_at_limit_accepts() {
            let tmp = TempDir::new().unwrap();
            let manifest = HnswManifest {
                magic: HNSW_MANIFEST_MAGIC.to_string(),
                version: HNSW_MANIFEST_VERSION,
                generation: 1,
                metric: "cosine".to_string(),
                chunk_count: Some(u64::MAX),
                splade_generation: Some(0),
                checksums: ManifestChecksums::default(),
            };
            write_manifest_json(tmp.path(), "b", &manifest);
            let loaded = read_manifest(tmp.path(), "b").unwrap().unwrap();
            assert_eq!(
                loaded.store_stamp(),
                Some(StoreStamp {
                    chunk_count: u64::MAX,
                    splade_generation: 0
                })
            );
        }

        /// version == HNSW_MANIFEST_VERSION + 1 is the first rejected value: a
        /// manifest written by a newer binary must be refused, not used.
        #[test]
        fn boundary_version_above_limit_rejects() {
            let tmp = TempDir::new().unwrap();
            let manifest = HnswManifest {
                magic: HNSW_MANIFEST_MAGIC.to_string(),
                version: HNSW_MANIFEST_VERSION + 1,
                generation: 1,
                metric: "cosine".to_string(),
                chunk_count: None,
                splade_generation: None,
                checksums: ManifestChecksums::default(),
            };
            write_manifest_json(tmp.path(), "b", &manifest);
            let err =
                read_manifest(tmp.path(), "b").expect_err("future-version manifest must reject");
            assert!(format!("{err}").contains("newer than this binary"));
        }

        /// Both half-present stamp shapes collapse to `None` — a hand-edited
        /// manifest carrying only one of the pair cannot prove a generation.
        #[test]
        fn boundary_half_present_stamp_is_none() {
            for (cc, sg) in [(Some(7u64), None), (None, Some(9u64))] {
                let tmp = TempDir::new().unwrap();
                let manifest = HnswManifest {
                    magic: HNSW_MANIFEST_MAGIC.to_string(),
                    version: HNSW_MANIFEST_VERSION,
                    generation: 1,
                    metric: "cosine".to_string(),
                    chunk_count: cc,
                    splade_generation: sg,
                    checksums: ManifestChecksums::default(),
                };
                write_manifest_json(tmp.path(), "b", &manifest);
                let loaded = read_manifest(tmp.path(), "b").unwrap().unwrap();
                assert_eq!(
                    loaded.store_stamp(),
                    None,
                    "half-present pair {cc:?}/{sg:?} must not yield a stamp"
                );
            }
        }
    }
}
