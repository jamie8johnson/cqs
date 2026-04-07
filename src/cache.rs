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

/// Result of verifying cached embeddings against the current model.
#[derive(Debug)]
pub struct VerifyReport {
    pub sampled: usize,
    pub matched: usize,
    pub mismatched: usize,
    pub missing: usize,
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
        let _span = tracing::info_span!("embedding_cache_open", path = %path.display()).entered();

        // Create parent directories
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CacheError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = rt.block_on(async {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await?;

            // WAL mode for concurrent access
            sqlx::query("PRAGMA journal_mode=WAL")
                .execute(&pool)
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

            // Batch in groups of 100 to avoid SQLite variable limit
            for batch in content_hashes.chunks(100) {
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

                    // Validate dimension
                    if dim as usize != expected_dim {
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

            for (content_hash, embedding) in entries {
                if embedding.is_empty() {
                    continue; // Skip zero-length embeddings
                }

                // Encode Vec<f32> to blob
                let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

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
            let size: i64 = sqlx::query_scalar(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

            if (size as u64) <= self.max_size_bytes {
                return Ok(0);
            }

            let excess = size as u64 - self.max_size_bytes;
            // Estimate: ~4200 bytes per entry (1024-dim)
            let entries_to_delete = (excess / 4200).max(100);

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
                .unwrap_or(0);

            let total_size: i64 = sqlx::query_scalar(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

            let unique_models: i64 =
                sqlx::query_scalar("SELECT COUNT(DISTINCT model_fingerprint) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or(0);

            let oldest: Option<i64> =
                sqlx::query_scalar("SELECT MIN(created_at) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or(None);

            let newest: Option<i64> =
                sqlx::query_scalar("SELECT MAX(created_at) FROM embedding_cache")
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or(None);

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
