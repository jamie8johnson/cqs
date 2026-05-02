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

use thiserror::Error;

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
/// #1128: the cache used to key on `(content_hash, model_fingerprint)`, but v18
/// added a parallel `embedding_base` column (raw NL embedding, before #1040
/// enrichment overwrites `embedding`). Same content + same model can now
/// produce two different vectors — one for each column. Without a `purpose`
/// discriminator in the cache PK, the second writer silently overwrites the
/// first, and reads return whichever was last written.
///
/// `Embedding` is the default (matches the only producer until enrichment
/// caching lands), so existing rows migrate to `purpose = 'embedding'` via
/// the schema's `DEFAULT 'embedding'`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CachePurpose {
    /// The post-enrichment embedding (or, today, the only one) — what
    /// `chunks.embedding` holds and what HNSW serves search against.
    #[default]
    Embedding,
    /// The raw NL embedding (pre-enrichment) — what `chunks.embedding_base`
    /// holds and what the dual-index "base" graph serves.
    EmbeddingBase,
}

impl CachePurpose {
    /// Stable string form persisted in the `purpose` column. Do NOT change
    /// without a schema migration — existing caches store `'embedding'` as
    /// the default for pre-#1128 rows.
    pub fn as_str(&self) -> &'static str {
        match self {
            CachePurpose::Embedding => "embedding",
            CachePurpose::EmbeddingBase => "embedding_base",
        }
    }
}

/// P2.3 (scope=structural): both [`EmbeddingCache::open_with_runtime`] and
/// [`QueryCache::open_with_runtime`] share ~90 lines of parent-dir prep,
/// runtime fallback, pool open, schema create, and 0o600 chmod loop with
/// only `busy_timeout` (5000 vs 2000 ms) and the schema SQL differing.
///
/// A full extraction would yield three private helpers (`prepare_cache_dir_perms`,
/// `apply_db_file_perms`, `connect_cache_pool(path, busy_ms, runtime, schema_sql)`)
/// collapsing each `open_with_runtime` to ~30 lines. Out of scope for this
/// batch (touches both opens + their tests + WAL/SHM filename quirk handling
/// in `apply_db_file_perms`); filed as follow-on issue. This module-level
/// note is the breadcrumb so the next sweep doesn't re-discover the
/// duplication from scratch.
///
/// Two of the duplications were just unified in spirit by the v1.30.0 audit
/// fixes — P2.5 (zero-handling on `CQS_CACHE_MAX_SIZE`) and P2.27 (NaN/Inf
/// rejection) both apply identically to both caches. When the helpers land,
/// those will fold into a single shared function.
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

/// Global embedding cache backed by SQLite.
///
/// Best-effort: all operations that fail are logged and skipped.
/// The index pipeline works identically with or without a functioning cache.
pub struct EmbeddingCache {
    pool: sqlx::SqlitePool,
    /// #968: `Arc<Runtime>` so the daemon can share one multi-thread runtime
    /// across `Store`, `EmbeddingCache`, and `QueryCache` instead of each
    /// constructor spinning up its own worker pool.
    rt: Arc<tokio::runtime::Runtime>,
    max_size_bytes: u64,
    /// DS2-5: serializes `evict()` calls so two parallel invocations can't both
    /// measure the same logical size and issue overlapping `LIMIT ?` DELETEs.
    /// Size/AVG/DELETE inside `evict()` are further wrapped in a single
    /// transaction so the snapshot is consistent against concurrent
    /// `write_batch` traffic.
    evict_lock: std::sync::Mutex<()>,
}

impl EmbeddingCache {
    /// Legacy global cache location.
    ///
    /// Resolves to the platform's native user cache directory:
    /// - Linux: `$XDG_CACHE_HOME/cqs/embeddings.db` or `~/.cache/cqs/embeddings.db`
    /// - macOS: `~/Library/Caches/cqs/embeddings.db`
    /// - Windows: `%LOCALAPPDATA%\cqs\embeddings.db`
    ///
    /// Kept so existing callers (`cqs cache` subcommand pre-slots) continue to
    /// work for invocations outside a project. New code prefers
    /// [`Self::project_default_path`] so caches are scoped to the project and
    /// survive slot promotion / removal.
    pub fn default_path() -> std::path::PathBuf {
        // P3.32: prefer the platform's native cache dir; fall back to
        // `~/.cache` for legacy behavior, then `.` for the headless case.
        dirs::cache_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("cqs")
            .join("embeddings.db")
    }

    /// Project-scoped cache path: `<project_cqs_dir>/embeddings_cache.db`.
    ///
    /// Located alongside `.cqs/slots/` so cache survives `cqs slot remove`
    /// and `cqs slot create` cycles. One file per project — same chunk hashed
    /// across two slots with the same model_id only embeds once.
    pub fn project_default_path(project_cqs_dir: &Path) -> std::path::PathBuf {
        project_cqs_dir.join(PROJECT_EMBEDDINGS_CACHE_FILENAME)
    }

    /// Open or create the embedding cache.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        Self::open_with_runtime(path, None)
    }

    /// Open with a pre-existing runtime (saves ~15ms by avoiding runtime
    /// creation and, per #968, lets the daemon share one runtime across
    /// `Store`, `EmbeddingCache`, and `QueryCache`).
    pub fn open_with_runtime(
        path: &Path,
        runtime: Option<Arc<tokio::runtime::Runtime>>,
    ) -> Result<Self, CacheError> {
        let _span = tracing::info_span!("embedding_cache_open", path = %path.display()).entered();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // PB-V1.29-7: best-effort parent chmod. On NFS / read-only
                // mounts / filesystems without unix permissions this fails,
                // but the cache itself is still usable — log and continue
                // instead of refusing to open. Mirrors the DB-file chmod
                // warn arm below.
                if let Err(e) =
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                {
                    tracing::warn!(
                        path = %parent.display(),
                        error = %e,
                        "Failed to set embedding cache parent dir permissions to 0o700"
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

        // Use SqliteConnectOptions to avoid URL-encoding issues with special paths
        // SHL-V1.25-12: honour CQS_BUSY_TIMEOUT_MS like the main Store pool
        // so the cache doesn't surrender at 5s while the store still waits.
        let connect_opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(busy_timeout_from_env(5000))
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1) // RM-2: single worker thread can only use 1 connection
                .idle_timeout(std::time::Duration::from_secs(30)) // RM-5: release idle connections
                .connect_with(connect_opts)
                .await?;

            // #1128: PRIMARY KEY now includes `purpose` so the same
            // (content_hash, model_fingerprint) can hold both the post-
            // enrichment `embedding` and the raw `embedding_base` vectors
            // without one overwriting the other. New caches get the column
            // up-front in CREATE TABLE; existing caches get it via the
            // idempotent ALTER below.
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    purpose TEXT NOT NULL DEFAULT 'embedding',
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint, purpose)
                )",
            )
            .execute(&pool)
            .await?;

            // #1128: idempotent migration for caches built before the
            // `purpose` column existed. We detect the legacy schema via
            // `pragma_table_info`; if present we rebuild the table so the
            // PRIMARY KEY actually includes `purpose`.
            //
            // SQLite has no `DROP / ADD PRIMARY KEY` — adding the column
            // alone leaves the legacy PK (content_hash, model_fingerprint)
            // in force, which would silently REJECT future EmbeddingBase
            // writes that share a hash with an existing Embedding row.
            // The 12-step `ALTER TABLE` recipe (rename → CREATE → INSERT
            // SELECT → DROP) is the SQLite-blessed way to relax the PK on
            // an existing table. All in one transaction so a crash mid-
            // migration leaves either the old shape or the new one,
            // never a half-applied state.
            //
            // Existing rows get `purpose = 'embedding'` because the legacy
            // producer only ever wrote that purpose.
            let has_purpose: bool = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pragma_table_info('embedding_cache') WHERE name = 'purpose'",
            )
            .fetch_one(&pool)
            .await?
                > 0;
            if !has_purpose {
                let mut tx = pool.begin().await?;
                sqlx::query("ALTER TABLE embedding_cache RENAME TO embedding_cache_legacy_v1128")
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "CREATE TABLE embedding_cache (
                        content_hash TEXT NOT NULL,
                        model_fingerprint TEXT NOT NULL,
                        purpose TEXT NOT NULL DEFAULT 'embedding',
                        embedding BLOB NOT NULL,
                        dim INTEGER NOT NULL,
                        created_at INTEGER NOT NULL,
                        PRIMARY KEY (content_hash, model_fingerprint, purpose)
                    )",
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO embedding_cache \
                     (content_hash, model_fingerprint, purpose, embedding, dim, created_at) \
                     SELECT content_hash, model_fingerprint, 'embedding', \
                            embedding, dim, created_at \
                     FROM embedding_cache_legacy_v1128",
                )
                .execute(&mut *tx)
                .await?;
                sqlx::query("DROP TABLE embedding_cache_legacy_v1128")
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                tracing::info!(
                    "Migrated embedding_cache schema: added `purpose` column to PRIMARY KEY \
                     (existing rows default to 'embedding')"
                );
            }

            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_cache_created ON embedding_cache (created_at)",
            )
            .execute(&pool)
            .await?;

            Ok::<_, sqlx::Error>(pool)
        })?;

        // P2.3: shared 0o600 chmod loop on the DB triplet — see helper docs.
        apply_db_file_perms(path);

        // P2.5: filter `0` so the env-var matches `QueryCache`'s semantic
        // ("0 is invalid → fall back to default"). Without the filter, setting
        // `CQS_CACHE_MAX_SIZE=0` silently disables eviction entirely (every
        // `evict()` thinks it's already under budget) and the cache grows
        // unbounded. With it, an explicit `0` still gets the 10GB default.
        let max_size_bytes = std::env::var("CQS_CACHE_MAX_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &u64| n > 0)
            .unwrap_or(10 * 1024 * 1024 * 1024); // 10GB default

        tracing::info!("Embedding cache opened");

        Ok(Self {
            pool,
            rt,
            max_size_bytes,
            evict_lock: std::sync::Mutex::new(()),
        })
    }

    /// Read cached embeddings for a batch of content hashes.
    /// Returns a map of content_hash → embedding (as Vec<f32>).
    /// Cache misses are simply absent from the map.
    ///
    /// `purpose` discriminates between the post-enrichment `embedding` and the
    /// raw `embedding_base` (#1128) — same hash + same model can have one row
    /// per purpose. The default purpose is `Embedding`, matching the only
    /// producer until enrichment-purpose caching lands.
    pub fn read_batch(
        &self,
        content_hashes: &[&str],
        model_fingerprint: &str,
        purpose: CachePurpose,
        expected_dim: usize,
    ) -> Result<HashMap<String, Vec<f32>>, CacheError> {
        let _span = tracing::debug_span!(
            "cache_read_batch",
            count = content_hashes.len(),
            fingerprint = &model_fingerprint[..8.min(model_fingerprint.len())],
            purpose = purpose.as_str()
        )
        .entered();

        if content_hashes.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result = HashMap::new();

            // SHL-V1.25-4: Batch size matches modern SQLite variable limit
            // (32766). Three vars per row accounts for the shared
            // model_fingerprint + purpose binds plus the content_hash bind,
            // with headroom. Cache hit lookups for a 50k-chunk index now
            // fire 2-3 SELECTs instead of 500.
            for batch in content_hashes.chunks(max_rows_per_statement(3)) {
                // P3 #130: cached helper. `?1` is `model_fingerprint`, `?2`
                // is `purpose`, the IN clause starts at `?3`.
                let placeholders = make_placeholders_offset(batch.len(), 3);
                let sql = format!(
                    "SELECT content_hash, embedding, dim FROM embedding_cache \
                     WHERE model_fingerprint = ?1 AND purpose = ?2 \
                     AND content_hash IN ({placeholders})"
                );

                let mut query = sqlx::query(&sql)
                    .bind(model_fingerprint)
                    .bind(purpose.as_str());
                for hash in batch {
                    query = query.bind(*hash);
                }

                let rows = query.fetch_all(&self.pool).await?;

                for row in rows {
                    use sqlx::Row;
                    let hash: String = row.get("content_hash");
                    let dim: i64 = row.get("dim");
                    let blob: Vec<u8> = row.get("embedding");

                    // Validate dimension (DS-46: guard negative before cast)
                    if dim < 0 || dim as usize != expected_dim {
                        tracing::debug!(
                            hash = &hash[..8.min(hash.len())],
                            cached_dim = dim,
                            expected_dim,
                            "Cache dim mismatch, skipping"
                        );
                        continue;
                    }

                    // Decode blob to Vec<f32>
                    let embedding: Vec<f32> = blob
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect();

                    if embedding.len() != expected_dim {
                        tracing::debug!(
                            hash = &hash[..8.min(hash.len())],
                            actual = embedding.len(),
                            expected_dim,
                            "Cache blob length mismatch, skipping"
                        );
                        continue;
                    }

                    result.insert(hash, embedding);
                }
            }

            tracing::debug!(hits = result.len(), "Cache read complete");
            Ok(result)
        })
    }

    /// Write a batch of embeddings to the cache (owned variant).
    ///
    /// Convenience wrapper that calls [`write_batch`](Self::write_batch) with
    /// borrowed slices. Used by tests; production paths should call
    /// `write_batch` directly with `&str` / `&[f32]` to avoid the intermediate
    /// `Vec<(String, Vec<f32>)>` allocation (P3 #127).
    pub fn write_batch_owned(
        &self,
        entries: &[(String, Vec<f32>)],
        model_fingerprint: &str,
        purpose: CachePurpose,
        dim: usize,
    ) -> Result<usize, CacheError> {
        let borrowed: Vec<(&str, &[f32])> = entries
            .iter()
            .map(|(h, e)| (h.as_str(), e.as_slice()))
            .collect();
        self.write_batch(&borrowed, model_fingerprint, purpose, dim)
    }

    /// Write a batch of embeddings to the cache.
    /// Best-effort: returns the number written, errors are logged.
    ///
    /// P3 #127: signature accepts borrows (`&str`, `&[f32]`) so the GPU/CPU
    /// embed paths don't need to clone every `content_hash` and embedding
    /// vector into an intermediate `Vec<(String, Vec<f32>)>` per batch.
    ///
    /// `purpose` (#1128) selects which dual-index column the cached vector
    /// belongs to — `Embedding` (default, post-enrichment) or `EmbeddingBase`
    /// (raw NL, pre-enrichment). Same hash + model can have one row per
    /// purpose; INSERT OR IGNORE on collision.
    pub fn write_batch(
        &self,
        entries: &[(&str, &[f32])],
        model_fingerprint: &str,
        purpose: CachePurpose,
        dim: usize,
    ) -> Result<usize, CacheError> {
        let _span = tracing::debug_span!(
            "cache_write_batch",
            count = entries.len(),
            fingerprint = &model_fingerprint[..8.min(model_fingerprint.len())],
            purpose = purpose.as_str()
        )
        .entered();

        if entries.is_empty() {
            return Ok(0);
        }

        // P2.66: hold `evict_lock` across the write so a concurrent `evict()`
        // can't measure size, then DELETE rows that this in-flight write_batch
        // committed between the SELECT and DELETE. Without this, a writer
        // sees its INSERT succeed while a cross-session reader sees a cache
        // miss — silently re-embedding chunks the cache "should" have. Mutex
        // poisoning is non-fatal: a previous holder's panic shouldn't keep
        // the cache write path locked out.
        let _evict_guard = self
            .evict_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let now = now_unix_i64()?;

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            let mut written = 0usize;
            let mut blob = Vec::with_capacity(dim * 4); // PF-6: reuse scratch buffer

            for &(content_hash, embedding) in entries {
                if embedding.is_empty() {
                    continue;
                }

                // DS-44: validate dimension matches
                if embedding.len() != dim {
                    tracing::warn!(
                        hash = &content_hash[..8.min(content_hash.len())],
                        actual = embedding.len(),
                        expected = dim,
                        "Skipping cache write: embedding length mismatch"
                    );
                    continue;
                }

                // P2.27: reject non-finite values. NaN/Inf in cached embeddings
                // poison every downstream reader (cosine produces NaN, breaking
                // sort+rank), and #1105 made it worse by extending cache lifetime
                // across slot create/remove.
                if embedding.iter().any(|f| !f.is_finite()) {
                    tracing::warn!(
                        hash = &content_hash[..8.min(content_hash.len())],
                        "Skipping cache write: embedding contains NaN or Inf"
                    );
                    continue;
                }

                // Encode &[f32] to blob (PF-6: reuse buffer)
                blob.clear();
                blob.extend(embedding.iter().flat_map(|f| f.to_le_bytes()));

                let result = sqlx::query(
                    "INSERT OR IGNORE INTO embedding_cache \
                     (content_hash, model_fingerprint, purpose, embedding, dim, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .bind(content_hash)
                .bind(model_fingerprint)
                .bind(purpose.as_str())
                .bind(&blob)
                .bind(dim as i64)
                .bind(now)
                .execute(&mut *tx)
                .await?;

                written += result.rows_affected() as usize;
            }

            tx.commit().await?;
            tracing::debug!(written, "Cache write complete");
            Ok(written)
        })
    }

    /// Evict oldest entries if cache exceeds max size.
    ///
    /// DS2-5: the size / AVG / DELETE trio runs inside a single
    /// `pool.begin()` transaction so concurrent `write_batch` traffic cannot
    /// invalidate the measurement between steps. An in-process `evict_lock`
    /// mutex further prevents two `evict()` callers from overlapping their
    /// `LIMIT ?` prefixes and each over-counting `rows_affected()`.
    pub fn evict(&self) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("cache_evict").entered();

        // Serialize evicts across threads. Mutex poisoning is non-fatal here:
        // if the previous holder panicked we still want to attempt an evict.
        let _guard = self
            .evict_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        self.rt.block_on(async {
            let mut tx = match self.pool.begin().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, "Cache evict begin-tx failed");
                    return Ok(0);
                }
            };

            // Use logical data size, not physical pages (DS-49)
            let size: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(SUM(LENGTH(embedding)), 0) + COUNT(*) * 200 FROM embedding_cache",
            )
            .fetch_one(&mut *tx)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "Cache evict size query failed");
                    return Ok(0);
                }
            };

            // Guard against negative/zero size (SEC-10)
            if size <= 0 || (size as u64) <= self.max_size_bytes {
                return Ok(0);
            }

            let excess = size as u64 - self.max_size_bytes;
            // Estimate per-entry size from actual data
            let avg_entry: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(AVG(LENGTH(embedding) + 200), 4200) FROM embedding_cache",
            )
            .fetch_one(&mut *tx)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "Cache evict avg-entry query failed, using default");
                    4200
                }
            };
            // AC-1: don't force minimum 100 deletions — delete only what's needed
            let entries_to_delete = (excess / avg_entry.max(1) as u64).max(1);

            let result = sqlx::query(
                "DELETE FROM embedding_cache WHERE rowid IN \
                 (SELECT rowid FROM embedding_cache ORDER BY created_at ASC LIMIT ?1)",
            )
            .bind(entries_to_delete as i64)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;

            let evicted = result.rows_affected() as usize;
            tracing::info!(evicted, "Cache eviction complete");
            Ok(evicted)
        })
    }

    /// Get cache statistics.
    ///
    /// EH-V1.29-7: all five sub-queries propagate sqlx errors via `?` instead
    /// of the previous `unwrap_or_else` zero-fallback. The return type is
    /// `Result<CacheStats, CacheError>` and callers already handle `Err` — a
    /// silent `{total_entries: 0, ...}` on a broken DB reads as "healthy
    /// empty cache" to agents, which is wrong.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let _span = tracing::info_span!("cache_stats").entered();

        self.rt.block_on(async {
            let total_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
                .fetch_one(&self.pool)
                .await?;

            let total_size: i64 = sqlx::query_scalar(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            )
            .fetch_one(&self.pool)
            .await?;

            let unique_models: i64 =
                sqlx::query_scalar("SELECT COUNT(DISTINCT model_fingerprint) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await?;

            let oldest: Option<i64> =
                sqlx::query_scalar("SELECT MIN(created_at) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await?;

            let newest: Option<i64> =
                sqlx::query_scalar("SELECT MAX(created_at) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await?;

            Ok(CacheStats {
                total_entries: total_entries as u64,
                total_size_bytes: total_size as u64,
                unique_models: unique_models as u64,
                oldest_timestamp: oldest,
                newest_timestamp: newest,
            })
        })
    }

    /// Clear all cached embeddings, or only those for a specific model fingerprint.
    pub fn clear(&self, model_fingerprint: Option<&str>) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("cache_clear", model = ?model_fingerprint).entered();

        self.rt.block_on(async {
            let result = if let Some(fp) = model_fingerprint {
                sqlx::query("DELETE FROM embedding_cache WHERE model_fingerprint = ?1")
                    .bind(fp)
                    .execute(&self.pool)
                    .await?
            } else {
                sqlx::query("DELETE FROM embedding_cache")
                    .execute(&self.pool)
                    .await?
            };

            let deleted = result.rows_affected() as usize;
            tracing::info!(deleted, "Cache cleared");
            Ok(deleted)
        })
    }

    /// Prune entries older than the given number of days.
    ///
    /// `days` is clamped to a 100-year ceiling (`MAX_PRUNE_DAYS`) so a typo
    /// (e.g. `--older-than 999999999999`) can't underflow the cutoff and
    /// silently delete everything (P3.20). The `now_unix_i64` helper
    /// (P3.18) defends against clock-wrap above i64::MAX in 2554.
    pub fn prune_older_than(&self, days: u32) -> Result<usize, CacheError> {
        const MAX_PRUNE_DAYS: u32 = 36_500; // 100 years; longer is operator error
        let days_clamped = days.min(MAX_PRUNE_DAYS);
        if days_clamped != days {
            tracing::warn!(
                requested = days,
                effective = days_clamped,
                "cache prune --older-than clamped to 100-year ceiling"
            );
        }
        let days = days_clamped;
        let _span = tracing::info_span!("cache_prune", days).entered();

        let now = now_unix_i64()?;
        // Saturating subtraction: if `cutoff` would go below epoch (clock skew /
        // very-large `days`), no rows can possibly be that old, so the prune is
        // a no-op. Without this branch the SIGNED comparison `created_at < cutoff`
        // returns true for every row (all `created_at >= 0` and `cutoff < 0`),
        // silently deleting everything.
        let cutoff_sat = (days as i64)
            .checked_mul(86400)
            .and_then(|d| now.checked_sub(d));
        let cutoff = match cutoff_sat {
            Some(c) if c >= 0 => c,
            _ => {
                tracing::info!(days, now, "cache prune: cutoff below epoch — no-op");
                return Ok(0);
            }
        };

        self.rt.block_on(async {
            let result = sqlx::query("DELETE FROM embedding_cache WHERE created_at < ?1")
                .bind(cutoff)
                .execute(&self.pool)
                .await?;

            let pruned = result.rows_affected() as usize;
            tracing::info!(pruned, "Cache pruned");
            Ok(pruned)
        })
    }

    /// Drop every cache entry tagged with the given `model_id`.
    ///
    /// Used by `cqs cache prune --model <id>` after a model swap when the
    /// user knows the corresponding embeddings will never be reused. Returns
    /// the number of rows deleted. Identical to [`Self::clear`] with
    /// `Some(model_id)` but exposes the spec's verb shape.
    pub fn prune_by_model(&self, model_id: &str) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("cache_prune_by_model", model_id).entered();
        self.clear(Some(model_id))
    }

    /// `VACUUM` the cache database to reclaim unused pages after large
    /// deletes. Spec §Cache: surface as `cqs cache compact`.
    pub fn compact(&self) -> Result<(), CacheError> {
        let _span = tracing::info_span!("cache_compact").entered();
        self.rt.block_on(async {
            // VACUUM cannot run inside an explicit transaction.
            sqlx::query("VACUUM").execute(&self.pool).await?;
            tracing::info!("Cache vacuumed");
            Ok(())
        })
    }

    /// Per-model cache statistics — entry count + sum-of-embedding-bytes.
    ///
    /// Surfaced by `cqs cache stats` so users can pick a `prune_by_model`
    /// target. Returns rows sorted by entry count descending.
    pub fn stats_per_model(&self) -> Result<Vec<PerModelStats>, CacheError> {
        let _span = tracing::info_span!("cache_stats_per_model").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, i64, i64)> = sqlx::query_as(
                "SELECT model_fingerprint, COUNT(*), COALESCE(SUM(LENGTH(embedding)), 0) \
                 FROM embedding_cache \
                 GROUP BY model_fingerprint \
                 ORDER BY COUNT(*) DESC",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|(model_id, entries, bytes)| PerModelStats {
                    model_id,
                    entries: entries.max(0) as u64,
                    total_bytes: bytes.max(0) as u64,
                })
                .collect())
        })
    }

    /// Partition `items` into (cached, missed) by checking which content
    /// hashes already have an embedding stored for `model_id` and `purpose`.
    ///
    /// Spec §Cache: pre-filter before the embed batch so only misses go
    /// through the GPU. `hash_fn` extracts the content hash bytes (matching
    /// the cache's stored `content_hash` column) from each item.
    ///
    /// Returns `(cached_with_emb, missed)` where:
    /// - `cached_with_emb`: items whose hash hit, paired with the cached
    ///   `Vec<f32>` embedding
    /// - `missed`: items whose hash didn't hit (or whose dim mismatched —
    ///   stale entries are silently re-embedded by the caller; the cache
    ///   write later overwrites via `INSERT OR IGNORE` once the dim matches)
    ///
    /// Preserves input order for both arrays so the caller can interleave
    /// fresh embeddings back in their original positions.
    ///
    /// `purpose` (#1128) discriminates between the post-enrichment `embedding`
    /// and the raw `embedding_base` columns — same hash + model can have one
    /// row per purpose.
    #[allow(clippy::type_complexity)]
    pub fn partition<'a, T>(
        &self,
        items: &'a [T],
        model_id: &str,
        purpose: CachePurpose,
        expected_dim: usize,
        hash_fn: impl Fn(&T) -> &str,
    ) -> Result<(Vec<(&'a T, Vec<f32>)>, Vec<&'a T>), CacheError> {
        let _span = tracing::debug_span!(
            "cache_partition",
            count = items.len(),
            model_id_prefix = &model_id[..8.min(model_id.len())],
            purpose = purpose.as_str()
        )
        .entered();

        if items.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let hashes: Vec<&str> = items.iter().map(&hash_fn).collect();
        let hits = self.read_batch(&hashes, model_id, purpose, expected_dim)?;
        let mut cached = Vec::with_capacity(hits.len());
        let mut missed = Vec::with_capacity(items.len() - hits.len());
        for item in items {
            let h = hash_fn(item);
            if let Some(emb) = hits.get(h) {
                cached.push((item, emb.clone()));
            } else {
                missed.push(item);
            }
        }
        Ok((cached, missed))
    }

    /// Insert many `(content_hash, model_id, embedding)` tuples in one
    /// transaction. Convenience wrapper over [`Self::write_batch`] when the
    /// caller doesn't already have entries grouped by model.
    ///
    /// Mixed `model_id` values across `entries` are handled by grouping
    /// entries per model and issuing one `write_batch` per group.
    ///
    /// All entries are written under the supplied `purpose` (#1128). Callers
    /// that need to write both `Embedding` and `EmbeddingBase` rows must call
    /// `insert_many` once per purpose.
    pub fn insert_many(
        &self,
        entries: &[(Vec<u8>, String, Vec<f32>)],
        purpose: CachePurpose,
        expected_dim: usize,
    ) -> Result<usize, CacheError> {
        let _span = tracing::debug_span!(
            "cache_insert_many",
            count = entries.len(),
            purpose = purpose.as_str()
        )
        .entered();
        if entries.is_empty() {
            return Ok(0);
        }
        // Group by model_id.
        let mut groups: HashMap<&str, Vec<(String, &[f32])>> = HashMap::new();
        for (hash, model_id, emb) in entries {
            // Cache stores content_hash as TEXT. Convert blob → utf-8 hex for
            // backwards compatibility with the existing column type.
            let hex = blake3_hex_or_passthrough(hash);
            groups
                .entry(model_id)
                .or_default()
                .push((hex, emb.as_slice()));
        }
        let mut total = 0;
        for (model_id, group) in groups {
            // Collapse to the borrowed shape the existing write_batch expects
            // — owned String for the hash, borrowed slice for the embedding.
            let borrowed: Vec<(&str, &[f32])> =
                group.iter().map(|(h, e)| (h.as_str(), *e)).collect();
            total += self.write_batch(&borrowed, model_id, purpose, expected_dim)?;
        }
        Ok(total)
    }
}

/// Best-effort hex encoding for blob hashes. If the bytes are already a valid
/// UTF-8 hex string (the common case — `Chunk::content_hash` is produced as
/// a hex string), the value passes through unchanged.
fn blake3_hex_or_passthrough(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => s.to_string(),
        _ => {
            let mut s = String::with_capacity(bytes.len() * 2);
            for b in bytes {
                use std::fmt::Write;
                let _ = write!(s, "{:02x}", b);
            }
            s
        }
    }
}

impl Drop for EmbeddingCache {
    /// RM-V1.29-7: mirror `Store::drop`. Best-effort `PRAGMA wal_checkpoint(TRUNCATE)`
    /// on daemon shutdown so the embedding cache's `-wal` sidecar doesn't
    /// accumulate hundreds of MB across weeks of daemon restarts. Errors and
    /// panics are swallowed (Drop can't fail). `catch_unwind` guards against
    /// `block_on` panicking when the cache is dropped from inside a tokio
    /// runtime.
    fn drop(&mut self) {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(e) = self.rt.block_on(async {
                sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                    .execute(&self.pool)
                    .await
            }) {
                tracing::warn!(
                    error = %e,
                    "EmbeddingCache WAL checkpoint on drop failed (non-fatal)"
                );
            }
        })) {
            let msg = crate::panic_message(&payload);
            tracing::warn!(
                panic = %msg,
                "WAL checkpoint panic caught in EmbeddingCache::drop (non-fatal)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache() -> (EmbeddingCache, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test_cache.db");
        let cache = EmbeddingCache::open(&path).unwrap();
        (cache, dir)
    }

    fn make_embedding(dim: usize, seed: f32) -> Vec<f32> {
        (0..dim).map(|i| seed + i as f32 * 0.001).collect()
    }

    // P3.17: pin the surprises in `blake3_hex_or_passthrough` so a future
    // "always-encode" tightening surfaces as an intentional break, not a
    // silent change in cache-key format. The current invariant is:
    //
    //   - Exactly 64 ASCII hex chars (any case) → passthrough as String.
    //   - Anything else (short, long, non-hex, raw bytes) → hex-encode.

    #[test]
    fn blake3_hex_or_passthrough_uppercase_64_chars_passthrough() {
        let upper = "ABCDEF0123456789".repeat(4); // 64 chars, all hex
        assert_eq!(blake3_hex_or_passthrough(upper.as_bytes()), upper);
    }

    #[test]
    fn blake3_hex_or_passthrough_short_hex_string_gets_encoded() {
        let short = "abcd"; // 4 hex chars — too short for passthrough
        let out = blake3_hex_or_passthrough(short.as_bytes());
        assert_eq!(out, "61626364"); // hex of ASCII 'a','b','c','d'
    }

    #[test]
    fn blake3_hex_or_passthrough_64_byte_non_hex_gets_encoded() {
        let bytes = vec![0xABu8; 64];
        let out = blake3_hex_or_passthrough(&bytes);
        assert_eq!(out.len(), 128); // 64 bytes × 2 hex chars per byte
        assert!(out.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_open_creates_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sub/dir/cache.db");
        assert!(!path.exists());
        let _cache = EmbeddingCache::open(&path).unwrap();
        assert!(path.exists());
    }

    /// SEC-D.4: `EmbeddingCache::open` must restrict the DB file to 0o600.
    /// The fix replaced `let _ = set_permissions(...)` with an `if let Err`
    /// arm that emits a `tracing::warn!`. The happy-path test verifies the
    /// chmod actually applies — the warn arm is still reachable on a
    /// platform where the call fails (NFS, read-only mount, etc.) but
    /// can't be triggered in a portable test without root.
    #[test]
    #[cfg(unix)]
    fn embedding_cache_open_logs_chmod_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("perm_check.db");
        let _cache = EmbeddingCache::open(&path).unwrap();
        let perms = std::fs::metadata(&path).unwrap().permissions();
        // Mode bits low byte should be 0o600 = owner rw only.
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "EmbeddingCache DB file must be chmodded to 0o600"
        );
    }

    #[test]
    fn test_roundtrip() {
        let (cache, _dir) = test_cache();
        let emb = make_embedding(1024, 1.0);
        let entries = vec![("hash_a".to_string(), emb.clone())];
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 1024)
            .unwrap();

        let result = cache
            .read_batch(&["hash_a"], "fp_1", CachePurpose::Embedding, 1024)
            .unwrap();
        assert_eq!(result.len(), 1);
        let cached = &result["hash_a"];
        assert_eq!(cached.len(), 1024);
        assert!((cached[0] - emb[0]).abs() < 1e-6);
    }

    #[test]
    fn test_miss() {
        let (cache, _dir) = test_cache();
        let result = cache
            .read_batch(&["nonexistent"], "fp_1", CachePurpose::Embedding, 1024)
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_batch_write() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..100)
            .map(|i| (format!("hash_{i}"), make_embedding(768, i as f32)))
            .collect();
        let written = cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 768)
            .unwrap();
        assert_eq!(written, 100);

        let hashes: Vec<&str> = entries.iter().map(|(h, _)| h.as_str()).collect();
        let result = cache
            .read_batch(&hashes, "fp_1", CachePurpose::Embedding, 768)
            .unwrap();
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_different_fingerprints() {
        let (cache, _dir) = test_cache();
        let emb_a = make_embedding(1024, 1.0);
        let emb_b = make_embedding(1024, 2.0);

        cache
            .write_batch_owned(
                &[("hash_x".to_string(), emb_a.clone())],
                "fp_a",
                CachePurpose::Embedding,
                1024,
            )
            .unwrap();
        cache
            .write_batch_owned(
                &[("hash_x".to_string(), emb_b.clone())],
                "fp_b",
                CachePurpose::Embedding,
                1024,
            )
            .unwrap();

        let a = cache
            .read_batch(&["hash_x"], "fp_a", CachePurpose::Embedding, 1024)
            .unwrap();
        let b = cache
            .read_batch(&["hash_x"], "fp_b", CachePurpose::Embedding, 1024)
            .unwrap();

        assert!((a["hash_x"][0] - emb_a[0]).abs() < 1e-6);
        assert!((b["hash_x"][0] - emb_b[0]).abs() < 1e-6);
    }

    #[test]
    fn test_dim_mismatch() {
        let (cache, _dir) = test_cache();
        let emb = make_embedding(768, 1.0);
        cache
            .write_batch_owned(
                &[("hash_a".to_string(), emb)],
                "fp_1",
                CachePurpose::Embedding,
                768,
            )
            .unwrap();

        // Read with wrong expected dim — should miss
        let result = cache
            .read_batch(&["hash_a"], "fp_1", CachePurpose::Embedding, 1024)
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_zero_length_embedding() {
        let (cache, _dir) = test_cache();
        let entries = vec![("hash_a".to_string(), vec![])];
        let written = cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 0)
            .unwrap();
        assert_eq!(written, 0); // empty embeddings skipped
    }

    /// C.1 / OB-NEW-6: `QueryCache::get` builds a query preview for the
    /// "DB read failed" warn log via byte slicing. Raw `query.len().min(40)`
    /// panics when byte 40 lands inside a multi-byte codepoint (CJK, emoji,
    /// accented Latin). Use `floor_char_boundary(40)` so the preview always
    /// lands on a UTF-8 boundary and the soft DB error stays soft.
    #[test]
    fn query_preview_does_not_panic_on_multibyte_query() {
        // Construct a query that places multi-byte CJK chars + an emoji
        // straddling byte 40, so naive slicing would panic.
        let q = format!(
            "{}café 注釈 emoji 🎉 more text past forty bytes",
            "x".repeat(35)
        );
        assert!(q.len() > 40);
        // Exercise the exact slicing path used by QueryCache::get.
        let preview_len = q.floor_char_boundary(40);
        let preview = &q[..preview_len]; // must not panic
        assert!(preview.len() <= 40);
        // And the slice must itself be valid UTF-8 (it is, by construction).
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn test_clear() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..10)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 128)
            .unwrap();

        let deleted = cache.clear(None).unwrap();
        assert_eq!(deleted, 10);

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 0);
    }

    #[test]
    fn test_clear_by_model() {
        let (cache, _dir) = test_cache();
        cache
            .write_batch_owned(
                &[("h1".to_string(), make_embedding(128, 1.0))],
                "fp_a",
                CachePurpose::Embedding,
                128,
            )
            .unwrap();
        cache
            .write_batch_owned(
                &[("h2".to_string(), make_embedding(128, 2.0))],
                "fp_b",
                CachePurpose::Embedding,
                128,
            )
            .unwrap();

        cache.clear(Some("fp_a")).unwrap();

        let a = cache
            .read_batch(&["h1"], "fp_a", CachePurpose::Embedding, 128)
            .unwrap();
        let b = cache
            .read_batch(&["h2"], "fp_b", CachePurpose::Embedding, 128)
            .unwrap();
        assert!(a.is_empty()); // cleared
        assert_eq!(b.len(), 1); // survived
    }

    #[test]
    fn test_stats() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..5)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 128)
            .unwrap();

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 5);
        assert_eq!(stats.unique_models, 1);
        assert!(stats.newest_timestamp.is_some());
    }

    #[test]
    fn test_eviction() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("evict_test.db");

        // Create cache with tiny max size
        let rt = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .unwrap();
            sqlx::query("PRAGMA journal_mode=WAL")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    purpose TEXT NOT NULL DEFAULT 'embedding',
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint, purpose)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_cache_created ON embedding_cache (created_at)",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool
        });

        let cache = EmbeddingCache {
            pool,
            rt,
            max_size_bytes: 1, // 1 byte — everything should be evicted
            evict_lock: std::sync::Mutex::new(()),
        };

        let entries: Vec<_> = (0..10)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 128)
            .unwrap();

        let evicted = cache.evict().unwrap();
        assert!(evicted > 0, "Should have evicted entries");
    }

    #[test]
    fn test_corrupt_db_recovery() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("corrupt.db");

        // Write garbage to the file
        std::fs::write(&path, b"not a sqlite database").unwrap();

        // Opening should fail gracefully
        let result = EmbeddingCache::open(&path);
        // SQLite may or may not detect this as corruption depending on the bytes
        // The important thing is it doesn't panic
        if result.is_err() {
            // Expected — corrupt DB
        } else {
            // SQLite sometimes accepts random bytes and creates a new DB
            // That's fine too — the cache will just be empty
        }
    }

    // ===== TC-20: read_batch crosses 100-entry sub-batch boundary =====

    #[test]
    fn test_read_batch_crosses_100_boundary() {
        let (cache, _dir) = test_cache();

        // Write 150 entries — read_batch internally batches in groups of 100,
        // so this crosses the boundary.
        let entries: Vec<_> = (0..150)
            .map(|i| (format!("hash_{i:04}"), make_embedding(768, i as f32)))
            .collect();
        let written = cache
            .write_batch_owned(&entries, "fp_cross", CachePurpose::Embedding, 768)
            .unwrap();
        assert_eq!(written, 150);

        // Read all 150 back in one call
        let hashes: Vec<&str> = entries.iter().map(|(h, _)| h.as_str()).collect();
        let result = cache
            .read_batch(&hashes, "fp_cross", CachePurpose::Embedding, 768)
            .unwrap();
        assert_eq!(
            result.len(),
            150,
            "read_batch should return all 150 entries across the 100-entry sub-batch boundary"
        );

        // Verify a sample from each sub-batch (first, boundary, last)
        for idx in [0, 99, 100, 149] {
            let key = format!("hash_{idx:04}");
            assert!(
                result.contains_key(&key),
                "Missing key '{}' from read_batch results",
                key
            );
        }
    }

    // ===== TC-21: NaN embedding behavior (P2.27 — now rejected) =====

    #[test]
    fn test_nan_embedding_rejected() {
        let (cache, _dir) = test_cache();

        // Embeddings containing NaN/Inf are rejected by write_batch as of P2.27.
        // Cache poisoning across processes is the failure mode prevented here.
        let mut nan_emb = make_embedding(128, 1.0);
        nan_emb[0] = f32::NAN;
        nan_emb[64] = f32::NAN;

        let entries = vec![("hash_nan".to_string(), nan_emb)];
        let written = cache
            .write_batch_owned(&entries, "fp_nan", CachePurpose::Embedding, 128)
            .unwrap();
        assert_eq!(
            written, 0,
            "NaN entry should be filtered before persistence"
        );

        let result = cache
            .read_batch(&["hash_nan"], "fp_nan", CachePurpose::Embedding, 128)
            .unwrap();
        assert!(
            !result.contains_key("hash_nan"),
            "rejected NaN entry must not be readable"
        );

        // Sanity: same cache still accepts a clean entry.
        let clean = vec![("hash_clean".to_string(), make_embedding(128, 0.5))];
        let written_clean = cache
            .write_batch_owned(&clean, "fp_nan", CachePurpose::Embedding, 128)
            .unwrap();
        assert_eq!(written_clean, 1);
    }

    // ===== TC-24: prune edge cases =====

    #[test]
    fn test_prune_zero_days() {
        let (cache, _dir) = test_cache();

        // Write entries (they get current timestamp)
        let entries: Vec<_> = (0..5)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 128)
            .unwrap();

        // Prune with 0 days — cutoff is "now - 0 seconds" = now.
        // Entries written in the same second should survive (created_at >= cutoff).
        let pruned = cache.prune_older_than(0).unwrap();
        // Same-second entries: created_at == cutoff, and the query is `< cutoff`,
        // so they should NOT be pruned.
        assert_eq!(
            pruned, 0,
            "prune(0) should not delete entries written in the same second"
        );

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 5);
    }

    #[test]
    fn test_prune_large_days() {
        let (cache, _dir) = test_cache();

        // Write some entries
        let entries: Vec<_> = (0..3)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 128)
            .unwrap();

        // Prune with u32::MAX days — should not panic (overflow-safe).
        // cutoff = now - (u32::MAX as i64 * 86400) will go deeply negative,
        // so no entries should be pruned (all created_at > cutoff).
        let pruned = cache.prune_older_than(u32::MAX).unwrap();
        assert_eq!(
            pruned, 0,
            "prune(u32::MAX) should not delete any entries (cutoff is in the far past)"
        );

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 3);
    }

    // ===== TC-26: duplicate content_hash behavior =====

    #[test]
    fn test_write_batch_duplicate_hashes() {
        let (cache, _dir) = test_cache();

        let emb_a = make_embedding(128, 1.0);
        let emb_b = make_embedding(128, 2.0);

        // Two entries with the same content_hash but different embeddings
        let entries = vec![
            ("dup_hash".to_string(), emb_a.clone()),
            ("dup_hash".to_string(), emb_b.clone()),
        ];

        // write_batch uses INSERT OR IGNORE — the second insert is silently dropped.
        let written = cache
            .write_batch_owned(&entries, "fp_dup", CachePurpose::Embedding, 128)
            .unwrap();
        // Only 1 row should be written (second is ignored due to PK conflict)
        assert_eq!(
            written, 1,
            "Duplicate hash should be ignored by INSERT OR IGNORE"
        );

        // Read back — the first embedding (emb_a) should win
        let result = cache
            .read_batch(&["dup_hash"], "fp_dup", CachePurpose::Embedding, 128)
            .unwrap();
        assert_eq!(result.len(), 1);
        let cached = &result["dup_hash"];
        assert!(
            (cached[0] - emb_a[0]).abs() < 1e-6,
            "First embedding should win: expected {}, got {}",
            emb_a[0],
            cached[0]
        );
    }

    #[test]
    fn test_prune_by_age() {
        let (cache, _dir) = test_cache();

        // Insert with old timestamps by going through SQL directly
        cache.rt.block_on(async {
            let old_time = 1000i64; // way in the past
            for i in 0..5 {
                let blob: Vec<u8> = vec![0u8; 512]; // 128-dim * 4 bytes
                sqlx::query(
                    "INSERT INTO embedding_cache (content_hash, model_fingerprint, purpose, embedding, dim, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
                    .bind(format!("old_{i}"))
                    .bind("fp_1")
                    .bind("embedding")
                    .bind(&blob)
                    .bind(128i64)
                    .bind(old_time)
                    .execute(&cache.pool)
                    .await
                    .unwrap();
            }
        });

        // Insert fresh entries normally
        let entries: Vec<_> = (0..3)
            .map(|i| (format!("new_{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "fp_1", CachePurpose::Embedding, 128)
            .unwrap();

        // Prune entries older than 1 day — should remove the 5 old ones
        let pruned = cache.prune_older_than(1).unwrap();
        assert_eq!(pruned, 5);

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 3); // only fresh ones survive
    }

    // ─── New spec methods ───────────────────────────────────────────────

    /// `partition` splits items into hits + misses preserving order.
    #[test]
    fn test_partition_hits_and_misses() {
        let (cache, _dir) = test_cache();
        // Pre-populate two of the four hashes.
        let entries = vec![
            ("hash_a".to_string(), make_embedding(64, 1.0)),
            ("hash_c".to_string(), make_embedding(64, 3.0)),
        ];
        cache
            .write_batch_owned(&entries, "model_x", CachePurpose::Embedding, 64)
            .unwrap();

        let items: Vec<&str> = vec!["hash_a", "hash_b", "hash_c", "hash_d"];
        let (cached, missed) = cache
            .partition(
                &items,
                "model_x",
                CachePurpose::Embedding,
                64,
                |s: &&str| *s,
            )
            .unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(missed.len(), 2);
        let cached_hashes: Vec<&str> = cached.iter().map(|(s, _)| **s).collect();
        let missed_hashes: Vec<&str> = missed.iter().map(|s| **s).collect();
        assert_eq!(cached_hashes, vec!["hash_a", "hash_c"]);
        assert_eq!(missed_hashes, vec!["hash_b", "hash_d"]);
    }

    /// `partition` returns empty splits for empty input.
    #[test]
    fn test_partition_empty() {
        let (cache, _dir) = test_cache();
        let items: Vec<&str> = Vec::new();
        let (cached, missed) = cache
            .partition(
                &items,
                "model_x",
                CachePurpose::Embedding,
                64,
                |s: &&str| *s,
            )
            .unwrap();
        assert!(cached.is_empty());
        assert!(missed.is_empty());
    }

    /// `prune_by_model` only removes entries for the named model_id.
    #[test]
    fn test_prune_by_model_keeps_other_models() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..3)
            .map(|i| (format!("h{i}"), make_embedding(64, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "model_a", CachePurpose::Embedding, 64)
            .unwrap();
        cache
            .write_batch_owned(&entries, "model_b", CachePurpose::Embedding, 64)
            .unwrap();
        let removed = cache.prune_by_model("model_a").unwrap();
        assert_eq!(removed, 3);
        let stats_a = cache
            .read_batch(&["h0", "h1", "h2"], "model_a", CachePurpose::Embedding, 64)
            .unwrap();
        assert!(stats_a.is_empty());
        let stats_b = cache
            .read_batch(&["h0", "h1", "h2"], "model_b", CachePurpose::Embedding, 64)
            .unwrap();
        assert_eq!(stats_b.len(), 3);
    }

    /// `compact` shrinks the DB after a large delete.
    #[test]
    fn test_compact_after_delete_shrinks_file() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..200)
            .map(|i| (format!("hh{i}"), make_embedding(128, i as f32)))
            .collect();
        cache
            .write_batch_owned(&entries, "model_y", CachePurpose::Embedding, 128)
            .unwrap();
        let before = cache.stats().unwrap().total_size_bytes;

        // Delete everything.
        let _ = cache.clear(None).unwrap();
        // VACUUM
        cache.compact().unwrap();
        let after = cache.stats().unwrap().total_size_bytes;
        assert!(after <= before, "compact should not grow the DB");
    }

    /// `stats_per_model` reports per-model entries + bytes.
    #[test]
    fn test_stats_per_model_groups_correctly() {
        let (cache, _dir) = test_cache();
        let mk = |n: usize, dim: usize| -> Vec<_> {
            (0..n)
                .map(|i| (format!("h{i}"), make_embedding(dim, i as f32)))
                .collect()
        };
        cache
            .write_batch_owned(&mk(5, 64), "alpha", CachePurpose::Embedding, 64)
            .unwrap();
        cache
            .write_batch_owned(&mk(2, 64), "beta", CachePurpose::Embedding, 64)
            .unwrap();
        let per = cache.stats_per_model().unwrap();
        assert_eq!(per.len(), 2);
        // Order: COUNT(*) DESC — alpha first.
        assert_eq!(per[0].model_id, "alpha");
        assert_eq!(per[0].entries, 5);
        assert_eq!(per[1].model_id, "beta");
        assert_eq!(per[1].entries, 2);
    }

    /// `insert_many` groups by model_id and writes all entries.
    #[test]
    fn test_insert_many_grouped_by_model() {
        let (cache, _dir) = test_cache();
        let entries: Vec<(Vec<u8>, String, Vec<f32>)> = vec![
            (
                "ha".bytes().collect(),
                "modelA".to_string(),
                make_embedding(32, 1.0),
            ),
            (
                "hb".bytes().collect(),
                "modelB".to_string(),
                make_embedding(32, 2.0),
            ),
            (
                "hc".bytes().collect(),
                "modelA".to_string(),
                make_embedding(32, 3.0),
            ),
        ];
        let n = cache
            .insert_many(&entries, CachePurpose::Embedding, 32)
            .unwrap();
        assert_eq!(n, 3);
        let per = cache.stats_per_model().unwrap();
        let total: u64 = per.iter().map(|p| p.entries).sum();
        assert_eq!(total, 3);
    }

    /// `partition` reports a miss when the cached entry has a stale dim
    /// (different model dim than expected). Re-embed path is the caller's.
    #[test]
    fn test_partition_dim_mismatch_treated_as_miss() {
        let (cache, _dir) = test_cache();
        // Cache a 128-dim entry under model_z.
        let entries = vec![("hd".to_string(), make_embedding(128, 0.5))];
        cache
            .write_batch_owned(&entries, "model_z", CachePurpose::Embedding, 128)
            .unwrap();
        // Query for the same hash + model but expect dim=64 — should miss.
        let items = vec!["hd"];
        let (cached, missed) = cache
            .partition(
                &items,
                "model_z",
                CachePurpose::Embedding,
                64,
                |s: &&str| *s,
            )
            .unwrap();
        assert!(cached.is_empty());
        assert_eq!(missed.len(), 1);
    }

    /// Project default path resolves under the project's `.cqs/` dir.
    #[test]
    fn test_project_default_path() {
        let p = EmbeddingCache::project_default_path(Path::new("/proj/.cqs"));
        assert_eq!(p, Path::new("/proj/.cqs/embeddings_cache.db"));
    }

    /// `model_id` round-trips with HF revision suffix unchanged.
    #[test]
    fn test_model_id_roundtrip_preserves_hf_revision() {
        let (cache, _dir) = test_cache();
        let model_id = "BAAI/bge-large-en-v1.5@d4aa6901d3a41ba39fb536a557fa166f842b0e09";
        let entries = vec![("hh".to_string(), make_embedding(64, 0.0))];
        cache
            .write_batch_owned(&entries, model_id, CachePurpose::Embedding, 64)
            .unwrap();
        let result = cache
            .read_batch(&["hh"], model_id, CachePurpose::Embedding, 64)
            .unwrap();
        assert_eq!(result.len(), 1);
        // Sanity: a different revision suffix MUST miss.
        let other = "BAAI/bge-large-en-v1.5@aaaa1111";
        let result2 = cache
            .read_batch(&["hh"], other, CachePurpose::Embedding, 64)
            .unwrap();
        assert!(result2.is_empty());
    }

    // ─── #1128: purpose discriminator tests ──────────────────────────────

    /// Round-trip with each purpose independently — Embedding writes don't
    /// shadow EmbeddingBase reads and vice versa.
    #[test]
    fn test_purpose_round_trip_embedding_and_base_isolated() {
        let (cache, _dir) = test_cache();
        let emb_post = make_embedding(64, 1.0);
        let emb_base = make_embedding(64, 7.0);

        cache
            .write_batch_owned(
                &[("h_pp".to_string(), emb_post.clone())],
                "fp_dual",
                CachePurpose::Embedding,
                64,
            )
            .unwrap();
        cache
            .write_batch_owned(
                &[("h_pp".to_string(), emb_base.clone())],
                "fp_dual",
                CachePurpose::EmbeddingBase,
                64,
            )
            .unwrap();

        // Read back per-purpose; each must return its own vector.
        let r_post = cache
            .read_batch(&["h_pp"], "fp_dual", CachePurpose::Embedding, 64)
            .unwrap();
        let r_base = cache
            .read_batch(&["h_pp"], "fp_dual", CachePurpose::EmbeddingBase, 64)
            .unwrap();
        assert_eq!(r_post.len(), 1);
        assert_eq!(r_base.len(), 1);
        assert!(
            (r_post["h_pp"][0] - emb_post[0]).abs() < 1e-6,
            "Embedding read returned wrong vector (got {} want {})",
            r_post["h_pp"][0],
            emb_post[0]
        );
        assert!(
            (r_base["h_pp"][0] - emb_base[0]).abs() < 1e-6,
            "EmbeddingBase read returned wrong vector (got {} want {})",
            r_base["h_pp"][0],
            emb_base[0]
        );

        // Two distinct rows under the same (hash, model_fingerprint) — total
        // entry count must be 2.
        let stats = cache.stats().unwrap();
        assert_eq!(
            stats.total_entries, 2,
            "PK collision: same hash+model+different purpose must yield two rows"
        );
    }

    /// Writes for one purpose must not be read out under the other purpose
    /// — silent overwrite was the pre-#1128 bug.
    #[test]
    fn test_purpose_isolation_no_cross_purpose_leak() {
        let (cache, _dir) = test_cache();
        let emb = make_embedding(64, 0.5);
        cache
            .write_batch_owned(
                &[("h_iso".to_string(), emb)],
                "fp_iso",
                CachePurpose::EmbeddingBase,
                64,
            )
            .unwrap();
        // Read with the other purpose — must miss.
        let result = cache
            .read_batch(&["h_iso"], "fp_iso", CachePurpose::Embedding, 64)
            .unwrap();
        assert!(
            result.is_empty(),
            "EmbeddingBase row leaked into Embedding read (cache silently returns wrong vector)"
        );
    }

    /// Migration: opening a cache created under the legacy schema (no
    /// `purpose` column) must keep existing rows readable. Pre-#1128 caches
    /// hold only `purpose='embedding'` rows by construction (the only
    /// producer was the post-enrichment vector).
    #[test]
    fn test_migration_legacy_schema_rows_readable_as_embedding() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("legacy_cache.db");

        // Create a legacy cache (pre-#1128 schema, no `purpose` column).
        let rt = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&path)
                        .create_if_missing(true)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
                )
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            // Insert a legacy row directly.
            let blob: Vec<u8> = (0..64u32)
                .flat_map(|i| (i as f32).to_le_bytes())
                .collect::<Vec<u8>>();
            sqlx::query(
                "INSERT INTO embedding_cache \
                 (content_hash, model_fingerprint, embedding, dim, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind("legacy_hash")
            .bind("legacy_fp")
            .bind(&blob)
            .bind(64i64)
            .bind(0i64)
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        });

        // Open it through the modern path — migration adds the column and
        // existing rows default to `purpose = 'embedding'`.
        let cache = EmbeddingCache::open(&path).unwrap();
        let result = cache
            .read_batch(&["legacy_hash"], "legacy_fp", CachePurpose::Embedding, 64)
            .unwrap();
        assert_eq!(
            result.len(),
            1,
            "legacy row should be readable as Embedding after migration"
        );
        // Reading as the other purpose must miss — the legacy row is
        // unambiguously 'embedding', not 'embedding_base'.
        let result_base = cache
            .read_batch(
                &["legacy_hash"],
                "legacy_fp",
                CachePurpose::EmbeddingBase,
                64,
            )
            .unwrap();
        assert!(
            result_base.is_empty(),
            "legacy row must not satisfy EmbeddingBase reads"
        );
    }

    /// `CachePurpose::as_str` is the wire format for the `purpose` column —
    /// pin the strings so a future enum variant rename doesn't silently
    /// invalidate every existing cache row.
    #[test]
    fn test_cache_purpose_as_str_stable() {
        assert_eq!(CachePurpose::Embedding.as_str(), "embedding");
        assert_eq!(CachePurpose::EmbeddingBase.as_str(), "embedding_base");
        assert_eq!(CachePurpose::default(), CachePurpose::Embedding);
    }

    /// After migration, an EmbeddingBase write with a hash that already
    /// exists as Embedding must succeed. Pre-#1128's "ADD COLUMN only" half-
    /// migration leaves the legacy `(content_hash, model_fingerprint)` PK in
    /// force and INSERT OR IGNORE would silently drop the EmbeddingBase row.
    /// The 12-step rebuild migration (rename → CREATE → INSERT SELECT → DROP)
    /// applies the new 3-column PK so both purposes coexist.
    #[test]
    fn test_migration_legacy_schema_accepts_embedding_base_after_rebuild() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("legacy_pk_check.db");

        // Build the legacy schema directly so we exercise the migration
        // path even on a fresh tempdir.
        let rt = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&path)
                        .create_if_missing(true)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
                )
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            // Seed with a single 'embedding'-purpose row.
            let blob: Vec<u8> = (0..8u32)
                .flat_map(|i| (i as f32).to_le_bytes())
                .collect::<Vec<u8>>();
            sqlx::query(
                "INSERT INTO embedding_cache \
                 (content_hash, model_fingerprint, embedding, dim, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind("colliding_hash")
            .bind("legacy_fp")
            .bind(&blob)
            .bind(8i64)
            .bind(0i64)
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        });

        let cache = EmbeddingCache::open(&path).unwrap();

        // Sanity: legacy row migrated and is readable as Embedding.
        let r1 = cache
            .read_batch(&["colliding_hash"], "legacy_fp", CachePurpose::Embedding, 8)
            .unwrap();
        assert_eq!(r1.len(), 1, "migrated legacy row must survive");

        // Now write the SAME (hash, model_fingerprint) under EmbeddingBase
        // — this is the case the half-migration would silently reject. With
        // the rebuilt PK, this row gets accepted as a separate entry.
        let base_emb = vec![0.5_f32; 8];
        let written = cache
            .write_batch_owned(
                &[("colliding_hash".to_string(), base_emb.clone())],
                "legacy_fp",
                CachePurpose::EmbeddingBase,
                8,
            )
            .unwrap();
        assert_eq!(
            written, 1,
            "EmbeddingBase write must succeed under post-migration PK"
        );

        // Both rows present.
        let r2 = cache
            .read_batch(
                &["colliding_hash"],
                "legacy_fp",
                CachePurpose::EmbeddingBase,
                8,
            )
            .unwrap();
        assert_eq!(r2.len(), 1);
        assert!((r2["colliding_hash"][0] - base_emb[0]).abs() < 1e-6);
        assert_eq!(cache.stats().unwrap().total_entries, 2);
    }
}

// ─── Query Cache ────────────────────────────────────────────────────────────

/// Persistent query embedding cache backed by SQLite.
///
/// Stores `(query_text, model_fingerprint) → embedding` on disk so that
/// repeated queries across CLI invocations don't re-run ONNX inference.
/// Best-effort: all failures are logged and silently skipped.
pub struct QueryCache {
    pool: sqlx::SqlitePool,
    /// #968: `Arc<Runtime>` so callers (e.g. the daemon) can share one
    /// runtime across `Store`, `EmbeddingCache`, and `QueryCache`. When
    /// no runtime is supplied `open` constructs its own.
    rt: Arc<tokio::runtime::Runtime>,
    /// P3 #124: max size cap. Honours `CQS_QUERY_CACHE_MAX_SIZE` (bytes,
    /// default 100 MB). Read at `open` time and used by [`Self::evict`] —
    /// no resize support, daemon restart picks up env changes.
    max_size_bytes: u64,
    /// DS2-5: serializes `evict()` calls so two parallel invocations can't
    /// both measure the same size and issue overlapping `LIMIT ?` DELETEs.
    /// Size/AVG/DELETE inside `evict()` are wrapped in a single transaction.
    evict_lock: std::sync::Mutex<()>,
}

impl QueryCache {
    /// Default cache location (same directory as the embedding cache).
    ///
    /// Uses the platform's native cache dir — see [`EmbeddingCache::default_path`]
    /// for the resolution order (`dirs::cache_dir()` → `~/.cache` → `.`).
    pub fn default_path() -> std::path::PathBuf {
        // P3.32: native platform cache dir.
        dirs::cache_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("cqs")
            .join("query_cache.db")
    }

    /// Open or create the query cache.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        Self::open_with_runtime(path, None)
    }

    /// Open with a pre-existing runtime (saves ~15ms by avoiding runtime
    /// creation and, per #968, lets the daemon share one runtime across
    /// `Store`, `EmbeddingCache`, and `QueryCache`).
    pub fn open_with_runtime(
        path: &Path,
        runtime: Option<Arc<tokio::runtime::Runtime>>,
    ) -> Result<Self, CacheError> {
        let _span = tracing::info_span!("query_cache_open", path = %path.display()).entered();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // PB-V1.29-7: best-effort parent chmod (same rationale as in
                // `EmbeddingCache::open`). Log and continue on failure rather
                // than refusing to open the query cache.
                if let Err(e) =
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                {
                    tracing::warn!(
                        path = %parent.display(),
                        error = %e,
                        "Failed to set query cache parent dir permissions to 0o700"
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

        // SHL-V1.25-12: query cache honours CQS_BUSY_TIMEOUT_MS too. Default
        // here is 2s because the query cache is write-lighter than the
        // embedding cache — still tunable by the global env knob.
        let connect_opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(busy_timeout_from_env(2000))
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .idle_timeout(std::time::Duration::from_secs(30))
                .connect_with(connect_opts)
                .await?;

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS query_cache (
                    query TEXT NOT NULL,
                    model_fp TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    ts INTEGER NOT NULL DEFAULT (unixepoch()),
                    PRIMARY KEY (query, model_fp)
                )",
            )
            .execute(&pool)
            .await?;

            Ok::<_, sqlx::Error>(pool)
        })?;

        // SEC-V1.25-4: restrict DB + WAL/SHM sidecar files to 0o600 to match
        // `EmbeddingCache::open`. Query text may be sensitive (user prompts,
        // internal tooling queries), and multi-user boxes must not leave this
        // world-readable. P2.3: shared with `EmbeddingCache` via `apply_db_file_perms`.
        apply_db_file_perms(path);

        // P3 #124: surface cap from env, default 100 MB. Disk-only — no per-row
        // accounting because the cache may persist across daemon restarts.
        let max_size_bytes = std::env::var("CQS_QUERY_CACHE_MAX_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &u64| n > 0)
            .unwrap_or(100 * 1024 * 1024);

        tracing::debug!(path = %path.display(), max_size_bytes, "Query cache opened");
        Ok(Self {
            pool,
            rt,
            max_size_bytes,
            evict_lock: std::sync::Mutex::new(()),
        })
    }

    /// Evict the oldest entries until the cache fits within
    /// `CQS_QUERY_CACHE_MAX_SIZE` (default 100 MB). Best-effort — sqlite
    /// errors are logged and reported as `Ok(0)`.
    ///
    /// P3 #124: mirrors [`EmbeddingCache::evict`]. Run from the same daemon
    /// periodic-eviction tick so disk usage stays bounded across long
    /// sessions.
    ///
    /// DS2-5: size / AVG / DELETE run in a single `pool.begin()` transaction
    /// so concurrent `put()` traffic cannot invalidate the measurement
    /// between steps. `evict_lock` serializes parallel evict callers.
    pub fn evict(&self) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("query_cache_evict").entered();

        let _guard = self
            .evict_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        self.rt.block_on(async {
            let mut tx = match self.pool.begin().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache evict begin-tx failed");
                    return Ok(0);
                }
            };

            // Same logical-data measure as `EmbeddingCache::evict` (data + per-row
            // overhead). Page-count would over-report after deletions because the
            // SQLite file doesn't shrink without VACUUM.
            let size: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(SUM(LENGTH(embedding)), 0) + COUNT(*) * 200 FROM query_cache",
            )
            .fetch_one(&mut *tx)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache evict size query failed");
                    return Ok(0);
                }
            };

            if size <= 0 || (size as u64) <= self.max_size_bytes {
                return Ok(0);
            }

            let excess = size as u64 - self.max_size_bytes;
            let avg_entry: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(AVG(LENGTH(embedding) + 200), 4200) FROM query_cache",
            )
            .fetch_one(&mut *tx)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache evict avg-entry query failed, using default");
                    4200
                }
            };
            let entries_to_delete = (excess / avg_entry.max(1) as u64).max(1);

            let result = sqlx::query(
                "DELETE FROM query_cache WHERE rowid IN \
                 (SELECT rowid FROM query_cache ORDER BY ts ASC LIMIT ?1)",
            )
            .bind(entries_to_delete as i64)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;

            let evicted = result.rows_affected() as usize;
            tracing::info!(evicted, "Query cache eviction complete");
            Ok(evicted)
        })
    }

    /// Logical size of the cache in bytes (sum of embedding blobs + row overhead).
    /// Used by `cqs cache stats --json` to surface query-cache size alongside
    /// the embedding cache (P3 #124).
    pub fn size_bytes(&self) -> Result<u64, CacheError> {
        self.rt.block_on(async {
            let size: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(LENGTH(embedding)), 0) + COUNT(*) * 200 FROM query_cache",
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(size.max(0) as u64)
        })
    }

    /// Look up a cached query embedding.
    pub fn get(&self, query: &str, model_fp: &str) -> Option<crate::embedder::Embedding> {
        self.rt.block_on(async {
            // EH-17 / OB-NEW-6: log sqlite failures instead of treating them
            // as a silent cache miss. A corrupted / locked cache is a real
            // signal, not background noise.
            let row: Option<(Vec<u8>,)> = match sqlx::query_as(
                "SELECT embedding FROM query_cache WHERE query = ?1 AND model_fp = ?2",
            )
            .bind(query)
            .bind(model_fp)
            .fetch_optional(&self.pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    // OB-NEW-6 / robustness: `query.len().min(40)` panics if
                    // byte 40 lands inside a multi-byte codepoint. Use
                    // `floor_char_boundary` so non-ASCII queries don't
                    // convert a soft DB-error log into hard process death.
                    let preview_len = query.floor_char_boundary(40);
                    tracing::warn!(
                        query_preview = %&query[..preview_len],
                        error = %e,
                        "query cache read failed"
                    );
                    return None;
                }
            };

            let (bytes,) = row?;
            // EH-17 / OB-NEW-6: a malformed embedding blob (length not a
            // multiple of 4) means the row is corrupt. Don't let it sit in
            // the DB forever — log it and delete so future reads skip the
            // cost of re-checking the same bad row.
            if bytes.len() % std::mem::size_of::<f32>() != 0 {
                tracing::warn!(
                    raw_len = bytes.len(),
                    "query cache entry has malformed embedding blob; deleting"
                );
                if let Err(e) =
                    sqlx::query("DELETE FROM query_cache WHERE query = ?1 AND model_fp = ?2")
                        .bind(query)
                        .bind(model_fp)
                        .execute(&self.pool)
                        .await
                {
                    tracing::warn!(error = %e, "failed to delete malformed query cache row");
                }
                return None;
            }
            let floats: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Some(crate::embedder::Embedding::new(floats))
        })
    }

    /// Store a query embedding (write-through).
    pub fn put(&self, query: &str, model_fp: &str, embedding: &crate::embedder::Embedding) {
        // P2.27: reject non-finite values so cached query embeddings can't
        // poison downstream cosine math (NaN propagates through scoring).
        if embedding.as_slice().iter().any(|f| !f.is_finite()) {
            tracing::warn!(
                query_len = query.len(),
                "Skipping query cache write: embedding contains NaN or Inf"
            );
            return;
        }
        let bytes: Vec<u8> = embedding
            .as_slice()
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        if let Err(e) = self.rt.block_on(async {
            sqlx::query(
                "INSERT OR REPLACE INTO query_cache (query, model_fp, embedding, ts)
                 VALUES (?1, ?2, ?3, unixepoch())",
            )
            .bind(query)
            .bind(model_fp)
            .bind(&bytes)
            .execute(&self.pool)
            .await
        }) {
            // EH-17 / OB-NEW-6: write failures on the query cache are
            // corruption / disk-full risks, not noise. Promote from debug!
            // to warn! so operators see them in default logs.
            tracing::warn!(error = %e, "Query cache write failed (non-fatal)");
        }
    }

    /// Prune entries older than `days` days. Returns count deleted.
    pub fn prune_older_than(&self, days: u32) -> Result<u64, CacheError> {
        let rows = self.rt.block_on(async {
            let result = sqlx::query("DELETE FROM query_cache WHERE ts < unixepoch() - ?1 * 86400")
                .bind(days)
                .execute(&self.pool)
                .await?;
            Ok::<_, sqlx::Error>(result.rows_affected())
        })?;
        if rows > 0 {
            tracing::info!(pruned = rows, days, "Query cache pruned");
        }
        Ok(rows)
    }
}

impl Drop for QueryCache {
    /// RM-V1.29-7: mirror `Store::drop`. Best-effort `PRAGMA wal_checkpoint(TRUNCATE)`
    /// on daemon shutdown so the query cache's `-wal` sidecar doesn't
    /// accumulate over long-running daemon sessions. `catch_unwind` guards
    /// against `block_on` panicking when dropped from inside a tokio runtime.
    fn drop(&mut self) {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(e) = self.rt.block_on(async {
                sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                    .execute(&self.pool)
                    .await
            }) {
                tracing::warn!(
                    error = %e,
                    "QueryCache WAL checkpoint on drop failed (non-fatal)"
                );
            }
        })) {
            let msg = crate::panic_message(&payload);
            tracing::warn!(
                panic = %msg,
                "WAL checkpoint panic caught in QueryCache::drop (non-fatal)"
            );
        }
    }
}

// ─── TC-ADV-V1.33-6: QueryCache malformed-blob auto-delete ──────────────────
//
// EH-17 / OB-NEW-6: a malformed embedding blob (length not a multiple of
// 4) is logged + deleted on `QueryCache::get`. Pins the auto-delete
// behaviour so a re-poll loop on bad rows can't reintroduce the cost of
// re-checking the same bad row.
#[cfg(test)]
mod query_cache_malformed_blob_tests {
    use super::*;

    /// TC-ADV-V1.33-6: insert a row whose embedding blob length isn't a
    /// multiple of 4 (corrupt row, schema migration mid-flight, manual SQL
    /// stomp). `get` returns `None` and deletes the row.
    #[test]
    fn test_query_cache_get_deletes_malformed_blob() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("query_cache.db");
        let cache = QueryCache::open(&path).unwrap();

        // Reach into the runtime to insert a malformed (7-byte) blob
        // directly via raw sqlx — bypassing `put` which would reject
        // non-finite floats but still requires a real Embedding.
        cache.rt.block_on(async {
            sqlx::query(
                "INSERT INTO query_cache (query, model_fp, embedding) \
                 VALUES (?1, ?2, ?3)",
            )
            .bind("malformed-q")
            .bind("test-fp")
            .bind(vec![0u8; 7]) // 7 bytes — not a multiple of 4
            .execute(&cache.pool)
            .await
            .unwrap();
        });

        // First get must return None (malformed blob detected, deleted).
        let got = cache.get("malformed-q", "test-fp");
        assert!(
            got.is_none(),
            "malformed blob must produce a cache miss, got Some(_)"
        );

        // Verify the row was actually deleted (next `get` would also miss
        // even without the malformed-blob path firing again).
        let row_count: i64 = cache.rt.block_on(async {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM query_cache WHERE query = ?1 AND model_fp = ?2",
            )
            .bind("malformed-q")
            .bind("test-fp")
            .fetch_one(&cache.pool)
            .await
            .unwrap()
        });
        assert_eq!(
            row_count, 0,
            "malformed row must be deleted after get, found {row_count} row(s)"
        );
    }
}

// ─── #968 shared-runtime integration tests ──────────────────────────────────
//
// Confirms that one `Arc<Runtime>` can drive Store + EmbeddingCache +
// QueryCache simultaneously, as the daemon does, and that the runtime
// drops cleanly once every consumer is released.
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

    /// #968 core invariant: the daemon can build one `Arc<Runtime>` and hand
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

        // Five live Arcs: shared_rt + Store + Store's summary_queue (#1126)
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

    /// #968: `Store::open_readonly_pooled_with_runtime` — the pre-968
    /// template path — keeps working under the new `Arc<Runtime>`
    /// signature. Guards against the template drifting from
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
