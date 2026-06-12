//! Test chunk discovery and stale call pruning.

use super::{TEST_CHUNKS_SQL, TEST_CHUNK_NAMES_SQL};
use crate::store::helpers::{ChunkRow, ChunkSummary, StoreError};
use crate::store::Store;

impl<Mode> Store<Mode> {
    /// Async helper for find_test_chunks (reused by find_dead_code)
    /// Loads only lightweight columns (no content/doc) since callers only need
    /// name, file, and line_start. The SQL WHERE clause still filters on content
    /// (for test markers like `#[test]`) but avoids returning it.
    /// Test markers and path patterns are sourced from `LanguageDef` fields
    /// (`test_markers`, `test_path_patterns`) across all enabled languages,
    /// falling back to hardcoded defaults when no language provides any.
    pub(super) async fn find_test_chunks_async(&self) -> Result<Vec<ChunkSummary>, StoreError> {
        // SQL is built once and cached in TEST_CHUNKS_SQL (LazyLock).
        // Select only lightweight columns; content/doc filtering happens in WHERE
        // but we don't need them in the result set.
        let rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(TEST_CHUNKS_SQL.as_str()))
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| ChunkSummary::from(ChunkRow::from_row_lightweight(&row)))
            .collect())
    }

    /// Async helper that returns only test chunk names (no metadata).
    /// Avoids allocating `ChunkSummary` structs when callers only need
    /// the name set (e.g., `find_dead_code` exclusion filtering).
    pub(super) async fn find_test_chunk_names_async(&self) -> Result<Vec<String>, StoreError> {
        // SQL is built once and cached in TEST_CHUNK_NAMES_SQL (LazyLock).
        let rows: Vec<(String,)> =
            sqlx::query_as(sqlx::AssertSqlSafe(TEST_CHUNK_NAMES_SQL.as_str()))
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

    /// Find test chunks using language-specific heuristics.
    /// Identifies test functions across all supported languages by:
    /// - Name patterns: `test_*` (Rust/Python), `Test*` (Go)
    /// - Content patterns: sourced from `LanguageDef::test_markers` per language
    /// - Path patterns: sourced from `LanguageDef::test_path_patterns` per language
    /// Uses a broad SQL filter then Rust post-filter for precision.
    /// Cached test chunks — populated on first access, returns a clone from OnceLock.
    /// **No invalidation by design.** Same contract as `get_call_graph`: the cache is
    /// intentionally write-once for the `Store` lifetime. Long-lived modes (batch, watch)
    /// must re-open the `Store` to see updated test discovery — do not add a `clear()`.
    /// Returns `Arc<Vec<ChunkSummary>>` so `Arc::clone` is O(1) rather than cloning the
    /// full Vec on every call.
    pub fn find_test_chunks(&self) -> Result<std::sync::Arc<Vec<ChunkSummary>>, StoreError> {
        if let Some(cached) = self.test_chunks_cache.get() {
            return Ok(std::sync::Arc::clone(cached));
        }
        let _span = tracing::info_span!("find_test_chunks").entered();
        let chunks = self.rt.block_on(self.find_test_chunks_async())?;
        let arc = std::sync::Arc::new(chunks);
        let _ = self.test_chunks_cache.set(std::sync::Arc::clone(&arc));
        Ok(arc)
    }
}

impl<Mode> Store<Mode> {
    /// Consistency check: `function_calls.file` values that reference NO file
    /// present in `chunks` UNION `file_registry`. Read-only — returns the list
    /// of orphaned origins for `cqs doctor` and the standing invariant test.
    ///
    /// This is the STANDING MECHANICAL CHECK for the chunk/call-graph lifecycle
    /// decouple. The two lifecycles are managed by distinct writers:
    /// `function_calls` is replaced per file by the single parse-driven writer
    /// (driven by parse-completion, NEVER by chunk count), while `chunks` is
    /// pruned by the chunk primitive. The seam between them reopened six times
    /// because each fix used a chunk-frame signal (`live_ids.is_empty()`) to
    /// make a call-graph decision. The invariant that catches any future
    /// reopening: every `function_calls.file` must correspond to a file the
    /// index knows about — i.e. present in `chunks` OR `file_registry`
    /// (`file_registry` is the v29 complete shadow, so a zero-chunk
    /// oversize-function file — zero chunks, NON-empty calls — is still
    /// "known"). A non-empty result means a writer left orphaned edges: either
    /// a delete path failed to clear `function_calls`, or a parse-driven write
    /// referenced a file never stamped into the registry.
    ///
    /// Note: this deliberately does NOT gate on `chunks` alone. An
    /// oversize-function file is legitimately present in `function_calls` with
    /// ZERO chunks; the `file_registry` arm of the UNION is what keeps it from
    /// being flagged. Gating on chunks alone is exactly the bug this check
    /// guards against.
    pub fn find_orphaned_function_calls(&self) -> Result<Vec<String>, StoreError> {
        let _span = tracing::info_span!("find_orphaned_function_calls").entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT DISTINCT file FROM function_calls \
                 WHERE file NOT IN (SELECT DISTINCT origin FROM chunks) \
                   AND file NOT IN (SELECT origin FROM file_registry) \
                 ORDER BY file",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows.into_iter().map(|(f,)| f).collect())
        })
    }
}
