//! Persistent query embedding cache backed by SQLite.
//!
//! `QueryCache`, split out of the former monolithic `cache.rs` (issue #1691).
//! Shared types and the process-global evict lock live in the parent module
//! and are pulled in via `use super::*`.

use super::*;
use std::path::Path;
use std::sync::Arc;

// ─── Query Cache ────────────────────────────────────────────────────────────

/// Persistent query embedding cache backed by SQLite.
///
/// Stores `(query_text, model_fingerprint) → embedding` on disk so that
/// repeated queries across CLI invocations don't re-run ONNX inference.
/// Best-effort: all failures are logged and silently skipped.
pub struct QueryCache {
    pool: sqlx::SqlitePool,
    /// `Arc<Runtime>` so callers (e.g. the daemon) can share one runtime
    /// across `Store`, `EmbeddingCache`, and `QueryCache`. When no runtime is
    /// supplied `open` constructs its own.
    rt: Arc<tokio::runtime::Runtime>,
    /// Max size cap. Honours `CQS_QUERY_CACHE_MAX_SIZE` (bytes, default
    /// 100 MB). Read at `open` time and used by [`Self::evict`] — no resize
    /// support, daemon restart picks up env changes.
    max_size_bytes: u64,
    // Evict serialization uses the module-level `QUERY_CACHE_EVICT_LOCK`
    // static so multiple opens in one process share the mutex.
}

impl QueryCache {
    /// Default cache location (same directory as the embedding cache).
    ///
    /// Uses the platform's native cache dir — see [`EmbeddingCache::default_path`]
    /// for the resolution order (`dirs::cache_dir()` → `~/.cache` → `.`).
    pub fn default_path() -> std::path::PathBuf {
        // Native platform cache dir.
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
    /// creation and lets the daemon share one runtime across `Store`,
    /// `EmbeddingCache`, and `QueryCache`).
    pub fn open_with_runtime(
        path: &Path,
        runtime: Option<Arc<tokio::runtime::Runtime>>,
    ) -> Result<Self, CacheError> {
        let _span = tracing::info_span!("query_cache_open", path = %path.display()).entered();

        // Pool-open skeleton shared with `EmbeddingCache` via
        // `connect_cache_pool` (the umask wrap matters here too: query text
        // may be sensitive — user prompts, internal tooling queries — so the
        // DB is born private). Default busy timeout 15000 ms — same
        // WAL-checkpoint contention class as the embedding cache, halved
        // because the query cache is write-lighter.
        let (pool, rt) = connect_cache_pool(path, 15_000, runtime, |pool| async move {
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

            Ok(())
        })?;

        // Surface cap from env, default 100 MB. Disk-only — no per-row
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
        })
    }

    /// Evict the oldest entries until the cache fits within
    /// `CQS_QUERY_CACHE_MAX_SIZE` (default 100 MB). Best-effort — sqlite
    /// errors are logged and reported as `Ok(0)`.
    ///
    /// Mirrors [`EmbeddingCache::evict`]. Run from the same daemon
    /// periodic-eviction tick so disk usage stays bounded across long
    /// sessions.
    ///
    /// Size / AVG / DELETE run in a single transaction so concurrent `put()`
    /// traffic cannot invalidate the measurement between steps.
    /// `QUERY_CACHE_EVICT_LOCK` (process-global) serializes parallel evict
    /// callers across all `QueryCache` handles in this process.
    ///
    /// The transaction is opened with `BEGIN IMMEDIATE` so a peer process
    /// running its own evict can't race us via deferred snapshots. See
    /// `EmbeddingCache::evict` for the full rationale.
    pub fn evict(&self) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("query_cache_evict").entered();

        // Process-global static — see QUERY_CACHE_EVICT_LOCK docs.
        let _guard = QUERY_CACHE_EVICT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        self.rt.block_on(async {
            // Same connection-held BEGIN IMMEDIATE pattern as
            // `EmbeddingCache::evict`. Implicit ROLLBACK on connection return
            // keeps early `return Ok(0)` paths safe.
            let mut conn = match self.pool.acquire().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache evict acquire failed");
                    return Ok(0);
                }
            };
            if let Err(e) = sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *conn)
                .await
            {
                tracing::warn!(error = %e, "Query cache evict BEGIN IMMEDIATE failed");
                return Ok(0);
            }

            // Same logical-data measure as `EmbeddingCache::evict` (data + per-row
            // overhead). Page-count would over-report after deletions because the
            // SQLite file doesn't shrink without VACUUM.
            let size: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(SUM(LENGTH(embedding)), 0) + COUNT(*) * 200 FROM query_cache",
            )
            .fetch_one(&mut *conn)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache evict size query failed");
                    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                    return Ok(0);
                }
            };

            if size <= 0 || (size as u64) <= self.max_size_bytes {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Ok(0);
            }

            let excess = size as u64 - self.max_size_bytes;
            let avg_entry: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(AVG(LENGTH(embedding) + 200), 4200) FROM query_cache",
            )
            .fetch_one(&mut *conn)
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
            .execute(&mut *conn)
            .await?;

            sqlx::query("COMMIT").execute(&mut *conn).await?;

            let evicted = result.rows_affected() as usize;
            tracing::info!(evicted, "Query cache eviction complete");
            Ok(evicted)
        })
    }

    /// Logical size of the cache in bytes (sum of embedding blobs + row overhead).
    /// Used by `cqs cache stats --json` to surface query-cache size alongside
    /// the embedding cache.
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
            // Log sqlite failures instead of treating them as a silent cache
            // miss. A corrupted / locked cache is a real signal, not noise.
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
                    // `query.len().min(40)` would panic if byte 40 lands
                    // inside a multi-byte codepoint. `floor_char_boundary`
                    // keeps non-ASCII queries from turning a soft DB-error log
                    // into hard process death.
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
            // A malformed embedding blob (length not a multiple of 4) means
            // the row is corrupt. Log and delete so future reads skip the cost
            // of re-checking the same bad row.
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
            // See `read_batch` above for the zero-copy cast rationale. Same
            // producer/consumer invariants apply here.
            let floats: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&bytes).to_vec();
            Some(crate::embedder::Embedding::new(floats))
        })
    }

    /// Store a query embedding (write-through).
    pub fn put(&self, query: &str, model_fp: &str, embedding: &crate::embedder::Embedding) {
        // Reject non-finite values so cached query embeddings can't poison
        // downstream cosine math (NaN propagates through scoring).
        if embedding.as_slice().iter().any(|f| !f.is_finite()) {
            tracing::warn!(
                query_len = query.len(),
                "Skipping query cache write: embedding contains NaN or Inf"
            );
            return;
        }
        // bytemuck zero-copy encode.
        let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(embedding.as_slice()).to_vec();
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
            // Write failures on the query cache are corruption / disk-full
            // risks, not noise — warn so operators see them in default logs.
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

impl QueryCache {
    /// Run a blocking `PRAGMA wal_checkpoint(TRUNCATE)` — see
    /// [`EmbeddingCache::checkpoint_wal`] for the contract. Structured shutdown
    /// paths call this before drop; drop falls back to a 1 s PASSIVE checkpoint.
    pub fn checkpoint_wal(&self) -> Result<(), CacheError> {
        let _span = tracing::info_span!("query_cache_checkpoint_wal_truncate").entered();
        self.rt.block_on(async {
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }
}

impl Drop for QueryCache {
    /// See [`EmbeddingCache::drop`] for the rationale. Best-effort
    /// `PRAGMA wal_checkpoint(PASSIVE)` with a 1 s timeout — caps
    /// daemon-shutdown WAL-copy stalls. Explicit truncate via
    /// [`QueryCache::checkpoint_wal`].
    fn drop(&mut self) {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let res = self.rt.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    sqlx::query("PRAGMA wal_checkpoint(PASSIVE)").execute(&self.pool),
                )
                .await
            });
            match res {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(
                    error = %e,
                    "QueryCache WAL checkpoint(PASSIVE) on drop failed (non-fatal)"
                ),
                Err(_) => tracing::warn!(
                    "QueryCache WAL checkpoint(PASSIVE) timed out after 1s on drop; \
                     WAL will be replayed on next open. Call checkpoint_wal() before \
                     drop in shutdown paths to truncate."
                ),
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

// ─── QueryCache malformed-blob auto-delete ──────────────────────────────────
//
// A malformed embedding blob (length not a multiple of 4) is logged +
// deleted on `QueryCache::get`. Pins the auto-delete behaviour so a re-poll
// loop on bad rows can't reintroduce the cost of re-checking the same bad row.
#[cfg(test)]
mod query_cache_malformed_blob_tests {
    use super::*;

    /// Insert a row whose embedding blob length isn't a multiple of 4 (corrupt
    /// row, schema migration mid-flight, manual SQL stomp). `get` returns
    /// `None` and deletes the row.
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

    /// Same as the EmbeddingCache test: every connection from the query-cache
    /// pool must carry a finite wal_autocheckpoint ceiling so an abrupt
    /// shutdown doesn't leave an unbounded WAL tail.
    #[test]
    fn query_cache_wal_autocheckpoint_pragma_is_applied_after_connect() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("query_cache.db");
        let cache = QueryCache::open(&path).unwrap();
        let pages: i64 = cache
            .rt
            .block_on(async {
                sqlx::query_scalar::<_, i64>("PRAGMA wal_autocheckpoint")
                    .fetch_one(&cache.pool)
                    .await
            })
            .expect("PRAGMA wal_autocheckpoint should succeed on a fresh QueryCache");
        assert_eq!(
            pages, 1000,
            "expected default wal_autocheckpoint=1000 from `wal_autocheckpoint_pragma()`"
        );
    }
}

// ─── shared-runtime integration tests ───────────────────────────────────────
//
// Confirms that one `Arc<Runtime>` can drive Store + EmbeddingCache +
// QueryCache simultaneously, as the daemon does, and that the runtime
