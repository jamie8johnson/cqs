//! SQLite storage for chunks and embeddings (sqlx async with sync wrappers)
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

mod calls;
mod chunks;
mod migrations;
mod notes;
mod types;

/// Helper types and embedding conversion functions.
///
/// This module is `pub(crate)` - external consumers should use the re-exported
/// types from `cqs::store` instead of accessing `cqs::store::helpers` directly.
pub(crate) mod helpers;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use tokio::runtime::Runtime;

// Re-export public types with documentation

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

/// Expected embedding dimensions (768 model + 1 sentiment).
pub use helpers::EXPECTED_DIMENSIONS;

/// Name of the embedding model used.
pub use helpers::MODEL_NAME;

/// Score a chunk name against a query for definition search.
pub use helpers::score_name_match;

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

// Internal use
use helpers::{clamp_line_number, ChunkRow};

use crate::nl::normalize_for_fts;

/// Defense-in-depth sanitization for FTS5 query strings.
///
/// Strips or escapes FTS5 special characters that could alter query semantics.
/// Applied after `normalize_for_fts()` as an extra safety layer — if `normalize_for_fts`
/// ever changes to allow characters through, this prevents FTS5 injection.
///
/// FTS5 special characters: `"`, `*`, `(`, `)`, `+`, `-`, `^`, `:`, `NEAR`
/// FTS5 boolean operators: `OR`, `AND`, `NOT` (case-sensitive in FTS5)
///
/// # Safety (injection)
///
/// This function independently strips all FTS5-significant characters including
/// double quotes. Safe for use in `format!`-constructed FTS5 queries even without
/// `normalize_for_fts()`. The double-pass pattern (`normalize_for_fts` then
/// `sanitize_fts_query`) is defense-in-depth — either layer alone prevents injection.
pub(crate) fn sanitize_fts_query(s: &str) -> String {
    // Remove FTS5 special characters
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':'))
        .collect();

    // Remove FTS5 boolean operators as standalone words.
    // Split on whitespace, filter out operators, rejoin.
    cleaned
        .split_whitespace()
        .filter(|word| !matches!(*word, "OR" | "AND" | "NOT" | "NEAR"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Thread-safe SQLite store for chunks and embeddings
///
/// Uses sqlx connection pooling for concurrent reads and WAL mode
/// for crash safety. All methods are synchronous but internally use
/// an async runtime to execute sqlx operations.
///
/// # Example
///
/// ```no_run
/// use cqs::Store;
/// use std::path::Path;
///
/// let store = Store::open(Path::new(".cqs/index.db"))?;
/// let stats = store.stats()?;
/// println!("Indexed {} chunks", stats.total_chunks);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Store {
    pub(crate) pool: SqlitePool,
    pub(crate) rt: Runtime,
    /// Whether close() has already been called (skip WAL checkpoint in Drop)
    closed: AtomicBool,
    notes_summaries_cache: RwLock<Option<Vec<NoteSummary>>>,
}

impl Store {
    /// Open an existing index with connection pooling
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let _span = tracing::info_span!("store_open", path = %path.display()).entered();
        let rt = Runtime::new().map_err(|e| StoreError::Runtime(e.to_string()))?;

        // Use SqliteConnectOptions::filename() to avoid URL parsing issues with
        // special characters in paths (spaces, #, ?, %, unicode).
        let connect_opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Normal)
            .pragma("mmap_size", "268435456"); // 256MB memory-mapped I/O

        // SQLite connection pool with WAL mode for concurrent reads
        let pool = rt.block_on(async {
            SqlitePoolOptions::new()
                .max_connections(4) // 4 = typical CLI parallelism (index, search, watch)
                .idle_timeout(std::time::Duration::from_secs(300)) // Close idle connections after 5 min
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        // 16MB page cache per connection (negative = KB, -16384 = 16MB)
                        sqlx::query("PRAGMA cache_size = -16384")
                            .execute(&mut *conn)
                            .await?;
                        // Keep temp tables in memory
                        sqlx::query("PRAGMA temp_store = MEMORY")
                            .execute(&mut *conn)
                            .await?;
                        Ok(())
                    })
                })
                .connect_with(connect_opts)
                .await
        })?;

        let store = Self {
            pool,
            rt,
            closed: AtomicBool::new(false),
            notes_summaries_cache: RwLock::new(None),
        };

        // Set restrictive permissions on database files (Unix only)
        // These files contain code embeddings - not secrets, but defense-in-depth
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let restrictive = std::fs::Permissions::from_mode(0o600);
            // Main database file
            if let Err(e) = std::fs::set_permissions(path, restrictive.clone()) {
                tracing::debug!(path = %path.display(), error = %e, "Failed to set permissions");
            }
            // WAL and SHM files (may not exist yet, ignore errors)
            let wal_path = path.with_extension("db-wal");
            let shm_path = path.with_extension("db-shm");
            if let Err(e) = std::fs::set_permissions(&wal_path, restrictive.clone()) {
                tracing::debug!(path = %wal_path.display(), error = %e, "Failed to set permissions");
            }
            if let Err(e) = std::fs::set_permissions(&shm_path, restrictive) {
                tracing::debug!(path = %shm_path.display(), error = %e, "Failed to set permissions");
            }
        }

        tracing::info!(path = %path.display(), "Database connected");

        // Quick integrity check — catches B-tree corruption early
        store.rt.block_on(async {
            let result: (String,) = sqlx::query_as("PRAGMA quick_check")
                .fetch_one(&store.pool)
                .await?;
            if result.0 != "ok" {
                return Err(StoreError::Corruption(result.0));
            }
            Ok::<_, StoreError>(())
        })?;

        // Check schema version compatibility
        store.check_schema_version(path)?;
        // Check model version compatibility
        store.check_model_version()?;
        // Warn if index was created by different cqs version
        store.check_cq_version();

        Ok(store)
    }

    /// Open an existing index in read-only mode with reduced resources.
    ///
    /// Uses minimal connection pool, smaller cache, and single-threaded runtime.
    /// Suitable for reference stores and background builds that only read data.
    pub fn open_readonly(path: &Path) -> Result<Self, StoreError> {
        let _span = tracing::info_span!("store_open_readonly", path = %path.display()).entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StoreError::Runtime(e.to_string()))?;

        // Use SqliteConnectOptions::filename() to avoid URL parsing issues with
        // special characters in paths (spaces, #, ?, %, unicode).
        let connect_opts = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Normal)
            .pragma("mmap_size", "67108864"); // 64MB mmap (reduced from 256MB)

        let pool = rt.block_on(async {
            SqlitePoolOptions::new()
                .max_connections(1)
                .idle_timeout(std::time::Duration::from_secs(300))
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        // 4MB page cache (reduced from 16MB)
                        sqlx::query("PRAGMA cache_size = -4096")
                            .execute(&mut *conn)
                            .await?;
                        // Keep temp tables in memory
                        sqlx::query("PRAGMA temp_store = MEMORY")
                            .execute(&mut *conn)
                            .await?;
                        Ok(())
                    })
                })
                .connect_with(connect_opts)
                .await
        })?;

        let store = Self {
            pool,
            rt,
            closed: AtomicBool::new(false),
            notes_summaries_cache: RwLock::new(None),
        };

        // Skip permissions setting (read-only, no file creation)

        tracing::info!(path = %path.display(), "Database connected (read-only)");

        // Quick integrity check — catches B-tree corruption early
        store.rt.block_on(async {
            let result: (String,) = sqlx::query_as("PRAGMA quick_check")
                .fetch_one(&store.pool)
                .await?;
            if result.0 != "ok" {
                return Err(StoreError::Corruption(result.0));
            }
            Ok::<_, StoreError>(())
        })?;

        store.check_schema_version(path)?;
        store.check_model_version()?;
        store.check_cq_version();

        Ok(store)
    }

    /// Create a new index
    ///
    /// Wraps all DDL and metadata inserts in a single transaction so a
    /// crash mid-init cannot leave a partial schema.
    pub fn init(&self, model_info: &ModelInfo) -> Result<(), StoreError> {
        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            // Create tables - execute each statement separately
            let schema = include_str!("../schema.sql");
            for statement in schema.split(';') {
                let stmt: String = statement
                    .lines()
                    .skip_while(|line| {
                        let trimmed = line.trim();
                        trimmed.is_empty() || trimmed.starts_with("--")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                sqlx::query(stmt).execute(&mut *tx).await?;
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

    fn check_schema_version(&self, path: &Path) -> Result<(), StoreError> {
        let path_str = path.display().to_string();
        self.rt.block_on(async {
            let row: Option<(String,)> =
                match sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_optional(&self.pool)
                    .await
                {
                    Ok(r) => r,
                    Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => {
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

            let version: i32 = row
                .and_then(|(s,)| {
                    s.parse()
                        .map_err(|e| {
                            tracing::warn!(
                                stored_value = %s,
                                error = %e,
                                "Failed to parse schema_version from metadata, defaulting to 0"
                            );
                        })
                        .ok()
                })
                .unwrap_or(0);

            if version > CURRENT_SCHEMA_VERSION {
                return Err(StoreError::SchemaNewerThanCq(version));
            }
            if version < CURRENT_SCHEMA_VERSION && version > 0 {
                // Attempt migration instead of failing
                match migrations::migrate(&self.pool, version, CURRENT_SCHEMA_VERSION).await {
                    Ok(()) => {
                        tracing::info!(
                            path = %path_str,
                            from = version,
                            to = CURRENT_SCHEMA_VERSION,
                            "Schema migrated successfully"
                        );
                    }
                    Err(StoreError::MigrationNotSupported(from, to)) => {
                        // No migration available, fall back to original error
                        return Err(StoreError::SchemaMismatch(path_str, from, to));
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })
    }

    fn check_model_version(&self) -> Result<(), StoreError> {
        self.rt.block_on(async {
            // Check model name
            let row: Option<(String,)> =
                match sqlx::query_as("SELECT value FROM metadata WHERE key = 'model_name'")
                    .fetch_optional(&self.pool)
                    .await
                {
                    Ok(r) => r,
                    Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => {
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

            let stored_model = row.map(|(s,)| s).unwrap_or_default();

            if !stored_model.is_empty() && stored_model != MODEL_NAME {
                return Err(StoreError::ModelMismatch(
                    stored_model,
                    MODEL_NAME.to_string(),
                ));
            }

            // Check embedding dimensions
            let dim_row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'dimensions'")
                    .fetch_optional(&self.pool)
                    .await?;

            if let Some((dim_str,)) = dim_row {
                if let Ok(stored_dim) = dim_str.parse::<u32>() {
                    if stored_dim != EXPECTED_DIMENSIONS {
                        return Err(StoreError::DimensionMismatch(
                            stored_dim,
                            EXPECTED_DIMENSIONS,
                        ));
                    }
                } else {
                    tracing::warn!(dim = %dim_str, "Failed to parse stored dimension");
                }
            }

            Ok(())
        })
    }

    fn check_cq_version(&self) {
        if let Err(e) = self.rt.block_on(async {
            let row: Option<(String,)> =
                match sqlx::query_as("SELECT value FROM metadata WHERE key = 'cq_version'")
                    .fetch_optional(&self.pool)
                    .await
                {
                    Ok(row) => row,
                    Err(e) => {
                        tracing::debug!(error = %e, "Failed to read cq_version from metadata");
                        return Ok::<_, StoreError>(());
                    }
                };

            let stored_version = row.map(|(s,)| s).unwrap_or_default();
            let current_version = env!("CARGO_PKG_VERSION");

            if !stored_version.is_empty() && stored_version != current_version {
                tracing::info!(
                    "Index created by cqs v{}, running v{}",
                    stored_version,
                    current_version
                );
            }
            Ok::<_, StoreError>(())
        }) {
            tracing::debug!(error = %e, "check_cq_version failed");
        }
    }

    /// Search FTS5 index for keyword matches.
    ///
    /// # Search Method Overview
    ///
    /// The Store provides several search methods with different characteristics:
    ///
    /// - **`search_fts`**: Full-text keyword search using SQLite FTS5. Returns chunk IDs.
    ///   Best for: Exact keyword matches, symbol lookup by name fragment.
    ///
    /// - **`search_by_name`**: Definition search by function/struct name. Uses FTS5 with
    ///   heavy weighting on the name column. Returns full `SearchResult` with scores.
    ///   Best for: "Where is X defined?" queries.
    ///
    /// - **`search_filtered`** (in search.rs): Semantic search with optional language/path
    ///   filters. Can use RRF hybrid search combining semantic + FTS scores.
    ///   Best for: Natural language queries like "retry with exponential backoff".
    ///
    /// - **`search_filtered_with_index`** (in search.rs): Like `search_filtered` but uses
    ///   HNSW/CAGRA vector index for O(log n) candidate retrieval instead of brute force.
    ///   Best for: Large indexes (>5k chunks) where brute force is slow.
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let _span = tracing::info_span!("search_fts", limit).entered();
        let normalized_query = sanitize_fts_query(&normalize_for_fts(query));
        if normalized_query.is_empty() {
            tracing::debug!(
                original_query = %query,
                "Query normalized to empty string, returning no FTS results"
            );
            return Ok(vec![]);
        }

        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
            )
            .bind(&normalized_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.into_iter().map(|(id,)| id).collect())
        })
    }

    /// Search for chunks by name (definition search).
    ///
    /// Searches the FTS5 name column for exact or prefix matches.
    /// Use this for "where is X defined?" queries instead of semantic search.
    pub fn search_by_name(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!("search_by_name", %name, limit).entered();
        let limit = limit.min(100);
        let normalized = sanitize_fts_query(&normalize_for_fts(name));
        if normalized.is_empty() {
            return Ok(vec![]);
        }

        // Search name column specifically using FTS5 column filter
        // Use * for prefix matching (e.g., "parse" matches "parse_config")
        debug_assert!(
            !normalized.contains('"'),
            "sanitized query must not contain double quotes"
        );
        let fts_query = format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized);

        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id
                 FROM chunks c
                 JOIN chunks_fts f ON c.id = f.id
                 WHERE chunks_fts MATCH ?1
                 ORDER BY bm25(chunks_fts, 10.0, 1.0, 1.0, 1.0) -- Heavy weight on name column
                 LIMIT ?2",
            )
            .bind(&fts_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            use sqlx::Row;
            let results = rows
                .into_iter()
                .map(|row| {
                    let chunk = ChunkSummary::from(ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: clamp_line_number(row.get::<i64, _>(8)),
                        line_end: clamp_line_number(row.get::<i64, _>(9)),
                        parent_id: row.get(10),
                    });
                    let score = score_name_match(&chunk.name, name);
                    SearchResult { chunk, score }
                })
                .collect();

            Ok(results)
        })
    }

    /// Compute RRF (Reciprocal Rank Fusion) scores for combining two ranked lists.
    ///
    /// Allocates a new HashMap per search. Pre-allocated buffer was considered but:
    /// - Input size varies (limit*3 semantic + limit*3 FTS = up to 6*limit entries)
    /// - HashMap with ~30-100 entries costs ~1KB, negligible vs embedding costs (~3KB)
    /// - Thread-local buffer would add complexity for ~0.1ms savings on typical searches
    pub(crate) fn rrf_fuse(
        semantic_ids: &[String],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        // K=60 is the standard RRF constant from the original paper.
        // Higher K reduces the impact of rank differences (smoother fusion).
        const K: f32 = 60.0;

        let mut scores: HashMap<&str, f32> = HashMap::new();

        for (rank, id) in semantic_ids.iter().enumerate() {
            // RRF formula: 1 / (K + rank). The + 1.0 converts 0-indexed enumerate()
            // to 1-indexed ranks (first result = rank 1, not rank 0).
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id.as_str()).or_insert(0.0) += contribution;
        }

        for (rank, id) in fts_ids.iter().enumerate() {
            // Same conversion: enumerate's 0-index -> RRF's 1-indexed rank
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id.as_str()).or_insert(0.0) += contribution;
        }

        let mut sorted: Vec<(String, f32)> = scores
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
        sorted.truncate(limit);
        sorted
    }

    /// Exposed for property testing only
    #[cfg(test)]
    pub(crate) fn rrf_fuse_test(
        semantic_ids: &[String],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        Self::rrf_fuse(semantic_ids, fts_ids, limit)
    }

    /// Update the `updated_at` metadata timestamp to now.
    ///
    /// Call after indexing operations complete (pipeline, watch reindex, note sync)
    /// to track when the index was last modified.
    pub fn touch_updated_at(&self) -> Result<(), StoreError> {
        let now = chrono::Utc::now().to_rfc3339();
        self.rt.block_on(async {
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES ('updated_at', ?1)")
                .bind(&now)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    /// Get cached notes summaries (loaded on first call, invalidated on mutation).
    ///
    /// Returns a cloned Vec rather than a slice reference to avoid holding the
    /// RwLock read guard across caller code. The clone cost is negligible — notes
    /// are typically <100 entries with small strings.
    pub fn cached_notes_summaries(&self) -> Result<Vec<NoteSummary>, StoreError> {
        {
            let guard = self
                .notes_summaries_cache
                .read()
                .map_err(|e| StoreError::Runtime(format!("notes cache lock poisoned: {e}")))?;
            if let Some(ref ns) = *guard {
                return Ok(ns.clone());
            }
        }
        // Cache miss — load from DB and populate
        let ns = self.list_notes_summaries()?;
        {
            let mut guard = self
                .notes_summaries_cache
                .write()
                .map_err(|e| StoreError::Runtime(format!("notes cache lock poisoned: {e}")))?;
            *guard = Some(ns.clone());
        }
        Ok(ns)
    }

    /// Invalidate the cached notes summaries.
    ///
    /// Must be called after any operation that modifies notes (upsert, replace, delete)
    /// so subsequent reads see fresh data.
    pub(crate) fn invalidate_notes_cache(&self) {
        if let Ok(mut guard) = self.notes_summaries_cache.write() {
            *guard = None;
        }
    }

    /// Gracefully close the store, performing WAL checkpoint.
    ///
    /// This ensures all WAL changes are written to the main database file,
    /// reducing startup time for subsequent opens and freeing disk space
    /// used by WAL files.
    ///
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

impl Drop for Store {
    fn drop(&mut self) {
        if self.closed.load(Ordering::Acquire) {
            return; // Already checkpointed in close()
        }
        // Best-effort WAL checkpoint on drop to avoid leaving large WAL files.
        // Errors are logged but not propagated (Drop can't fail).
        // catch_unwind guards against block_on panicking when called from
        // within an async context (e.g., if Store is dropped inside a tokio runtime).
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(e) = self.rt.block_on(async {
                sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                    .execute(&self.pool)
                    .await
            }) {
                tracing::warn!(error = %e, "WAL checkpoint on drop failed (non-fatal)");
            }
        }));
        // Pool closes automatically when dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ===== Property-based tests for RRF =====

    proptest! {
        /// Property: RRF scores are always positive
        #[test]
        fn prop_rrf_scores_positive(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..20),
            fts in prop::collection::vec("[a-z]{1,5}", 0..20),
            limit in 1usize..50
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            for (_, score) in &result {
                prop_assert!(*score > 0.0, "RRF score should be positive: {}", score);
            }
        }

        /// Property: RRF scores are bounded
        /// Note: Duplicates in input lists can accumulate extra points.
        /// Max theoretical: sum of 1/(K+r+1) for all appearances across both lists.
        #[test]
        fn prop_rrf_scores_bounded(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..20),
            fts in prop::collection::vec("[a-z]{1,5}", 0..20),
            limit in 1usize..50
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            // Conservative upper bound: sum of first N terms of 1/(K+r+1) for both lists
            // where N is max list length (20). With duplicates, actual max is ~0.3
            let max_possible = 0.5; // generous bound accounting for duplicates
            for (id, score) in &result {
                prop_assert!(
                    *score <= max_possible,
                    "RRF score {} for '{}' exceeds max {}",
                    score, id, max_possible
                );
            }
        }

        /// Property: RRF respects limit
        #[test]
        fn prop_rrf_respects_limit(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..30),
            fts in prop::collection::vec("[a-z]{1,5}", 0..30),
            limit in 1usize..20
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            prop_assert!(
                result.len() <= limit,
                "Result length {} exceeds limit {}",
                result.len(), limit
            );
        }

        /// Property: RRF results are sorted by score descending
        #[test]
        fn prop_rrf_sorted_descending(
            semantic in prop::collection::vec("[a-z]{1,5}", 1..20),
            fts in prop::collection::vec("[a-z]{1,5}", 1..20),
            limit in 1usize..50
        ) {
            let result = Store::rrf_fuse_test(&semantic, &fts, limit);
            for window in result.windows(2) {
                prop_assert!(
                    window[0].1 >= window[1].1,
                    "Results not sorted: {} < {}",
                    window[0].1, window[1].1
                );
            }
        }

        /// Property: Items appearing in both lists get higher scores
        /// Note: Uses hash_set to ensure unique IDs - duplicates in input lists
        /// accumulate scores which can violate the "overlap wins" property.
        #[test]
        fn prop_rrf_rewards_overlap(
            common_id in "[a-z]{3}",
            only_semantic in prop::collection::hash_set("[A-Z]{3}", 1..5),
            only_fts in prop::collection::hash_set("[0-9]{3}", 1..5)
        ) {
            let mut semantic = vec![common_id.clone()];
            semantic.extend(only_semantic);
            let mut fts = vec![common_id.clone()];
            fts.extend(only_fts);

            let result = Store::rrf_fuse_test(&semantic, &fts, 100);

            let common_score = result.iter()
                .find(|(id, _)| id == &common_id)
                .map(|(_, s)| *s)
                .unwrap_or(0.0);

            let max_single = result.iter()
                .filter(|(id, _)| id != &common_id)
                .map(|(_, s)| *s)
                .fold(0.0f32, |a, b| a.max(b));

            prop_assert!(
                common_score >= max_single,
                "Common item score {} should be >= single-list max {}",
                common_score, max_single
            );
        }

        // ===== FTS fuzz tests =====

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
    }
}
