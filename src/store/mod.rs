// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! SQLite storage for chunks, embeddings, and call graph data.
//!
//! Provides sync methods that internally use tokio runtime to execute async sqlx operations.
//! This allows callers to use the Store synchronously while benefiting from sqlx's async features.
//!
//! ## Module Structure
//!
//! - `helpers` - Types and embedding conversion functions
//! - `chunks` - Chunk CRUD operations
//! - `notes` - Note CRUD and search
//! - `calls` - Call graph storage and queries
//! - `types` - Type dependency storage and queries
//! - `migrations` - Database schema migrations
//! - `metadata` - Metadata get/set and version validation
//! - `search` - FTS search, name search, RRF fusion

pub mod calls;
mod chunks;
mod metadata;
mod migrations;
mod notes;
mod search;
mod sparse;
mod types;

/// Helper types and embedding conversion functions.
/// This module is `pub(crate)` - external consumers should use the re-exported
/// types from `cqs::store` instead of accessing `cqs::store::helpers` directly.
pub(crate) mod helpers;

use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Serialize all write transactions across Store instances within a process.
///
/// SQLite WAL mode allows concurrent readers, but only one writer at a time.
/// `pool.begin()` issues `BEGIN DEFERRED` — two concurrent processes (e.g.,
/// `cqs watch` + `cqs index`) can both acquire deferred transactions and race
/// to upgrade to exclusive, causing SQLITE_BUSY (DS-5).
///
/// This mutex ensures at most one in-process write transaction is active at
/// any time. The guard is held alongside the sqlx `Transaction` and dropped
/// when the transaction commits/rolls back.
///
/// Note: this serializes writes within a single process only. Cross-process
/// serialization relies on SQLite's busy_timeout (5s) and the index lock file.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions, SqlitePool};
use tokio::runtime::Runtime;

// Re-export public types with documentation

/// Cross-project call graph context and types.
pub use calls::cross_project::{
    CrossProjectCallee, CrossProjectCaller, CrossProjectContext, NamedStore,
};

/// In-memory call graph (forward + reverse adjacency lists).
pub use helpers::CallGraph;

/// Information about a function caller (from call graph).
pub use helpers::CallerInfo;

/// Caller with call-site context for impact analysis.
pub use helpers::CallerWithContext;

/// Chunk identity for diff comparison (name, file, line, window info).
pub use helpers::ChunkIdentity;

/// Summary of an indexed code chunk (function, class, etc.).
pub use helpers::ChunkSummary;

/// Parent context for expanded search results (small-to-big retrieval).
pub use helpers::ParentContext;

/// Statistics about the index (chunk counts, languages, etc.).
pub use helpers::IndexStats;

/// Embedding model metadata.
pub use helpers::ModelInfo;

/// A note search result with similarity score.
pub use helpers::NoteSearchResult;

/// Statistics about indexed notes.
pub use helpers::NoteStats;

/// Summary of a note (text, sentiment, mentions).
pub use helpers::NoteSummary;

/// Filter and scoring options for search.
pub use helpers::SearchFilter;

/// A code chunk search result with similarity score.
pub use helpers::SearchResult;

/// A file in the index whose content has changed on disk.
pub use helpers::StaleFile;

/// Report of index freshness (stale + missing files).
pub use helpers::StaleReport;

/// Store operation errors.
pub use helpers::StoreError;

/// Unified search result (code chunk or note).
pub use helpers::UnifiedResult;

/// Current database schema version.
pub use helpers::CURRENT_SCHEMA_VERSION;

/// Which HNSW index a dirty-flag operation applies to (enriched vs base).
pub use metadata::HnswKind;

/// Name of the embedding model (compile-time default for BGE-large).
/// Runtime code should use `Store::stored_model_name()` or `ModelInfo::new()`.
/// This constant exists for callers outside the store (e.g. `doctor.rs`).
pub const MODEL_NAME: &str = crate::embedder::DEFAULT_MODEL_REPO;

/// Expected embedding dimensions (compile-time default for BGE-large).
/// Runtime code should use `Store::dim` instead. This constant exists for
/// callers outside the store that need a compile-time value.
pub const EXPECTED_DIMENSIONS: usize = crate::EMBEDDING_DIM;

/// Default name_boost weight for CLI search commands.
pub use helpers::DEFAULT_NAME_BOOST;

/// Score a chunk name against a query for definition search.
pub use helpers::score_name_match;

/// Score a pre-lowercased chunk name against a pre-lowercased query (loop-optimized variant).
pub use helpers::score_name_match_pre_lower;

/// Result of atomic GC prune (all 4 operations in one transaction).
pub use chunks::PruneAllResult;

/// Statistics about call graph entries (chunk-level calls table).
pub use calls::CallStats;

/// A dead function with confidence scoring.
pub use calls::DeadFunction;

/// Confidence level for dead code detection.
pub use calls::DeadConfidence;

/// Detailed function call statistics (function_calls table).
pub use calls::FunctionCallStats;

/// Statistics about type dependency edges (type_edges table).
pub use types::TypeEdgeStats;

/// In-memory type graph (forward + reverse adjacency lists).
pub use types::TypeGraph;

/// A type usage relationship from a chunk.
pub use types::TypeUsage;

/// Set RRF K override from config scoring overrides.
pub use search::set_rrf_k_from_config;

/// Defense-in-depth sanitization for FTS5 query strings.
/// Strips or escapes FTS5 special characters that could alter query semantics.
/// Applied after `normalize_for_fts()` as an extra safety layer — if `normalize_for_fts`
/// ever changes to allow characters through, this prevents FTS5 injection.
/// FTS5 special characters: `"`, `*`, `(`, `)`, `+`, `-`, `^`, `:`, `NEAR`
/// FTS5 boolean operators: `OR`, `AND`, `NOT` (case-sensitive in FTS5)
/// # Safety (injection)
/// This function independently strips all FTS5-significant characters including
/// double quotes. Safe for use in `format!`-constructed FTS5 queries even without
/// `normalize_for_fts()`. The double-pass pattern (`normalize_for_fts` then
/// `sanitize_fts_query`) is defense-in-depth — either layer alone prevents injection.
pub(crate) fn sanitize_fts_query(s: &str) -> String {
    // Single-pass: split on whitespace (no allocation), filter FTS5 boolean
    // operators, strip FTS5 special chars from each surviving word, write
    // directly into one output String — no intermediate allocation.
    let mut out = String::with_capacity(s.len());
    for word in s
        .split_whitespace()
        .filter(|w| !matches!(*w, "OR" | "AND" | "NOT" | "NEAR"))
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(
            word.chars().filter(|c| {
                !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':' | '{' | '}')
            }),
        );
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.to_string()
}

/// Typestate marker for a store opened in read-only mode.
///
/// A [`Store<ReadOnly>`] exposes only query methods. Write methods
/// (`upsert_*`, `set_*`, `delete_*`, `prune_*`, etc.) live exclusively
/// on `impl Store<ReadWrite>`, so the compiler refuses to call them on
/// a read-only store. This converts a class of runtime errors into
/// compile-time errors — see the closed-bug examples in GitHub #946.
#[derive(Debug, Clone, Copy)]
pub struct ReadOnly;

/// Typestate marker for a store opened in read-write mode.
///
/// A [`Store<ReadWrite>`] exposes the full surface: both query and
/// mutation methods. This is the default type parameter of `Store`, so
/// bare `Store` in legacy code is equivalent to `Store<ReadWrite>`.
#[derive(Debug, Clone, Copy)]
pub struct ReadWrite;

/// Thread-safe SQLite store for chunks and embeddings
/// Uses sqlx connection pooling for concurrent reads and WAL mode
/// for crash safety. All methods are synchronous but internally use
/// an async runtime to execute sqlx operations.
///
/// # Typestate
///
/// The `Mode` type parameter records whether the store was opened
/// read-only or read-write. Read methods live on `impl<Mode> Store<Mode>`
/// and are available to both. Write methods live on `impl Store<ReadWrite>`
/// only — the compiler refuses any attempt to call a mutating method on
/// a `Store<ReadOnly>` handle (GitHub #946). `Mode` defaults to
/// [`ReadWrite`] so bare `Store` keeps working for legacy call sites.
///
/// # Memory-mapped I/O
/// `open()` sets `PRAGMA mmap_size = 256MB` per connection with a 4-connection pool,
/// reserving up to 1GB of virtual address space. `open_readonly()` uses 64MB × 1.
/// This is intentional and benign on 64-bit systems (128TB virtual address space).
/// Mmap pages are demand-paged from the database file and evicted under memory
/// pressure — actual RSS reflects only accessed pages, not the mmap reservation.
/// # Example
/// ```no_run
/// use cqs::Store;
/// use std::path::Path;
/// let store = Store::open(Path::new(".cqs/index.db"))?;
/// let stats = store.stats()?;
/// println!("Indexed {} chunks", stats.total_chunks);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Store<Mode = ReadWrite> {
    pub(crate) pool: SqlitePool,
    pub(crate) rt: Runtime,
    /// Embedding dimension for this store (read from metadata on open, default `EMBEDDING_DIM`).
    pub(crate) dim: usize,
    /// Whether close() has already been called (skip WAL checkpoint in Drop)
    closed: AtomicBool,
    notes_summaries_cache: RwLock<Option<Arc<Vec<NoteSummary>>>>,
    /// PF-V1.25-4: cached `OwnedNoteBoostIndex` derived from `notes_summaries_cache`.
    /// Built lazily on first `cached_note_boost_index()` call, invalidated
    /// alongside the notes cache in `invalidate_notes_cache`.
    note_boost_cache: RwLock<Option<Arc<crate::search::scoring::OwnedNoteBoostIndex>>>,
    /// Cached call graph — populated on first access, valid until `clear_caches()`.
    /// `OnceLock` is write-once within a cache epoch. `clear_caches(&mut self)` swaps
    /// in a fresh OnceLock, which is safe because `&mut self` guarantees exclusive access.
    call_graph_cache: std::sync::OnceLock<std::sync::Arc<CallGraph>>,
    test_chunks_cache: std::sync::OnceLock<std::sync::Arc<Vec<ChunkSummary>>>,
    chunk_type_map_cache: std::sync::OnceLock<std::sync::Arc<ChunkTypeMap>>,
    /// Typestate marker — `ReadOnly` or `ReadWrite`. Zero-sized.
    _mode: PhantomData<Mode>,
}

/// Map from chunk ID to (ChunkType, Language) — used by HNSW traversal-time filtering.
pub type ChunkTypeMap =
    std::collections::HashMap<String, (crate::parser::ChunkType, crate::parser::Language)>;

/// Internal configuration for [`Store::open_with_config`].
/// Captures the five parameters that differ between read-write and read-only
/// opens so the shared connection/pool/validation logic lives in one place.
struct StoreOpenConfig {
    read_only: bool,
    use_current_thread: bool,
    max_connections: u32,
    mmap_size: String,
    cache_size: String,
    /// Pre-existing runtime to reuse. If `Some`, skips runtime creation
    /// (~15ms saving). If `None`, creates a new one per `use_current_thread`.
    runtime: Option<Runtime>,
}

/// Filesystem types where SQLite `mmap_size > 0` hurts performance.
///
/// On these backends, mmap either falls back to per-page I/O (9P, NFS, SMB)
/// or triggers synchronous flushes on unrelated writes. Setting `mmap_size=0`
/// forces SQLite to use regular `pread`/`pwrite`, which is uniformly faster
/// on these filesystems.
///
/// `drvfs` / `fuse.drvfs` cover legacy WSL1. `9p` is WSL2's DrvFS transport
/// to the Windows host (paths under `/mnt/c/`, `/mnt/d/`, etc.). `cifs`,
/// `smb3`, `smbfs` cover SMB shares. `nfs`, `nfs4` cover NFS mounts.
/// `ntfs`, `ntfs3`, `fuseblk` cover native-Linux NTFS access (ntfs-3g).
const SLOW_MMAP_FSTYPES: &[&str] = &[
    "9p",
    "cifs",
    "smb3",
    "smbfs",
    "drvfs",
    "fuse.drvfs",
    "nfs",
    "nfs4",
    "ntfs",
    "ntfs3",
    "fuseblk",
];

/// Read `CQS_MMAP_SIZE` env var, returning `Some(value)` if explicitly set
/// to a valid non-negative integer. Returns `None` if unset or unparseable,
/// so callers can distinguish "user-requested" from "fell back to default".
fn mmap_size_env_override() -> Option<String> {
    std::env::var("CQS_MMAP_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok().map(|n| n.to_string()))
}

/// Resolve the SQLite `mmap_size` PRAGMA value for a database at `db_path`.
///
/// Precedence (highest first):
/// 1. `CQS_MMAP_SIZE` env var — explicit user override always wins.
/// 2. Slow-FS auto-detection — if `db_path` is on 9P/NTFS/SMB/NFS, return `"0"`
///    (disables mmap) and log an info message.
/// 3. `default_bytes` — the mode-specific default (e.g. 256 MB pooled, 64 MB
///    read-only).
///
/// The env var is the raw byte count (e.g. `268435456` for 256 MB).
fn resolve_mmap_size(default_bytes: &str, db_path: &Path) -> String {
    if let Some(explicit) = mmap_size_env_override() {
        return explicit;
    }
    if is_slow_mmap_fs(db_path) {
        tracing::info!(
            path = %db_path.display(),
            "Slow FS detected (WSL/9P/NTFS/SMB/NFS), disabling SQLite mmap (set CQS_MMAP_SIZE to override)"
        );
        return "0".to_string();
    }
    default_bytes.to_string()
}

/// Return `true` if `path` lives on a filesystem where SQLite `mmap_size > 0`
/// degrades performance (WSL `/mnt/c/` 9P, NFS, SMB, NTFS-via-FUSE).
///
/// Implementation: on Unix, parses `/proc/self/mountinfo` and looks up the
/// fstype of the mount containing `path`. On non-Unix, returns `false` (mmap
/// behavior on Windows native / macOS APFS is fine with the default size).
///
/// Canonicalizes `path` first; if canonicalization fails (e.g., the DB file
/// doesn't exist yet on first open), tries the parent directory.
fn is_slow_mmap_fs(path: &Path) -> bool {
    #[cfg(unix)]
    {
        // Resolve to an absolute path. On fresh opens the DB file may not
        // exist yet (create_if_missing=true), so fall back to the parent dir.
        let canonical = dunce::canonicalize(path).or_else(|_| {
            path.parent().map_or_else(
                || {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no parent",
                    ))
                },
                dunce::canonicalize,
            )
        });
        let abs = match canonical {
            Ok(p) => p,
            Err(_) => return false,
        };
        let mountinfo = match std::fs::read_to_string("/proc/self/mountinfo") {
            Ok(c) => c,
            Err(_) => return false,
        };
        match fstype_for_path(&mountinfo, &abs) {
            Some(fstype) => SLOW_MMAP_FSTYPES.iter().any(|&s| s == fstype),
            None => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Parse `/proc/self/mountinfo` content and return the fstype of the mount
/// point that contains `abs_path`.
///
/// mountinfo format (per `proc(5)`):
/// ```text
///   36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext3 /dev/root rw,errors=continue
///   (1) (2) (3)   (4)   (5)      (6)    ...    -  (fstype)
/// ```
/// Field 5 is the mount point; after the `-` separator, the first field is
/// the fstype. Optional fields (field 7+) vary, so split on `-`.
///
/// Picks the longest mount point that is a path-prefix of `abs_path`
/// (so nested mounts like `/mnt/c/WINDOWS` inside `/mnt/c` win correctly).
fn fstype_for_path(mountinfo: &str, abs_path: &Path) -> Option<String> {
    let abs_str = abs_path.to_str()?;
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        // Split on the ` - ` separator between variable-length and
        // fixed fields. mountinfo guarantees the separator token is ` - `.
        let (left, right) = match line.split_once(" - ") {
            Some(parts) => parts,
            None => continue,
        };
        let left_fields: Vec<&str> = left.split_whitespace().collect();
        if left_fields.len() < 5 {
            continue;
        }
        let mount_point = left_fields[4];
        // mountinfo escapes space/tab/newline/backslash as \040, \011, \012, \134.
        // For our purposes we only need to match exact prefixes; unescape \040
        // since spaces in paths are the realistic case. Other escapes are rare
        // enough to skip.
        let mp = mount_point.replace("\\040", " ");
        if !path_starts_with(abs_str, &mp) {
            continue;
        }
        let right_fields: Vec<&str> = right.split_whitespace().collect();
        let fstype = match right_fields.first() {
            Some(t) => *t,
            None => continue,
        };
        let len = mp.len();
        if best.as_ref().is_none_or(|(blen, _)| len > *blen) {
            best = Some((len, fstype.to_string()));
        }
    }
    best.map(|(_, fs)| fs)
}

/// Check whether `path` begins with `prefix` at a path-component boundary.
///
/// A plain `str::starts_with` would match `/mnt/ca` against `/mnt/c` — wrong,
/// since those are sibling mounts. The prefix matches only if `path == prefix`
/// or `path` continues with `/` after the prefix. Also handles the special
/// case where `prefix == "/"` (root mount) — always a prefix.
fn path_starts_with(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    if !path.starts_with(prefix) {
        return false;
    }
    // Exact match or next char is a path separator.
    path.len() == prefix.len() || path.as_bytes()[prefix.len()] == b'/'
}

#[cfg(test)]
mod slow_fs_tests {
    use super::{fstype_for_path, path_starts_with, SLOW_MMAP_FSTYPES};
    use std::path::Path;

    /// Realistic mountinfo snippet captured on WSL2 (Debian 13, kernel 6.6).
    /// Rootfs is ext4, `/mnt/c` is 9P-over-drvfs, `/proc` and `/sys` are virtual.
    const WSL_MOUNTINFO: &str = "\
75 80 0:29 / /usr/lib/modules/6.6.87 rw,nosuid,nodev,noatime - overlay none rw,lowerdir=/modules
80 65 8:48 / / rw,relatime - ext4 /dev/sdd rw,discard,errors=remount-ro
111 80 0:22 / /sys rw,nosuid,nodev,noexec,noatime shared:15 - sysfs sysfs rw
112 80 0:59 / /proc rw,nosuid,nodev,noexec,noatime shared:16 - proc proc rw
135 80 0:73 / /mnt/c rw,noatime - 9p C:\\134 rw,aname=drvfs;path=C:\\
136 80 0:74 / /mnt/d rw,noatime - 9p D:\\134 rw,aname=drvfs;path=D:\\
";

    #[test]
    fn wsl_mnt_c_detected_as_9p() {
        let fs = fstype_for_path(
            WSL_MOUNTINFO,
            Path::new("/mnt/c/Projects/cqs/.cqs/index.db"),
        );
        assert_eq!(fs.as_deref(), Some("9p"));
        assert!(SLOW_MMAP_FSTYPES.contains(&"9p"));
    }

    #[test]
    fn wsl_home_detected_as_ext4_not_slow() {
        let fs = fstype_for_path(
            WSL_MOUNTINFO,
            Path::new("/home/user001/project/.cqs/index.db"),
        );
        assert_eq!(fs.as_deref(), Some("ext4"));
        assert!(!SLOW_MMAP_FSTYPES.contains(&"ext4"));
    }

    #[test]
    fn wsl_mnt_c_root_itself_matches() {
        // Path exactly at mount point (edge case).
        let fs = fstype_for_path(WSL_MOUNTINFO, Path::new("/mnt/c"));
        assert_eq!(fs.as_deref(), Some("9p"));
    }

    #[test]
    fn sibling_mount_not_matched() {
        // `/mnt/ca` must not match mount `/mnt/c`. Without component-boundary
        // checking, naive starts_with would incorrectly return "9p".
        let info = "135 80 0:73 / /mnt/c rw,noatime - 9p C:\\134 rw\n80 65 8:48 / / rw - ext4 /dev/sdd rw\n";
        let fs = fstype_for_path(info, Path::new("/mnt/ca/file"));
        // Falls through to root ext4, not the /mnt/c 9p.
        assert_eq!(fs.as_deref(), Some("ext4"));
    }

    #[test]
    fn longest_prefix_wins_for_nested_mounts() {
        // `/mnt/c/WINDOWS` nested inside `/mnt/c` — the nested mount fstype
        // should win even though both are prefixes. Uses an artificial "cifs"
        // for the nested mount to prove the longer match is picked.
        let info = "\
80 65 8:48 / / rw - ext4 /dev/sdd rw
135 80 0:73 / /mnt/c rw - 9p C:\\134 rw
200 135 0:99 / /mnt/c/WINDOWS rw - cifs //host/share rw
";
        let fs = fstype_for_path(info, Path::new("/mnt/c/WINDOWS/system32/a.db"));
        assert_eq!(fs.as_deref(), Some("cifs"));
    }

    #[test]
    fn handles_optional_fields_with_shared_tag() {
        // mountinfo may have optional fields (shared:N, master:M) between
        // mount opts and the `-` separator. Must still parse.
        let info = "\
76 80 0:32 / /some/mount rw,relatime shared:1 master:2 - 9p foo rw\n";
        let fs = fstype_for_path(info, Path::new("/some/mount/file"));
        assert_eq!(fs.as_deref(), Some("9p"));
    }

    #[test]
    fn empty_or_garbage_mountinfo_returns_none() {
        assert!(fstype_for_path("", Path::new("/any/path")).is_none());
        assert!(fstype_for_path("not a mountinfo line\n", Path::new("/any/path")).is_none());
        // Missing ` - ` separator.
        assert!(fstype_for_path("80 65 8:48 / / rw ext4 /dev/sdd rw\n", Path::new("/")).is_none());
    }

    #[test]
    fn native_linux_ext4_is_not_slow() {
        // No WSL in sight — plain ext4 at root.
        let info = "80 65 8:48 / / rw,relatime - ext4 /dev/sda1 rw\n";
        let fs = fstype_for_path(info, Path::new("/home/alice/project/.cqs/index.db"));
        assert_eq!(fs.as_deref(), Some("ext4"));
        assert!(!SLOW_MMAP_FSTYPES.contains(&"ext4"));
    }

    #[test]
    fn nfs_and_cifs_are_slow() {
        let info = "\
80 65 8:48 / / rw - ext4 /dev/sdd rw
90 80 0:50 / /net/nfs rw - nfs server:/export rw
91 80 0:51 / /net/smb rw - cifs //host/share rw
";
        assert_eq!(
            fstype_for_path(info, Path::new("/net/nfs/index.db")).as_deref(),
            Some("nfs")
        );
        assert_eq!(
            fstype_for_path(info, Path::new("/net/smb/index.db")).as_deref(),
            Some("cifs")
        );
        assert!(SLOW_MMAP_FSTYPES.contains(&"nfs"));
        assert!(SLOW_MMAP_FSTYPES.contains(&"cifs"));
    }

    #[test]
    fn path_starts_with_component_boundary() {
        assert!(path_starts_with("/mnt/c/foo", "/mnt/c"));
        assert!(path_starts_with("/mnt/c", "/mnt/c"));
        assert!(!path_starts_with("/mnt/ca", "/mnt/c"));
        assert!(!path_starts_with("/mnt", "/mnt/c"));
        // Root is always a prefix of any absolute path.
        assert!(path_starts_with("/anything", "/"));
        assert!(path_starts_with("/", "/"));
    }

    #[test]
    fn resolve_mmap_size_env_override_wins() {
        // Env explicit set → returns that value, ignores slow-fs detection.
        // Save/restore to stay neighbour-friendly with parallel tests that
        // may inspect CQS_MMAP_SIZE.
        let prev = std::env::var("CQS_MMAP_SIZE").ok();
        std::env::set_var("CQS_MMAP_SIZE", "12345");
        let got = super::resolve_mmap_size("268435456", Path::new("/mnt/c/any"));
        assert_eq!(got, "12345");
        match prev {
            Some(v) => std::env::set_var("CQS_MMAP_SIZE", v),
            None => std::env::remove_var("CQS_MMAP_SIZE"),
        }
    }
}

/// Read `CQS_SQLITE_CACHE_SIZE` env var, falling back to `default_kib`.
/// SQLite `cache_size` PRAGMA uses a negative kibibyte count (e.g. `-16384`
/// for 16 MB). The env var should be a signed integer in that same format —
/// negative means kibibytes, positive means a page count. Accepting only
/// i64 keeps parsing simple while letting tuners pick either convention.
fn cache_size_from_env(default_kib: &str) -> String {
    std::env::var("CQS_SQLITE_CACHE_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok().map(|n| n.to_string()))
        .unwrap_or_else(|| default_kib.to_string())
}

impl<Mode> Store<Mode> {
    /// Embedding dimension for vectors in this store.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Update the embedding dimension after init (fresh DB only).
    /// `Store::open` defaults to `EMBEDDING_DIM` when the metadata table doesn't
    /// exist yet. After `init()` writes the correct dim, call this to sync.
    pub fn set_dim(&mut self, dim: usize) {
        self.dim = dim;
    }
}

impl Store<ReadWrite> {
    /// Open an existing index with connection pooling
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let max_connections = std::env::var("CQS_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(4);
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: false,
                use_current_thread: false,
                max_connections,
                mmap_size: resolve_mmap_size("268435456", path), // 256MB default
                cache_size: cache_size_from_env("-16384"),       // 16MB
                runtime: None,
            },
        )
    }

    /// Open an existing index in read-only mode with single-threaded runtime
    /// but full memory. Uses `current_thread` tokio runtime (1 OS thread
    /// instead of 4) while keeping the full 256MB mmap and 16MB cache of
    /// `open()`. Ideal for read-only CLI commands on the primary project index
    /// where we need full search performance but don't need multi-threaded
    /// async.
    ///
    /// AD-1: Renamed from `open_light` to clarify semantics — this is a
    /// read-only pooled connection, not a "light" store.
    pub fn open_readonly_pooled(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: true,
                use_current_thread: true,
                max_connections: 1,
                mmap_size: resolve_mmap_size("268435456", path),
                cache_size: cache_size_from_env("-16384"),
                runtime: None,
            },
        )
    }

    /// Open an existing index in read-only mode with reduced resources.
    /// Uses minimal connection pool, smaller cache, and single-threaded runtime.
    /// Suitable for reference stores and background builds that only read data.
    pub fn open_readonly(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: true,
                use_current_thread: true,
                max_connections: 1,
                mmap_size: resolve_mmap_size("67108864", path), // 64MB default
                cache_size: cache_size_from_env("-4096"),       // 4MB
                runtime: None,
            },
        )
    }

    /// Open in read-only pooled mode using a pre-existing tokio runtime.
    /// Saves ~15ms per invocation by avoiding runtime creation.
    pub fn open_readonly_pooled_with_runtime(
        path: &Path,
        runtime: Runtime,
    ) -> Result<Self, StoreError> {
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: true,
                use_current_thread: true,
                max_connections: 1,
                mmap_size: resolve_mmap_size("268435456", path),
                cache_size: cache_size_from_env("-16384"),
                runtime: Some(runtime),
            },
        )
    }

    /// Shared open logic for both read-write and read-only modes.
    fn open_with_config(path: &Path, config: StoreOpenConfig) -> Result<Self, StoreError> {
        let mode = if config.read_only { "readonly" } else { "open" };
        let _span = tracing::info_span!("store_open", %mode, path = %path.display()).entered();

        // Reuse provided runtime or build a new one.
        let rt = if let Some(rt) = config.runtime {
            rt
        } else if config.use_current_thread {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
        } else {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(config.max_connections as usize)
                .enable_all()
                .build()?
        };

        // Use SqliteConnectOptions::filename() to avoid URL parsing issues with
        // special characters in paths (spaces, #, ?, %, unicode).
        let mut connect_opts = SqliteConnectOptions::new()
            .filename(path)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(helpers::sql::busy_timeout_from_env(5000))
            // NORMAL synchronous in WAL mode: fsync on checkpoint, not every commit.
            // Trade-off: a crash can lose the last few committed transactions (WAL
            // tail not yet fsynced), but the database remains consistent. Acceptable
            // for a rebuildable search index — `cqs index --force` recovers fully.
            // FULL would fsync every commit, ~2x slower on spinning disk / WSL-NTFS.
            .synchronous(SqliteSynchronous::Normal)
            .pragma("mmap_size", config.mmap_size)
            .log_slow_statements(log::LevelFilter::Warn, std::time::Duration::from_secs(5));

        if config.read_only {
            connect_opts = connect_opts.read_only(true);
        } else {
            connect_opts = connect_opts.create_if_missing(true);
        }

        // Build cache_size PRAGMA string once for the after_connect closure.
        let cache_pragma = format!("PRAGMA cache_size = {}", config.cache_size);

        let pool = rt.block_on(async {
            SqlitePoolOptions::new()
                .max_connections(config.max_connections)
                .idle_timeout(std::time::Duration::from_secs(
                    std::env::var("CQS_IDLE_TIMEOUT_SECS")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(30), // PB-2: shorter timeout to release WAL locks
                ))
                .after_connect(move |conn, _meta| {
                    let pragma = cache_pragma.clone();
                    Box::pin(async move {
                        sqlx::query(&pragma).execute(&mut *conn).await?;
                        sqlx::query("PRAGMA temp_store = MEMORY")
                            .execute(&mut *conn)
                            .await?;
                        Ok(())
                    })
                })
                .connect_with(connect_opts)
                .await
        })?;

        // Set restrictive permissions on database files (Unix only, write mode only)
        #[cfg(unix)]
        if !config.read_only {
            use std::os::unix::fs::PermissionsExt;
            let restrictive = std::fs::Permissions::from_mode(0o600);
            if let Err(e) = std::fs::set_permissions(path, restrictive.clone()) {
                tracing::debug!(path = %path.display(), error = %e, "Failed to set permissions");
            }
            let wal_path = path.with_extension("db-wal");
            let shm_path = path.with_extension("db-shm");
            if let Err(e) = std::fs::set_permissions(&wal_path, restrictive.clone()) {
                tracing::debug!(path = %wal_path.display(), error = %e, "Failed to set permissions");
            }
            if let Err(e) = std::fs::set_permissions(&shm_path, restrictive) {
                tracing::debug!(path = %shm_path.display(), error = %e, "Failed to set permissions");
            }
        }

        tracing::info!(
            path = %path.display(),
            read_only = config.read_only,
            "Database connected"
        );

        // Cheap B-tree sanity check — write opens only.
        //
        // The previous `PRAGMA integrity_check(1)` walked every page and took
        // 85s+ on a 1.1GB database over WSL /mnt/c, dominating every CLI
        // invocation and blocking the eval harness (each `cqs search` shelled
        // out, each open re-paid the cost). Two changes fix that:
        //
        // 1. Skip the check entirely on read-only opens. Reads cannot
        //    introduce corruption, and if a read encounters corrupt pages the
        //    query will fail naturally — an upfront walk of the whole file
        //    just to pre-discover that is not earning its cost for a
        //    rebuildable search index.
        // 2. On write opens, use `PRAGMA quick_check` instead of
        //    `integrity_check`. quick_check validates the B-tree structure
        //    without the slower cross-checks of index content vs table
        //    content, which is the right tradeoff for a startup canary.
        //
        // Opt-in via CQS_INTEGRITY_CHECK=1. The quick_check takes ~40s on
        // WSL /mnt/c (NTFS over 9P) which dominated every write-open. For a
        // rebuildable search index the risk/cost tradeoff favors skipping by
        // default. Legacy CQS_SKIP_INTEGRITY_CHECK=1 still works (forces skip
        // even when CQS_INTEGRITY_CHECK=1 is set).
        let opt_in = std::env::var("CQS_INTEGRITY_CHECK").as_deref() == Ok("1");
        let force_skip = std::env::var("CQS_SKIP_INTEGRITY_CHECK").as_deref() == Ok("1");
        let run_check = opt_in && !force_skip && !config.read_only;
        if config.read_only {
            tracing::debug!("Skipping integrity check (read-only open)");
        } else if !run_check {
            tracing::debug!("Integrity check skipped (set CQS_INTEGRITY_CHECK=1 to enable)");
        }
        if run_check {
            rt.block_on(async {
                let result: (String,) = sqlx::query_as("PRAGMA quick_check(1)")
                    .fetch_one(&pool)
                    .await?;
                if result.0 != "ok" {
                    return Err(StoreError::Corruption(result.0));
                }
                Ok::<_, StoreError>(())
            })?;
        }

        // Read dim from metadata before constructing Store (avoid unsafe mutation).
        // Defaults to EMBEDDING_DIM for fresh/pre-v15 databases without dimensions key.
        let dim = rt
            .block_on(async {
                let row: Option<(String,)> =
                    match sqlx::query_as("SELECT value FROM metadata WHERE key = 'dimensions'")
                        .fetch_optional(&pool)
                        .await
                    {
                        Ok(r) => r,
                        Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => {
                            return Ok::<_, StoreError>(None);
                        }
                        Err(e) => return Err(e.into()),
                    };
                Ok(match row {
                    Some((s,)) => match s.parse::<u32>() {
                        Ok(0) => {
                            tracing::warn!(raw = %s, "dimensions metadata is 0 — invalid, using default");
                            None
                        }
                        Ok(d) => Some(d as usize),
                        Err(e) => {
                            tracing::warn!(raw = %s, error = %e, "dimensions metadata is not a valid integer, using default");
                            None
                        }
                    },
                    None => None,
                })
            })?
            .unwrap_or(crate::EMBEDDING_DIM);

        let store = Self {
            pool,
            rt,
            dim,
            closed: AtomicBool::new(false),
            notes_summaries_cache: RwLock::new(None),
            note_boost_cache: RwLock::new(None),
            call_graph_cache: std::sync::OnceLock::new(),
            test_chunks_cache: std::sync::OnceLock::new(),
            chunk_type_map_cache: std::sync::OnceLock::new(),
            _mode: PhantomData,
        };

        // Skip model name validation on open — dimension is validated at embed time,
        // and configurable models (v1.7.0) can legitimately use any model name.
        // Model mismatch is checked at index time via check_model_version_with().
        store.check_schema_version(path)?;
        store.check_cq_version();

        Ok(store)
    }
}

impl<Mode> Store<Mode> {
    /// Begin a write transaction with in-process serialization (DS-5).
    ///
    /// Acquires `WRITE_LOCK` before calling `pool.begin()`, preventing two
    /// concurrent write transactions from racing to upgrade their deferred
    /// locks to exclusive (which causes SQLITE_BUSY).
    ///
    /// Returns both the mutex guard and the transaction. The guard must be
    /// held (in scope) until the transaction commits or rolls back — dropping
    /// the guard early re-enables concurrent writes.
    ///
    /// Read-only transactions should use `self.pool.begin()` directly.
    pub(crate) async fn begin_write(
        &self,
    ) -> Result<
        (
            std::sync::MutexGuard<'static, ()>,
            sqlx::Transaction<'_, sqlx::Sqlite>,
        ),
        sqlx::Error,
    > {
        // v1.22.0 audit OB-20: span so write-lock contention is visible.
        let _span = tracing::debug_span!("begin_write").entered();
        let guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tx = self.pool.begin().await?;
        Ok((guard, tx))
    }

    /// Reset all in-memory caches without closing the connection pool.
    ///
    /// Cheaper than `drop` + `open`: avoids pool teardown, runtime creation,
    /// PRAGMA setup, and integrity check. Designed for long-lived Store
    /// instances (watch mode, future daemon) that need to pick up index
    /// changes without paying full re-open cost.
    ///
    /// Requires `&mut self` — exclusive access guarantees no concurrent
    /// readers see a half-cleared state.
    pub fn clear_caches(&mut self) {
        let _span = tracing::debug_span!("store_clear_caches").entered();
        *self
            .notes_summaries_cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // PF-V1.25-4: note_boost_cache is derived from notes_summaries_cache;
        // clear alongside it so a reset doesn't leave stale boost data.
        *self
            .note_boost_cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.call_graph_cache = std::sync::OnceLock::new();
        self.test_chunks_cache = std::sync::OnceLock::new();
        self.chunk_type_map_cache = std::sync::OnceLock::new();
        tracing::debug!("Store caches cleared");
    }

    /// Create a new index
    /// Wraps all DDL and metadata inserts in a single transaction so a
    /// crash mid-init cannot leave a partial schema.
    pub fn init(&self, model_info: &ModelInfo) -> Result<(), StoreError> {
        let _span = tracing::info_span!("Store::init").entered();
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // Create tables + indexes + triggers. A naive `schema.split(';')`
            // would cut trigger bodies in half because `CREATE TRIGGER ...
            // BEGIN ... END` contains an embedded semicolon. The loop below
            // tracks whether we're inside a BEGIN/END block (case-insensitive
            // keyword match at word boundaries) and folds the body into a
            // single statement.
            let schema = include_str!("../schema.sql");
            for stmt in split_sql_statements(schema) {
                if stmt.is_empty() {
                    continue;
                }
                sqlx::query(&stmt).execute(&mut *tx).await?;
            }

            // Store metadata (OR REPLACE handles re-init after incomplete cleanup)
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("schema_version")
                .bind(CURRENT_SCHEMA_VERSION.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("model_name")
                .bind(&model_info.name)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("dimensions")
                .bind(model_info.dimensions.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("created_at")
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                .bind("cq_version")
                .bind(env!("CARGO_PKG_VERSION"))
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;

            tracing::info!(
                schema_version = CURRENT_SCHEMA_VERSION,
                "Schema initialized"
            );

            Ok(())
        })
    }

    /// Gracefully close the store, performing WAL checkpoint.
    /// This ensures all WAL changes are written to the main database file,
    /// reducing startup time for subsequent opens and freeing disk space
    /// used by WAL files.
    /// Safe to skip (pool will close connections on drop), but recommended
    /// for clean shutdown in long-running processes.
    pub fn close(self) -> Result<(), StoreError> {
        self.closed.store(true, Ordering::Release);
        self.rt.block_on(async {
            // TRUNCATE mode: checkpoint and delete WAL file
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&self.pool)
                .await?;
            tracing::debug!("WAL checkpoint completed");
            self.pool.close().await;
            Ok(())
        })
    }
}

/// Split a SQL script into individual statements, honoring `CREATE TRIGGER
/// ... BEGIN ... END` blocks so embedded semicolons inside a trigger body
/// don't cause the parser to cut the statement in half.
///
/// This is a minimal implementation — it doesn't handle quoted strings,
/// nested parentheses, or comments beyond line-comments. It's only called
/// on `src/schema.sql`, which is hand-written and trusted.
///
/// Rules:
/// - Skip empty lines and `--` comments at the start of each statement
/// - Semicolons outside a `BEGIN`/`END` block end the statement
/// - Semicolons inside a `BEGIN`/`END` block are part of the body
/// - Case-insensitive keyword matching at word boundaries
fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_trigger_body = false;
    // Walk the text line-by-line. This is good enough for our schema, which
    // places one statement per logical block with comments above.
    for raw_line in sql.lines() {
        let trimmed = raw_line.trim();
        // Strip pure comment / empty lines at the start of a statement.
        if current.trim().is_empty() && (trimmed.is_empty() || trimmed.starts_with("--")) {
            continue;
        }
        // Detect entry into a trigger body via a standalone `BEGIN` token.
        let upper = trimmed.to_ascii_uppercase();
        if !in_trigger_body && (upper == "BEGIN" || upper.ends_with(" BEGIN")) {
            in_trigger_body = true;
        }
        current.push_str(raw_line);
        current.push('\n');
        // Detect end of trigger body via a standalone `END` token (possibly
        // followed by a trailing semicolon on the same line).
        if in_trigger_body && (upper.starts_with("END;") || upper == "END" || upper == "END;") {
            in_trigger_body = false;
            // The trailing semicolon after END closes the whole CREATE
            // TRIGGER statement. Flush.
            statements.push(current.trim().trim_end_matches(';').trim().to_string());
            current.clear();
            continue;
        }
        // Outside a trigger body: split on `;` at end-of-line.
        if !in_trigger_body && trimmed.ends_with(';') {
            // Strip the trailing semicolon from the flushed statement.
            let stmt = current.trim().trim_end_matches(';').trim().to_string();
            if !stmt.is_empty() {
                statements.push(stmt);
            }
            current.clear();
        }
    }
    // Flush any trailing statement with no final semicolon.
    let tail = current.trim().to_string();
    if !tail.is_empty() {
        statements.push(tail);
    }
    statements
}

#[cfg(test)]
mod sql_split_tests {
    use super::split_sql_statements;

    #[test]
    fn test_split_single_table() {
        let sql = "CREATE TABLE foo (id INTEGER);";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].starts_with("CREATE TABLE foo"));
    }

    #[test]
    fn test_split_multiple_statements() {
        let sql = "CREATE TABLE foo (id INTEGER);\nCREATE INDEX idx_foo ON foo(id);";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn test_split_trigger_body_preserved() {
        // A trigger body contains an embedded `;` that would split naively.
        let sql = "\
CREATE TABLE foo (id INTEGER);

CREATE TRIGGER bump_on_delete
AFTER DELETE ON foo
BEGIN
    INSERT INTO bar (x) VALUES (1);
END;

CREATE TABLE baz (id INTEGER);
";
        let stmts = split_sql_statements(sql);
        assert_eq!(
            stmts.len(),
            3,
            "foo + trigger + baz — trigger body must not be cut"
        );
        assert!(stmts[1].contains("CREATE TRIGGER"));
        assert!(
            stmts[1].contains("INSERT INTO bar"),
            "trigger body must be preserved intact"
        );
        assert!(stmts[1].contains("END"), "trigger END must be included");
    }

    #[test]
    fn test_split_skips_leading_comments() {
        let sql = "-- comment\n-- another\nCREATE TABLE foo (id INTEGER);";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].starts_with("CREATE TABLE"));
    }

    #[test]
    fn test_split_empty_input() {
        assert!(split_sql_statements("").is_empty());
        assert!(split_sql_statements("-- just a comment\n").is_empty());
        assert!(split_sql_statements("\n\n\n").is_empty());
    }
}

impl<Mode> Drop for Store<Mode> {
    /// Performs a best-effort WAL (Write-Ahead Logging) checkpoint when the Store is dropped to prevent accumulation of large WAL files.
    /// # Arguments
    /// * `&mut self` - A mutable reference to the Store instance being dropped
    /// # Returns
    /// Nothing. Errors during checkpoint are logged as warnings but not propagated, as Drop implementations cannot fail.
    /// # Panics
    /// Does not panic. Uses `catch_unwind` to safely handle potential panics from `block_on` when called from within an async context (e.g., dropping Store inside a tokio runtime).
    fn drop(&mut self) {
        if self.closed.load(Ordering::Acquire) {
            return; // Already checkpointed in close()
        }
        // Best-effort WAL checkpoint on drop to avoid leaving large WAL files.
        // Errors are logged but not propagated (Drop can't fail).
        // catch_unwind guards against block_on panicking when called from
        // within an async context (e.g., if Store is dropped inside a tokio runtime).
        // v1.22.0 audit EH-9: previously `let _ =` silently swallowed the
        // panic payload. Now logs it so the caught panic is visible.
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(e) = self.rt.block_on(async {
                sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                    .execute(&self.pool)
                    .await
            }) {
                tracing::warn!(error = %e, "WAL checkpoint on drop failed (non-fatal)");
            }
        })) {
            let msg = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("unknown panic");
            tracing::warn!(
                panic = msg,
                "WAL checkpoint panic caught in Store::drop (non-fatal)"
            );
        }
        // Pool closes automatically when dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    use crate::nl::normalize_for_fts;

    // ===== FTS fuzz tests =====

    proptest! {
        #[test]
        fn fuzz_normalize_for_fts_no_panic(input in "\\PC{0,500}") {
            let _ = normalize_for_fts(&input);
        }

        #[test]
        fn fuzz_normalize_for_fts_safe_output(input in "\\PC{0,200}") {
            let result = normalize_for_fts(&input);
            for c in result.chars() {
                prop_assert!(
                    c.is_alphanumeric() || c == ' ' || c == '_',
                    "Unexpected char '{}' (U+{:04X}) in output: {}",
                    c, c as u32, result
                );
            }
        }

        #[test]
        fn fuzz_normalize_for_fts_special_chars(
            prefix in "[a-z]{0,10}",
            special in prop::sample::select(vec!['*', '"', ':', '^', '(', ')', '-', '+']),
            suffix in "[a-z]{0,10}"
        ) {
            let input = format!("{}{}{}", prefix, special, suffix);
            let result = normalize_for_fts(&input);
            prop_assert!(
                !result.contains(special),
                "Special char '{}' should be stripped from: {} -> {}",
                special, input, result
            );
        }

        #[test]
        fn fuzz_normalize_for_fts_unicode(input in "[\\p{L}\\p{N}\\s]{0,100}") {
            let result = normalize_for_fts(&input);
            prop_assert!(result.len() <= input.len() * 4);
        }

        // ===== sanitize_fts_query property tests (SEC-4) =====

        /// Output never contains FTS5 special characters
        #[test]
        fn prop_sanitize_no_special_chars(input in "\\PC{0,500}") {
            let result = sanitize_fts_query(&input);
            for c in result.chars() {
                prop_assert!(
                    !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':'),
                    "FTS5 special char '{}' in sanitized output: {}",
                    c, result
                );
            }
        }

        /// Output never contains standalone boolean operators
        #[test]
        fn prop_sanitize_no_operators(input in "\\PC{0,300}") {
            let result = sanitize_fts_query(&input);
            for word in result.split_whitespace() {
                prop_assert!(
                    !matches!(word, "OR" | "AND" | "NOT" | "NEAR"),
                    "FTS5 operator '{}' survived sanitization: {}",
                    word, result
                );
            }
        }

        /// Combined pipeline: normalize + sanitize is safe for arbitrary input
        #[test]
        fn prop_pipeline_safe(input in "\\PC{0,300}") {
            let result = sanitize_fts_query(&normalize_for_fts(&input));
            // No FTS5 special chars
            for c in result.chars() {
                prop_assert!(
                    !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':'),
                    "Special char '{}' in pipeline output: {}",
                    c, result
                );
            }
            // No boolean operators
            for word in result.split_whitespace() {
                prop_assert!(
                    !matches!(word, "OR" | "AND" | "NOT" | "NEAR"),
                    "Operator '{}' in pipeline output: {}",
                    word, result
                );
            }
        }

        /// Targeted: strings composed entirely of special chars produce empty output
        #[test]
        fn prop_sanitize_all_special(
            chars in prop::collection::vec(
                prop::sample::select(vec!['"', '*', '(', ')', '+', '-', '^', ':']),
                1..50
            )
        ) {
            let input: String = chars.into_iter().collect();
            let result = sanitize_fts_query(&input);
            prop_assert!(
                result.is_empty(),
                "All-special input should produce empty output, got: {}",
                result
            );
        }

        /// Targeted: operator words surrounded by normal text are stripped
        #[test]
        fn prop_sanitize_operators_removed(
            pre in "[a-z]{1,10}",
            op in prop::sample::select(vec!["OR", "AND", "NOT", "NEAR"]),
            post in "[a-z]{1,10}"
        ) {
            let input = format!("{} {} {}", pre, op, post);
            let result = sanitize_fts_query(&input);
            prop_assert!(
                !result.split_whitespace().any(|w| w == op),
                "Operator '{}' not stripped from: {} -> {}",
                op, input, result
            );
            // Pre and post words should survive
            prop_assert!(result.contains(&pre), "Pre-text '{}' missing from: {}", pre, result);
            prop_assert!(result.contains(&post), "Post-text '{}' missing from: {}", post, result);
        }

        /// Adversarial: mixed special chars + operators + normal text
        #[test]
        fn prop_sanitize_adversarial(
            normal in "[a-z]{1,10}",
            special in prop::sample::select(vec!['"', '*', '(', ')', '+', '-', '^', ':']),
            op in prop::sample::select(vec!["OR", "AND", "NOT", "NEAR"]),
        ) {
            let input = format!("{}{} {} {}{}", special, normal, op, normal, special);
            let result = sanitize_fts_query(&input);
            for c in result.chars() {
                prop_assert!(
                    !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':'),
                    "Special char '{}' in adversarial output: {}",
                    c, result
                );
            }
            for word in result.split_whitespace() {
                prop_assert!(
                    !matches!(word, "OR" | "AND" | "NOT" | "NEAR"),
                    "Operator '{}' in adversarial output: {}",
                    word, result
                );
            }
        }
    }

    // ===== TC-19: concurrent access and edge-case tests =====

    fn make_test_store_initialized() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn concurrent_readonly_opens() {
        // Two readonly stores opened against the same DB should both succeed (WAL allows
        // multiple readers).
        let (_writer, dir) = make_test_store_initialized();
        let db_path = dir.path().join("index.db");

        let ro1 = Store::open_readonly(&db_path).expect("first readonly open failed");
        let ro2 = Store::open_readonly(&db_path).expect("second readonly open failed");

        // Both stores should be able to query metadata without error.
        assert!(ro1.check_model_version().is_ok());
        assert!(ro2.check_model_version().is_ok());
    }

    #[test]
    fn readonly_open_while_writer_holds() {
        // A readonly store opened while a writer Store is alive should succeed.
        // SQLite WAL mode permits concurrent readers alongside a writer.
        let (writer, dir) = make_test_store_initialized();
        let db_path = dir.path().join("index.db");

        let ro = Store::open_readonly(&db_path).expect("readonly open failed while writer active");
        assert!(ro.check_model_version().is_ok());

        // Writer is still alive — drop it after to make the intent clear.
        drop(writer);
    }

    #[test]
    fn onclock_cache_not_invalidated_by_writes() {
        // get_call_graph() populates the OnceLock cache on first call.
        // Subsequent writes to function_calls must NOT update the cached value —
        // this is intentional by design (per-command Store lifetime contract).
        let (store, _dir) = make_test_store_initialized();

        // Prime the cache with an empty call graph.
        let graph_before = store.get_call_graph().expect("first get_call_graph failed");
        let callers_before = graph_before.forward.len();

        // Write new call data to the store.
        store
            .upsert_function_calls(
                std::path::Path::new("test.rs"),
                &[crate::parser::FunctionCalls {
                    name: "caller".to_string(),
                    line_start: 1,
                    calls: vec![crate::parser::CallSite {
                        callee_name: "callee".to_string(),
                        line_number: 2,
                    }],
                }],
            )
            .unwrap();

        // Cache must still return the stale (pre-write) value.
        let graph_after = store
            .get_call_graph()
            .expect("second get_call_graph failed");
        assert_eq!(
            graph_after.forward.len(),
            callers_before,
            "OnceLock cache should not be invalidated by writes within the same Store lifetime"
        );
    }

    #[test]
    fn double_init_is_idempotent() {
        // Calling init() twice on the same store should succeed without error.
        // Schema uses INSERT OR REPLACE / CREATE TABLE IF NOT EXISTS, so a second
        // init() must be a no-op rather than a conflict.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();

        store
            .init(&ModelInfo::default())
            .expect("first init() failed");
        store
            .init(&ModelInfo::default())
            .expect("second init() should be idempotent but failed");
    }
}
