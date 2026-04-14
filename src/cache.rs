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

use thiserror::Error;

use crate::store::helpers::sql::max_rows_per_statement;

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("Cache database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Cache I/O error: {0}")]
    Io(#[from] std::io::Error),
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

/// Global embedding cache backed by SQLite.
///
/// Best-effort: all operations that fail are logged and skipped.
/// The index pipeline works identically with or without a functioning cache.
pub struct EmbeddingCache {
    pool: sqlx::SqlitePool,
    rt: tokio::runtime::Runtime,
    max_size_bytes: u64,
}

impl EmbeddingCache {
    /// Default cache location.
    pub fn default_path() -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".cache/cqs/embeddings.db")
    }

    /// Open or create the embedding cache.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        Self::open_with_runtime(path, None)
    }

    /// Open with a pre-existing runtime (saves ~15ms by avoiding runtime creation).
    pub fn open_with_runtime(
        path: &Path,
        runtime: Option<tokio::runtime::Runtime>,
    ) -> Result<Self, CacheError> {
        let _span = tracing::info_span!("embedding_cache_open", path = %path.display()).entered();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }

        let rt = if let Some(rt) = runtime {
            rt
        } else {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| CacheError::Io(std::io::Error::other(e)))?
        };

        // Use SqliteConnectOptions to avoid URL-encoding issues with special paths
        let connect_opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1) // RM-2: single worker thread can only use 1 connection
                .idle_timeout(std::time::Duration::from_secs(30)) // RM-5: release idle connections
                .connect_with(connect_opts)
                .await?;

            // Create table if not exists
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS embedding_cache (
                    content_hash TEXT NOT NULL,
                    model_fingerprint TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint)
                )",
            )
            .execute(&pool)
            .await?;

            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_cache_created ON embedding_cache (created_at)",
            )
            .execute(&pool)
            .await?;

            Ok::<_, sqlx::Error>(pool)
        })?;

        // Restrict DB file permissions (cache contains embedding data, not secrets,
        // but no reason for world-readable)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            for suffix in &["", "-wal", "-shm"] {
                let db_file = path.with_extension(
                    path.extension()
                        .map(|e| format!("{}{}", e.to_string_lossy(), suffix))
                        .unwrap_or_else(|| suffix.trim_start_matches('-').to_string()),
                );
                if db_file.exists() {
                    let _ = std::fs::set_permissions(&db_file, perms.clone());
                }
            }
        }

        let max_size_bytes = std::env::var("CQS_CACHE_MAX_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10 * 1024 * 1024 * 1024); // 10GB default

        tracing::info!("Embedding cache opened");

        Ok(Self {
            pool,
            rt,
            max_size_bytes,
        })
    }

    /// Read cached embeddings for a batch of content hashes.
    /// Returns a map of content_hash → embedding (as Vec<f32>).
    /// Cache misses are simply absent from the map.
    pub fn read_batch(
        &self,
        content_hashes: &[&str],
        model_fingerprint: &str,
        expected_dim: usize,
    ) -> Result<HashMap<String, Vec<f32>>, CacheError> {
        let _span = tracing::debug_span!(
            "cache_read_batch",
            count = content_hashes.len(),
            fingerprint = &model_fingerprint[..8.min(model_fingerprint.len())]
        )
        .entered();

        if content_hashes.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result = HashMap::new();

            // SHL-V1.25-4: Batch size matches modern SQLite variable limit
            // (32766). Two vars per row accounts for the shared model_fingerprint
            // bind plus the content_hash bind, with headroom for either being
            // added to in the future. Cache hit lookups for a 50k-chunk index
            // now fire 2-3 SELECTs instead of 500.
            for batch in content_hashes.chunks(max_rows_per_statement(2)) {
                let placeholders: Vec<String> =
                    (0..batch.len()).map(|i| format!("?{}", i + 2)).collect();
                let sql = format!(
                    "SELECT content_hash, embedding, dim FROM embedding_cache \
                     WHERE model_fingerprint = ?1 AND content_hash IN ({})",
                    placeholders.join(",")
                );

                let mut query = sqlx::query(&sql).bind(model_fingerprint);
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

    /// Write a batch of embeddings to the cache.
    /// Best-effort: returns the number written, errors are logged.
    pub fn write_batch(
        &self,
        entries: &[(String, Vec<f32>)],
        model_fingerprint: &str,
        dim: usize,
    ) -> Result<usize, CacheError> {
        let _span = tracing::debug_span!(
            "cache_write_batch",
            count = entries.len(),
            fingerprint = &model_fingerprint[..8.min(model_fingerprint.len())]
        )
        .entered();

        if entries.is_empty() {
            return Ok(0);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;
            let mut written = 0usize;
            let mut blob = Vec::with_capacity(dim * 4); // PF-6: reuse scratch buffer

            for (content_hash, embedding) in entries {
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

                // Encode Vec<f32> to blob (PF-6: reuse buffer)
                blob.clear();
                blob.extend(embedding.iter().flat_map(|f| f.to_le_bytes()));

                let result = sqlx::query(
                    "INSERT OR IGNORE INTO embedding_cache \
                     (content_hash, model_fingerprint, embedding, dim, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .bind(content_hash)
                .bind(model_fingerprint)
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
    pub fn evict(&self) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("cache_evict").entered();

        self.rt.block_on(async {
            // Use logical data size, not physical pages (DS-49)
            let size: i64 = match sqlx::query_scalar(
                "SELECT COALESCE(SUM(LENGTH(embedding)), 0) + COUNT(*) * 200 FROM embedding_cache",
            )
            .fetch_one(&self.pool)
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
            let avg_entry: i64 = sqlx::query_scalar(
                "SELECT COALESCE(AVG(LENGTH(embedding) + 200), 4200) FROM embedding_cache",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Cache evict avg-entry query failed, using default");
                4200
            });
            // AC-1: don't force minimum 100 deletions — delete only what's needed
            let entries_to_delete = (excess / avg_entry.max(1) as u64).max(1);

            let result = sqlx::query(
                "DELETE FROM embedding_cache WHERE rowid IN \
                 (SELECT rowid FROM embedding_cache ORDER BY created_at ASC LIMIT ?1)",
            )
            .bind(entries_to_delete as i64)
            .execute(&self.pool)
            .await?;

            let evicted = result.rows_affected() as usize;
            tracing::info!(evicted, "Cache eviction complete");
            Ok(evicted)
        })
    }

    /// Get cache statistics.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let _span = tracing::info_span!("cache_stats").entered();

        self.rt.block_on(async {
            let total_entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM embedding_cache")
                .fetch_one(&self.pool)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "cache stats: COUNT failed");
                    0
                });

            let total_size: i64 = sqlx::query_scalar(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cache stats: page_size failed");
                0
            });

            let unique_models: i64 =
                sqlx::query_scalar("SELECT COUNT(DISTINCT model_fingerprint) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "cache stats: DISTINCT failed");
                        0
                    });

            let oldest: Option<i64> =
                sqlx::query_scalar("SELECT MIN(created_at) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "cache stats: MIN failed");
                        None
                    });

            let newest: Option<i64> =
                sqlx::query_scalar("SELECT MAX(created_at) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "cache stats: MAX failed");
                        None
                    });

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
    pub fn prune_older_than(&self, days: u32) -> Result<usize, CacheError> {
        let _span = tracing::info_span!("cache_prune", days).entered();

        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            - (days as i64 * 86400);

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

    #[test]
    fn test_open_creates_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sub/dir/cache.db");
        assert!(!path.exists());
        let _cache = EmbeddingCache::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_roundtrip() {
        let (cache, _dir) = test_cache();
        let emb = make_embedding(1024, 1.0);
        let entries = vec![("hash_a".to_string(), emb.clone())];
        cache.write_batch(&entries, "fp_1", 1024).unwrap();

        let result = cache.read_batch(&["hash_a"], "fp_1", 1024).unwrap();
        assert_eq!(result.len(), 1);
        let cached = &result["hash_a"];
        assert_eq!(cached.len(), 1024);
        assert!((cached[0] - emb[0]).abs() < 1e-6);
    }

    #[test]
    fn test_miss() {
        let (cache, _dir) = test_cache();
        let result = cache.read_batch(&["nonexistent"], "fp_1", 1024).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_batch_write() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..100)
            .map(|i| (format!("hash_{i}"), make_embedding(768, i as f32)))
            .collect();
        let written = cache.write_batch(&entries, "fp_1", 768).unwrap();
        assert_eq!(written, 100);

        let hashes: Vec<&str> = entries.iter().map(|(h, _)| h.as_str()).collect();
        let result = cache.read_batch(&hashes, "fp_1", 768).unwrap();
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_different_fingerprints() {
        let (cache, _dir) = test_cache();
        let emb_a = make_embedding(1024, 1.0);
        let emb_b = make_embedding(1024, 2.0);

        cache
            .write_batch(&[("hash_x".to_string(), emb_a.clone())], "fp_a", 1024)
            .unwrap();
        cache
            .write_batch(&[("hash_x".to_string(), emb_b.clone())], "fp_b", 1024)
            .unwrap();

        let a = cache.read_batch(&["hash_x"], "fp_a", 1024).unwrap();
        let b = cache.read_batch(&["hash_x"], "fp_b", 1024).unwrap();

        assert!((a["hash_x"][0] - emb_a[0]).abs() < 1e-6);
        assert!((b["hash_x"][0] - emb_b[0]).abs() < 1e-6);
    }

    #[test]
    fn test_dim_mismatch() {
        let (cache, _dir) = test_cache();
        let emb = make_embedding(768, 1.0);
        cache
            .write_batch(&[("hash_a".to_string(), emb)], "fp_1", 768)
            .unwrap();

        // Read with wrong expected dim — should miss
        let result = cache.read_batch(&["hash_a"], "fp_1", 1024).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_zero_length_embedding() {
        let (cache, _dir) = test_cache();
        let entries = vec![("hash_a".to_string(), vec![])];
        let written = cache.write_batch(&entries, "fp_1", 0).unwrap();
        assert_eq!(written, 0); // empty embeddings skipped
    }

    #[test]
    fn test_clear() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..10)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache.write_batch(&entries, "fp_1", 128).unwrap();

        let deleted = cache.clear(None).unwrap();
        assert_eq!(deleted, 10);

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 0);
    }

    #[test]
    fn test_clear_by_model() {
        let (cache, _dir) = test_cache();
        cache
            .write_batch(&[("h1".to_string(), make_embedding(128, 1.0))], "fp_a", 128)
            .unwrap();
        cache
            .write_batch(&[("h2".to_string(), make_embedding(128, 2.0))], "fp_b", 128)
            .unwrap();

        cache.clear(Some("fp_a")).unwrap();

        let a = cache.read_batch(&["h1"], "fp_a", 128).unwrap();
        let b = cache.read_batch(&["h2"], "fp_b", 128).unwrap();
        assert!(a.is_empty()); // cleared
        assert_eq!(b.len(), 1); // survived
    }

    #[test]
    fn test_stats() {
        let (cache, _dir) = test_cache();
        let entries: Vec<_> = (0..5)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache.write_batch(&entries, "fp_1", 128).unwrap();

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
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
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
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY (content_hash, model_fingerprint)
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
        };

        let entries: Vec<_> = (0..10)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache.write_batch(&entries, "fp_1", 128).unwrap();

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
        let written = cache.write_batch(&entries, "fp_cross", 768).unwrap();
        assert_eq!(written, 150);

        // Read all 150 back in one call
        let hashes: Vec<&str> = entries.iter().map(|(h, _)| h.as_str()).collect();
        let result = cache.read_batch(&hashes, "fp_cross", 768).unwrap();
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

    // ===== TC-21: NaN embedding behavior =====

    #[test]
    fn test_nan_embedding() {
        let (cache, _dir) = test_cache();

        // Create an embedding containing NaN values
        let mut nan_emb = make_embedding(128, 1.0);
        nan_emb[0] = f32::NAN;
        nan_emb[64] = f32::NAN;

        let entries = vec![("hash_nan".to_string(), nan_emb)];
        // write_batch does not currently reject NaN — it round-trips through blob encoding.
        // This test documents the current behavior: NaN is stored and retrieved.
        let written = cache.write_batch(&entries, "fp_nan", 128).unwrap();
        assert_eq!(written, 1);

        let result = cache.read_batch(&["hash_nan"], "fp_nan", 128).unwrap();
        assert_eq!(result.len(), 1);
        let cached = &result["hash_nan"];
        assert!(cached[0].is_nan(), "NaN should round-trip through cache");
        assert!(cached[64].is_nan(), "NaN should round-trip through cache");
        // Non-NaN values should be preserved
        assert!(!cached[1].is_nan());
    }

    // ===== TC-24: prune edge cases =====

    #[test]
    fn test_prune_zero_days() {
        let (cache, _dir) = test_cache();

        // Write entries (they get current timestamp)
        let entries: Vec<_> = (0..5)
            .map(|i| (format!("h{i}"), make_embedding(128, i as f32)))
            .collect();
        cache.write_batch(&entries, "fp_1", 128).unwrap();

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
        cache.write_batch(&entries, "fp_1", 128).unwrap();

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
        let written = cache.write_batch(&entries, "fp_dup", 128).unwrap();
        // Only 1 row should be written (second is ignored due to PK conflict)
        assert_eq!(
            written, 1,
            "Duplicate hash should be ignored by INSERT OR IGNORE"
        );

        // Read back — the first embedding (emb_a) should win
        let result = cache.read_batch(&["dup_hash"], "fp_dup", 128).unwrap();
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
                    "INSERT INTO embedding_cache (content_hash, model_fingerprint, embedding, dim, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)")
                    .bind(format!("old_{i}"))
                    .bind("fp_1")
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
        cache.write_batch(&entries, "fp_1", 128).unwrap();

        // Prune entries older than 1 day — should remove the 5 old ones
        let pruned = cache.prune_older_than(1).unwrap();
        assert_eq!(pruned, 5);

        let stats = cache.stats().unwrap();
        assert_eq!(stats.total_entries, 3); // only fresh ones survive
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
    rt: tokio::runtime::Runtime,
}

impl QueryCache {
    /// Default cache location (same directory as the embedding cache).
    pub fn default_path() -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".cache/cqs/query_cache.db")
    }

    /// Open or create the query cache.
    pub fn open(path: &Path) -> Result<Self, CacheError> {
        let _span = tracing::info_span!("query_cache_open", path = %path.display()).entered();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CacheError::Io(std::io::Error::other(e)))?;

        let connect_opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(2))
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

        // SEC-V1.25-4: restrict DB + WAL/SHM sidecar files to 0o600 to
        // match EmbeddingCache::open. Query text may be sensitive (user
        // prompts, internal tooling queries), and multi-user boxes must
        // not leave this world-readable.
        #[cfg(unix)]
        {
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
                            "Failed to set query cache permissions to 0o600"
                        );
                    }
                }
            }
        }

        tracing::debug!(path = %path.display(), "Query cache opened");
        Ok(Self { pool, rt })
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
                    let preview_len = query.len().min(40);
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
