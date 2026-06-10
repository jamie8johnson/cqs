//! Global embedding cache for fast model switching.
//!
//! Stores embeddings keyed by `(content_hash, model_fingerprint)` in a SQLite DB
//! at `~/.cache/cqs/embeddings.db`. Transparent acceleration layer — the index
//! pipeline checks the cache before running ONNX inference.
//!
//! The cache is global (shared across all projects) and best-effort (failures
//! warn and fall back to normal embedding, never abort the pipeline).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use thiserror::Error;

/// Process-global mutex serializing `EmbeddingCache::evict()` across all
/// `EmbeddingCache` handles in this process.
///
/// `EmbeddingCache::open` is called from multiple paths (bulk pipeline
/// `prepare_for_embedding`, watch daemon reindex, `cqs cache prune`). A
/// shared lock prevents two instances calling `evict()` concurrently from
/// different runtimes from each measuring the same logical size and issuing
/// overlapping `LIMIT ?` DELETEs. Cross-process serialization relies on
/// SQLite busy_timeout + the `BEGIN IMMEDIATE` evict transaction.
static EMBEDDING_CACHE_EVICT_LOCK: Mutex<()> = Mutex::new(());

/// Process-global mutex serializing `QueryCache::evict()` across all
/// `QueryCache` handles in this process. See `EMBEDDING_CACHE_EVICT_LOCK`.
static QUERY_CACHE_EVICT_LOCK: Mutex<()> = Mutex::new(());

/// Cap the on-disk WAL at this many pages on each open so an abrupt shutdown
/// (SIGKILL, panic, daemon worker crash) leaves a bounded WAL for the next
/// open. Default 1000 pages mirrors SQLite's built-in autocheckpoint default;
/// set explicitly so it applies to read-mostly cache connections that rarely
/// COMMIT (without an explicit PRAGMA the autocheckpoint only runs on COMMIT).
/// Override via `CQS_WAL_AUTOCHECKPOINT_PAGES`.
fn wal_autocheckpoint_pragma() -> String {
    let pages: u32 = std::env::var("CQS_WAL_AUTOCHECKPOINT_PAGES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    format!("PRAGMA wal_autocheckpoint = {}", pages)
}

use crate::store::helpers::sql::{
    busy_timeout_from_env, make_placeholders_offset, max_rows_per_statement,
};

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("Cache database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Defense-in-depth: clock anomalies (system clock before unix epoch, or
    /// past i64 cap in 2554) and pathologically out-of-range arguments
    /// (e.g. `--older-than 999999999999`). Surfaced as an error rather than
    /// silently wrapping/clamping so the operator sees the corruption.
    #[error("Cache internal error: {0}")]
    Internal(String),
}

/// Returns the current Unix timestamp in seconds as an `i64`, or
/// [`CacheError::Internal`] if the clock is before the epoch or past the
/// i64 ceiling (year 2554). Defense-in-depth — `as i64` casts on `as_secs()`
/// silently wrap above `i64::MAX`.
fn now_unix_i64() -> Result<i64, CacheError> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CacheError::Internal("clock before unix epoch".into()))?
        .as_secs();
    i64::try_from(secs).map_err(|_| CacheError::Internal("clock above i64 cap".into()))
}

/// Statistics about the embedding cache.
#[derive(Debug)]
pub struct CacheStats {
    pub total_entries: u64,
    pub total_size_bytes: u64,
    pub unique_models: u64,
    pub oldest_timestamp: Option<i64>,
    pub newest_timestamp: Option<i64>,
}

/// Per-model cache statistics — surfaced by [`EmbeddingCache::stats_per_model`]
/// for `cqs cache stats` so users can see which model_id holds how many
/// embeddings before pruning.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PerModelStats {
    pub model_id: String,
    pub entries: u64,
    pub total_bytes: u64,
}

/// File name of the project-scoped embeddings cache, sibling to `slots/`
/// inside `.cqs/`. The cache is shared across all slots of a project so an
/// embedder swap (BGE → E5 etc.) only re-embeds chunks whose hash hasn't
/// previously been seen for that model_id.
pub const PROJECT_EMBEDDINGS_CACHE_FILENAME: &str = "embeddings_cache.db";

/// Discriminator for which dual-index column an embedding was generated for.
///
/// The cache keys on `(content_hash, model_fingerprint, purpose)`. The
/// `embedding_base` column holds the raw NL embedding (before enrichment
/// overwrites `embedding`), so the same content + model can produce two
/// different vectors — one per column. Without a `purpose` discriminator in
/// the cache PK, the second writer silently overwrites the first, and reads
/// return whichever was last written.
///
/// `Embedding` is the default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CachePurpose {
    /// The post-enrichment embedding — what `chunks.embedding` holds and what
    /// HNSW serves search against.
    #[default]
    Embedding,
    /// The raw NL embedding (pre-enrichment) — what `chunks.embedding_base`
    /// holds and what the dual-index "base" graph serves.
    EmbeddingBase,
}

impl CachePurpose {
    /// Stable string form persisted in the `purpose` column. Do NOT change
    /// without a schema migration — caches store `'embedding'` as the default.
    pub fn as_str(&self) -> &'static str {
        match self {
            CachePurpose::Embedding => "embedding",
            CachePurpose::EmbeddingBase => "embedding_base",
        }
    }
}

/// Restrict the DB file and its WAL/SHM sidecars to 0o600. Best-effort:
/// chmod failures (NFS, read-only mounts, filesystems without unix
/// permissions) are logged and skipped rather than failing the open.
#[cfg(unix)]
fn apply_db_file_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    for suffix in &["", "-wal", "-shm"] {
        let db_file = path.with_extension(
            path.extension()
                .map(|e| format!("{}{}", e.to_string_lossy(), suffix))
                .unwrap_or_else(|| suffix.trim_start_matches('-').to_string()),
        );
        if db_file.exists() {
            if let Err(e) = std::fs::set_permissions(&db_file, perms.clone()) {
                tracing::warn!(
                    path = %db_file.display(),
                    error = %e,
                    "Failed to set cache permissions to 0o600"
                );
            }
        }
    }
}

#[cfg(not(unix))]
fn apply_db_file_perms(_path: &Path) {}

/// Shared pool-open skeleton for [`EmbeddingCache::open_with_runtime`] and
/// [`QueryCache::open_with_runtime`]: parent-dir prep (0o700), runtime
/// fallback, WAL/Normal connect options honouring `CQS_BUSY_TIMEOUT_MS`, the
/// 0o077 umask wrap around pool creation, the per-connection
/// `wal_autocheckpoint` pragma, schema initialization, and the 0o600 chmod
/// loop on the DB triplet.
///
/// `busy_timeout_default_ms` is the per-cache default when
/// `CQS_BUSY_TIMEOUT_MS` is unset (30 s embedding cache / 15 s query cache —
/// rationale at the call sites). `init_schema` runs on the freshly opened
/// pool inside the umask window so schema writes (and any sidecar files they
/// create) are born private.
fn connect_cache_pool<F, Fut>(
    path: &Path,
    busy_timeout_default_ms: u64,
    runtime: Option<Arc<tokio::runtime::Runtime>>,
    init_schema: F,
) -> Result<(sqlx::SqlitePool, Arc<tokio::runtime::Runtime>), CacheError>
where
    F: FnOnce(sqlx::SqlitePool) -> Fut,
    Fut: std::future::Future<Output = Result<(), sqlx::Error>>,
{
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort parent chmod. On NFS / read-only mounts /
            // filesystems without unix permissions this fails, but the
            // cache itself is still usable — log and continue instead of
            // refusing to open. Mirrors the DB-file chmod warn arm in
            // `apply_db_file_perms`.
            if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            {
                tracing::warn!(
                    path = %parent.display(),
                    error = %e,
                    "Failed to set cache parent dir permissions to 0o700"
                );
            }
        }
    }

    let rt: Arc<tokio::runtime::Runtime> = if let Some(rt) = runtime {
        rt
    } else {
        Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| CacheError::Io(std::io::Error::other(e)))?,
        )
    };

    // Use SqliteConnectOptions to avoid URL-encoding issues with special
    // paths. Honour CQS_BUSY_TIMEOUT_MS like the main Store pool so the
    // caches don't surrender while the store still waits.
    let connect_opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(busy_timeout_from_env(busy_timeout_default_ms))
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

    // Tighten umask to 0o077 around pool creation so the DB (and WAL/SHM
    // sidecars) are born 0o600, not the user's umask default (0o644).
    // Without this, there is a window between SQLite first-write and
    // `apply_db_file_perms` below where the sidecar files are
    // world-readable. SAFETY: `libc::umask` is process-global; we do this
    // on a synchronous open path before the pool spins up its worker,
    // restoring before any other file-creating code runs.
    #[cfg(unix)]
    let prev_umask = unsafe { libc::umask(0o077) };
    // Cap the on-disk WAL via after_connect so every connection (including
    // the read-only checkout acquired for statistics) carries the ceiling.
    // Same default and env override as the main store.
    let wal_pragma = wal_autocheckpoint_pragma();
    let pool = rt.block_on(async {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1) // single worker thread can only use 1 connection
            .idle_timeout(std::time::Duration::from_secs(30)) // release idle connections
            .after_connect(move |conn, _meta| {
                let wal = wal_pragma.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(wal.as_str()))
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(connect_opts)
            .await?;

        init_schema(pool.clone()).await?;

        Ok::<_, sqlx::Error>(pool)
    })?;
    // Restore the previous umask now that pool creation is done. On the
    // `?` error path above this is skipped, but the process exits (or `?`
    // propagates) before any other umask-sensitive code runs. The success
    // path is the common case and is correctly restored here.
    #[cfg(unix)]
    unsafe {
        libc::umask(prev_umask);
    }

    // Restrict DB + WAL/SHM sidecar files to 0o600. Belt-and-suspenders
    // alongside the umask wrap above so a future refactor that drops the
    // umask doesn't silently regress.
    apply_db_file_perms(path);

    Ok((pool, rt))
}

mod embedding_cache;
mod query_cache;

pub use embedding_cache::EmbeddingCache;
pub use query_cache::QueryCache;

#[cfg(test)]
mod shared_runtime_tests {
    use super::*;

    /// Build the same kind of multi-thread runtime the daemon uses.
    fn build_daemon_runtime() -> Arc<tokio::runtime::Runtime> {
        let worker_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(4);
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(worker_threads)
                .enable_all()
                .build()
                .unwrap(),
        )
    }

    /// Core invariant: the daemon can build one `Arc<Runtime>` and hand
    /// the same handle to `Store::open_with_runtime`,
    /// `EmbeddingCache::open_with_runtime`, and
    /// `QueryCache::open_with_runtime`; all three operate concurrently and
    /// drop cleanly without deadlocking or panicking.
    #[test]
    fn test_shared_runtime_drives_all_three() {
        let dir = tempfile::TempDir::new().unwrap();
        let shared_rt = build_daemon_runtime();

        // --- Store ---
        let store_path = dir.path().join("index.db");
        let store =
            crate::store::Store::open_with_runtime(&store_path, Arc::clone(&shared_rt)).unwrap();
        store
            .init(&crate::store::ModelInfo::default())
            .expect("store init");
        // Sanity: the store's runtime is the same Arc we handed in.
        assert!(Arc::ptr_eq(store.runtime(), &shared_rt));

        // --- EmbeddingCache ---
        let emb_path = dir.path().join("embeddings.db");
        let emb_cache =
            EmbeddingCache::open_with_runtime(&emb_path, Some(Arc::clone(&shared_rt))).unwrap();
        // Round-trip one entry so the cache actually uses the runtime.
        let entries = vec![("h1".to_string(), vec![0.1_f32; 8])];
        assert_eq!(
            emb_cache
                .write_batch_owned(&entries, "fp", CachePurpose::Embedding, 8)
                .unwrap(),
            1
        );
        let got = emb_cache
            .read_batch(&["h1"], "fp", CachePurpose::Embedding, 8)
            .unwrap();
        assert_eq!(got.len(), 1);

        // --- QueryCache ---
        let q_path = dir.path().join("query_cache.db");
        let q_cache = QueryCache::open_with_runtime(&q_path, Some(Arc::clone(&shared_rt))).unwrap();
        let q_emb = crate::embedder::Embedding::new(vec![0.2_f32; 8]);
        q_cache.put("select x", "fp", &q_emb);
        let got = q_cache.get("select x", "fp").expect("round-trip");
        assert_eq!(got.as_slice().len(), 8);

        // Five live Arcs: shared_rt + Store + Store's summary_queue
        // + EmbeddingCache + QueryCache. Store contributes two refs because
        // it spawns a `PendingSummaryQueue` that holds its own `Arc<Runtime>`
        // clone for `block_on` driving the queue's SQL writes.
        assert_eq!(Arc::strong_count(&shared_rt), 5);

        // Drop consumers — runtime must outlive all of them already.
        drop(store);
        drop(emb_cache);
        drop(q_cache);
        assert_eq!(Arc::strong_count(&shared_rt), 1);
    }

    /// `Store::open_readonly_pooled_with_runtime` works under the
    /// `Arc<Runtime>` signature. Guards against the template drifting from
    /// `Store::open_with_runtime` as new knobs land.
    #[test]
    fn test_open_readonly_pooled_with_runtime_works() {
        let dir = tempfile::TempDir::new().unwrap();
        let shared_rt = build_daemon_runtime();
        let path = dir.path().join("ro.db");

        // Initialize the DB under ReadWrite first — open_readonly_pooled
        // refuses to create a new DB.
        {
            let rw = crate::store::Store::open_with_runtime(&path, Arc::clone(&shared_rt)).unwrap();
            rw.init(&crate::store::ModelInfo::default()).unwrap();
        }

        let ro =
            crate::store::Store::open_readonly_pooled_with_runtime(&path, Arc::clone(&shared_rt))
                .unwrap();
        assert!(Arc::ptr_eq(ro.runtime(), &shared_rt));
        // `chunk_count` flows through `self.rt.block_on`, so a live
        // runtime on the shared Arc is what makes the read path work.
        let count = ro.chunk_count().unwrap();
        assert_eq!(count, 0);
    }
}
