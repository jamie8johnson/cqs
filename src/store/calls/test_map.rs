//! Test chunk discovery and stale call pruning.

use super::{TEST_CHUNKS_SQL, TEST_CHUNK_NAMES_SQL};
use crate::store::helpers::{ChunkRow, ChunkSummary, StoreError};
use crate::store::Store;

impl Store {
    /// Async helper for find_test_chunks (reused by find_dead_code)
    ///
    /// Loads only lightweight columns (no content/doc) since callers only need
    /// name, file, and line_start. The SQL WHERE clause still filters on content
    /// (for test markers like `#[test]`) but avoids returning it.
    ///
    /// Test markers and path patterns are sourced from `LanguageDef` fields
    /// (`test_markers`, `test_path_patterns`) across all enabled languages,
    /// falling back to hardcoded defaults when no language provides any.
    pub(super) async fn find_test_chunks_async(&self) -> Result<Vec<ChunkSummary>, StoreError> {
        // SQL is built once and cached in TEST_CHUNKS_SQL (LazyLock).
        // Select only lightweight columns; content/doc filtering happens in WHERE
        // but we don't need them in the result set.
        let rows: Vec<_> = sqlx::query(&TEST_CHUNKS_SQL).fetch_all(&self.pool).await?;

        Ok(rows
            .into_iter()
            .map(|row| ChunkSummary::from(ChunkRow::from_row_lightweight(&row)))
            .collect())
    }

    /// Async helper that returns only test chunk names (no metadata).
    ///
    /// Avoids allocating `ChunkSummary` structs when callers only need
    /// the name set (e.g., `find_dead_code` exclusion filtering).
    pub(super) async fn find_test_chunk_names_async(&self) -> Result<Vec<String>, StoreError> {
        // SQL is built once and cached in TEST_CHUNK_NAMES_SQL (LazyLock).
        let rows: Vec<(String,)> = sqlx::query_as(&TEST_CHUNK_NAMES_SQL)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    /// Delete function_calls for files no longer in the chunks table.
    ///
    /// Used by GC to clean up orphaned call graph entries after pruning chunks.
    pub fn prune_stale_calls(&self) -> Result<u64, StoreError> {
        let _span = tracing::info_span!("prune_stale_calls").entered();
        self.rt.block_on(async {
            let result = sqlx::query(
                "DELETE FROM function_calls WHERE file NOT IN (SELECT DISTINCT origin FROM chunks)",
            )
            .execute(&self.pool)
            .await?;
            let count = result.rows_affected();
            if count > 0 {
                tracing::info!(pruned = count, "Pruned stale call graph entries");
            }
            Ok(count)
        })
    }

    /// Find test chunks using language-specific heuristics.
    ///
    /// Identifies test functions across all supported languages by:
    /// - Name patterns: `test_*` (Rust/Python), `Test*` (Go)
    /// - Content patterns: sourced from `LanguageDef::test_markers` per language
    /// - Path patterns: sourced from `LanguageDef::test_path_patterns` per language
    ///
    /// Uses a broad SQL filter then Rust post-filter for precision.
    ///
    /// Cached test chunks — populated on first access, returns clone from OnceLock.
    ///
    /// **No invalidation by design.** Same contract as `get_call_graph`: the cache is
    /// intentionally write-once for the `Store` lifetime. Long-lived modes (batch, watch)
    /// must re-open the `Store` to see updated test discovery — do not add a `clear()`.
    /// ~14 call sites benefit from this single-scan caching.
    pub fn find_test_chunks(&self) -> Result<Vec<ChunkSummary>, StoreError> {
        if let Some(cached) = self.test_chunks_cache.get() {
            return Ok(cached.clone());
        }
        let _span = tracing::info_span!("find_test_chunks").entered();
        let chunks = self.rt.block_on(self.find_test_chunks_async())?;
        let _ = self.test_chunks_cache.set(chunks.clone());
        Ok(chunks)
    }
}
