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
use sqlx::{ConnectOptions, SqlitePool};
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

// Internal use
use helpers::ChunkRow;

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
            word.chars()
                .filter(|c| !matches!(c, '"' | '*' | '(' | ')' | '+' | '-' | '^' | ':')),
        );
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.to_string()
}

/// Thread-safe SQLite store for chunks and embeddings
///
/// Uses sqlx connection pooling for concurrent reads and WAL mode
/// for crash safety. All methods are synchronous but internally use
/// an async runtime to execute sqlx operations.
///
/// # Memory-mapped I/O
///
/// `open()` sets `PRAGMA mmap_size = 256MB` per connection with a 4-connection pool,
/// reserving up to 1GB of virtual address space. `open_readonly()` uses 64MB × 1.
/// This is intentional and benign on 64-bit systems (128TB virtual address space).
/// Mmap pages are demand-paged from the database file and evicted under memory
/// pressure — actual RSS reflects only accessed pages, not the mmap reservation.
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
    /// Cached call graph — populated on first access, valid for Store lifetime.
    ///
    /// **No invalidation mechanism by design.** `OnceLock` is intentionally write-once:
    /// once populated the cache is never cleared. This is safe because `Store` is opened
    /// per-command (one `open()` → use → `close()` cycle), so the index cannot change
    /// while the cache is live. Long-lived `Store` instances (batch mode, watch mode)
    /// must be re-opened to pick up index changes; the caller is responsible for that
    /// lifecycle. Do not add invalidation logic here — it would be dead code for the
    /// normal case and racy for the long-lived case (use a fresh `Store` instead).
    call_graph_cache: std::sync::OnceLock<std::sync::Arc<CallGraph>>,
    /// Cached test chunks — populated on first access, valid for Store lifetime.
    ///
    /// Same no-invalidation contract as `call_graph_cache` above: intentionally
    /// write-once for the per-command `Store` lifetime. Re-open the `Store` if the
    /// underlying index has been updated (e.g., after `cqs index` in watch mode).
    test_chunks_cache: std::sync::OnceLock<Vec<ChunkSummary>>,
}

/// Internal configuration for [`Store::open_with_config`].
///
/// Captures the five parameters that differ between read-write and read-only
/// opens so the shared connection/pool/validation logic lives in one place.
struct StoreOpenConfig {
    read_only: bool,
    use_current_thread: bool,
    max_connections: u32,
    mmap_size: &'static str,
    cache_size: &'static str,
}

impl Store {
    /// Open an existing index with connection pooling
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: false,
                use_current_thread: false,
                max_connections: 4,
                mmap_size: "268435456", // 256MB
                cache_size: "-16384",   // 16MB
            },
        )
    }

    /// Open an existing index with single-threaded runtime but full memory.
    ///
    /// Uses `current_thread` tokio runtime (1 OS thread instead of 4) while
    /// keeping the full 256MB mmap and 16MB cache of `open()`. Ideal for
    /// read-only CLI commands on the primary project index where we need
    /// full search performance but don't need multi-threaded async.
    pub fn open_light(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: false,
                use_current_thread: true,
                max_connections: 4,
                mmap_size: "268435456", // 256MB
                cache_size: "-16384",   // 16MB
            },
        )
    }

    /// Open an existing index in read-only mode with reduced resources.
    ///
    /// Uses minimal connection pool, smaller cache, and single-threaded runtime.
    /// Suitable for reference stores and background builds that only read data.
    pub fn open_readonly(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_config(
            path,
            StoreOpenConfig {
                read_only: true,
                use_current_thread: true,
                max_connections: 1,
                mmap_size: "67108864", // 64MB
                cache_size: "-4096",   // 4MB
            },
        )
    }

    /// Shared open logic for both read-write and read-only modes.
    fn open_with_config(path: &Path, config: StoreOpenConfig) -> Result<Self, StoreError> {
        let mode = if config.read_only { "readonly" } else { "open" };
        let _span = tracing::info_span!("store_open", %mode, path = %path.display()).entered();

        // Build runtime: multi-thread for write (RM-14: match pool size),
        // current-thread for read-only (minimal overhead).
        let rt = if config.use_current_thread {
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
            .busy_timeout(std::time::Duration::from_secs(5))
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
                .idle_timeout(std::time::Duration::from_secs(300))
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

        let store = Self {
            pool,
            rt,
            closed: AtomicBool::new(false),
            notes_summaries_cache: RwLock::new(None),
            call_graph_cache: std::sync::OnceLock::new(),
            test_chunks_cache: std::sync::OnceLock::new(),
        };

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

        // Quick integrity check — catches B-tree corruption early
        store.rt.block_on(async {
            let result: (String,) = sqlx::query_as("PRAGMA integrity_check(1)")
                .fetch_one(&store.pool)
                .await?;
            if result.0 != "ok" {
                return Err(StoreError::Corruption(result.0));
            }
            Ok::<_, StoreError>(())
        })?;

        // Check model version BEFORE schema migration — if model mismatches,
        // we don't want to commit a schema upgrade on a DB we'll reject anyway
        store.check_model_version()?;
        store.check_schema_version(path)?;
        store.check_cq_version();

        Ok(store)
    }

    /// Create a new index
    ///
    /// Wraps all DDL and metadata inserts in a single transaction so a
    /// crash mid-init cannot leave a partial schema.
    pub fn init(&self, model_info: &ModelInfo) -> Result<(), StoreError> {
        let _span = tracing::info_span!("Store::init").entered();
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

    /// Validates and optionally migrates the database schema version to match the current expected version.
    ///
    /// Queries the metadata table for the stored schema version and compares it against the current version. If the stored version is older, attempts to migrate the schema. Returns an error if the stored version is newer than the current version (indicating the database is incompatible), if the schema is corrupted, or if migration fails without a supported migration path.
    ///
    /// # Arguments
    ///
    /// `path` - The file path to the database, used for error reporting.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the schema version is valid and matches the current version, or if migration succeeds. Returns `Err(StoreError)` if the schema is newer than supported, corrupted, or migration fails.
    ///
    /// # Errors
    ///
    /// - `StoreError::SchemaNewerThanCq` - The stored schema version is newer than the current version.
    /// - `StoreError::Corruption` - The stored schema version is not a valid integer.
    /// - `StoreError::SchemaMismatch` - Schema migration is not supported for the version difference.
    /// - Other `StoreError` variants from database access or migration failures.
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

            let version: i32 = match row {
                Some((s,)) => s.parse().map_err(|e| {
                    StoreError::Corruption(format!(
                        "schema_version '{}' is not a valid integer: {}",
                        s, e
                    ))
                })?,
                // EH-22: Missing key is OK — init() hasn't been called yet on a fresh DB.
                // After init(), schema_version is guaranteed present.
                None => 0,
            };

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

    /// Validates that the stored model name and embedding dimensions match the expected values.
    ///
    /// This method checks the metadata table in the database to ensure compatibility between the current application and previously stored data. It verifies both the model name and the embedding vector dimensions.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if validation passes or if the metadata table doesn't exist yet. Returns `Err(StoreError)` if the stored model name or dimensions don't match the expected values, or if a database error occurs.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::ModelMismatch` if the stored model name differs from the expected `MODEL_NAME`.
    ///
    /// Returns `StoreError::DimensionMismatch` if the stored embedding dimensions differ from `EXPECTED_DIMENSIONS`.
    ///
    /// Returns other `StoreError` variants if database access fails.
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
                    return Err(StoreError::Corruption(format!(
                        "dimensions metadata '{}' is not a valid integer",
                        dim_str
                    )));
                }
            }

            Ok(())
        })
    }

    /// Checks if the stored CQL version in the metadata table matches the current application version.
    ///
    /// Retrieves the `cq_version` value from the metadata table and compares it against the current package version. If versions differ, logs an informational message. Errors during version retrieval are logged at debug level but do not propagate, allowing the application to continue.
    ///
    /// # Arguments
    ///
    /// `&self` - Reference to the store instance with access to the database pool and runtime.
    ///
    /// # Errors
    ///
    /// Errors are caught and logged but not propagated. Database query failures are logged at debug level.
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

        // Pre-lowercase query once for score_name_match_pre_lower (PF-3)
        let lower_name = name.to_lowercase();

        // Search name column specifically using FTS5 column filter
        // Use * for prefix matching (e.g., "parse" matches "parse_config")
        debug_assert!(
            !normalized.contains('"'),
            "sanitized query must not contain double quotes"
        );
        if normalized.contains('"') {
            return Ok(vec![]);
        }
        let fts_query = format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized);

        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id, c.parent_type_name
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

            let mut results = rows
                .into_iter()
                .map(|row| {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(&row));
                    let name_lower = chunk.name.to_lowercase();
                    let score = helpers::score_name_match_pre_lower(&name_lower, &lower_name);
                    SearchResult { chunk, score }
                })
                .collect::<Vec<_>>();

            // Re-sort by name-match score (FTS bm25 ordering may differ)
            results.sort_by(|a, b| b.score.total_cmp(&a.score));

            Ok(results)
        })
    }

    /// Compute RRF (Reciprocal Rank Fusion) scores for combining two ranked lists.
    ///
    /// Pre-allocates the HashMap with capacity for both input lists (PERF-28).
    /// Input size varies (limit*3 semantic + limit*3 FTS) but is always known upfront.
    pub(crate) fn rrf_fuse(
        semantic_ids: &[&str],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        // K=60 is the standard RRF constant from the original paper.
        // Higher K reduces the impact of rank differences (smoother fusion).
        const K: f32 = 60.0;

        let mut scores: HashMap<&str, f32> =
            HashMap::with_capacity(semantic_ids.len() + fts_ids.len());

        for (rank, id) in semantic_ids.iter().enumerate() {
            // RRF formula: 1 / (K + rank). The + 1.0 converts 0-indexed enumerate()
            // to 1-indexed ranks (first result = rank 1, not rank 0).
            let contribution = 1.0 / (K + rank as f32 + 1.0);
            *scores.entry(id).or_insert(0.0) += contribution;
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
        let refs: Vec<&str> = semantic_ids.iter().map(|s| s.as_str()).collect();
        Self::rrf_fuse(&refs, fts_ids, limit)
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

    /// Mark the HNSW index as dirty (out of sync with SQLite).
    ///
    /// Call before writing chunks to SQLite. Clear after successful HNSW save.
    /// On load, a dirty flag means a crash occurred between SQLite commit and
    /// HNSW save — the HNSW index should not be trusted.
    pub fn set_hnsw_dirty(&self, dirty: bool) -> Result<(), StoreError> {
        let val = if dirty { "1" } else { "0" };
        self.rt.block_on(async {
            sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES ('hnsw_dirty', ?1)")
                .bind(val)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    /// Check if the HNSW index is marked as dirty (potentially stale).
    ///
    /// Returns `false` if the key doesn't exist (pre-v13 indexes).
    pub fn is_hnsw_dirty(&self) -> Result<bool, StoreError> {
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'hnsw_dirty'")
                    .fetch_optional(&self.pool)
                    .await?;
            Ok(row.is_some_and(|(v,)| v == "1"))
        })
    }

    /// Set a metadata key/value pair, or delete it if `value` is `None`.
    fn set_metadata_opt(&self, key: &str, value: Option<&str>) -> Result<(), StoreError> {
        self.rt.block_on(async {
            match value {
                Some(v) => {
                    sqlx::query("INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)")
                        .bind(key)
                        .bind(v)
                        .execute(&self.pool)
                        .await?;
                }
                None => {
                    sqlx::query("DELETE FROM metadata WHERE key = ?1")
                        .bind(key)
                        .execute(&self.pool)
                        .await?;
                }
            }
            Ok(())
        })
    }

    /// Get a metadata value by key, returning `None` if the key doesn't exist.
    fn get_metadata_opt(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = ?1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await?;
            Ok(row.map(|(v,)| v))
        })
    }

    /// Store a pending LLM batch ID so interrupted processes can resume polling.
    pub fn set_pending_batch_id(&self, batch_id: Option<&str>) -> Result<(), StoreError> {
        self.set_metadata_opt("pending_llm_batch", batch_id)
    }

    /// Get the pending LLM batch ID, if any.
    pub fn get_pending_batch_id(&self) -> Result<Option<String>, StoreError> {
        self.get_metadata_opt("pending_llm_batch")
    }

    /// Store a pending doc-comment batch ID so interrupted processes can resume polling.
    pub fn set_pending_doc_batch_id(&self, batch_id: Option<&str>) -> Result<(), StoreError> {
        self.set_metadata_opt("pending_doc_batch", batch_id)
    }

    /// Get the pending doc-comment batch ID, if any.
    pub fn get_pending_doc_batch_id(&self) -> Result<Option<String>, StoreError> {
        self.get_metadata_opt("pending_doc_batch")
    }

    /// Store a pending HyDE batch ID so interrupted processes can resume polling.
    pub fn set_pending_hyde_batch_id(&self, batch_id: Option<&str>) -> Result<(), StoreError> {
        self.set_metadata_opt("pending_hyde_batch", batch_id)
    }

    /// Get the pending HyDE batch ID, if any.
    pub fn get_pending_hyde_batch_id(&self) -> Result<Option<String>, StoreError> {
        self.get_metadata_opt("pending_hyde_batch")
    }

    /// Get cached notes summaries (loaded on first call, invalidated on mutation).
    ///
    /// Returns a cloned Vec rather than a slice reference to avoid holding the
    /// RwLock read guard across caller code. The clone cost is negligible — notes
    /// are typically <100 entries with small strings.
    pub fn cached_notes_summaries(&self) -> Result<Vec<NoteSummary>, StoreError> {
        {
            let guard = self.notes_summaries_cache.read().unwrap_or_else(|p| {
                tracing::warn!("notes cache read lock poisoned, recovering");
                p.into_inner()
            });
            if let Some(ref ns) = *guard {
                return Ok(ns.clone());
            }
        }
        // Cache miss — load from DB and populate
        let ns = self.list_notes_summaries()?;
        {
            let mut guard = self.notes_summaries_cache.write().unwrap_or_else(|p| {
                tracing::warn!("notes cache write lock poisoned, recovering");
                p.into_inner()
            });
            *guard = Some(ns.clone());
        }
        Ok(ns)
    }

    /// Invalidate the cached notes summaries.
    ///
    /// Must be called after any operation that modifies notes (upsert, replace, delete)
    /// so subsequent reads see fresh data.
    pub(crate) fn invalidate_notes_cache(&self) {
        match self.notes_summaries_cache.write() {
            Ok(mut guard) => *guard = None,
            Err(p) => {
                tracing::warn!("notes cache write lock poisoned during invalidation, recovering");
                *p.into_inner() = None;
            }
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
    /// Performs a best-effort WAL (Write-Ahead Logging) checkpoint when the Store is dropped to prevent accumulation of large WAL files.
    ///
    /// # Arguments
    ///
    /// * `&mut self` - A mutable reference to the Store instance being dropped
    ///
    /// # Returns
    ///
    /// Nothing. Errors during checkpoint are logged as warnings but not propagated, as Drop implementations cannot fail.
    ///
    /// # Panics
    ///
    /// Does not panic. Uses `catch_unwind` to safely handle potential panics from `block_on` when called from within an async context (e.g., dropping Store inside a tokio runtime).
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

    // ===== TC-8: pending batch ID =====

    use crate::test_helpers::setup_store;

    #[test]
    fn test_pending_batch_roundtrip() {
        let (store, _dir) = setup_store();
        store.set_pending_batch_id(Some("batch_123")).unwrap();
        let result = store.get_pending_batch_id().unwrap();
        assert_eq!(result, Some("batch_123".to_string()));
    }

    #[test]
    fn test_pending_batch_clear() {
        let (store, _dir) = setup_store();
        store.set_pending_batch_id(Some("batch_abc")).unwrap();
        store.set_pending_batch_id(None).unwrap();
        let result = store.get_pending_batch_id().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_pending_batch_default_none() {
        let (store, _dir) = setup_store();
        let result = store.get_pending_batch_id().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_pending_batch_overwrite() {
        let (store, _dir) = setup_store();
        store.set_pending_batch_id(Some("a")).unwrap();
        store.set_pending_batch_id(Some("b")).unwrap();
        let result = store.get_pending_batch_id().unwrap();
        assert_eq!(result, Some("b".to_string()));
    }

    // ===== TC-10: HNSW dirty flag =====

    #[test]
    fn test_hnsw_dirty_roundtrip() {
        let (store, _dir) = setup_store();
        store.set_hnsw_dirty(true).unwrap();
        assert!(store.is_hnsw_dirty().unwrap());
    }

    #[test]
    fn test_hnsw_dirty_default_false() {
        let (store, _dir) = setup_store();
        assert!(!store.is_hnsw_dirty().unwrap());
    }

    #[test]
    fn test_hnsw_dirty_toggle() {
        let (store, _dir) = setup_store();
        store.set_hnsw_dirty(true).unwrap();
        assert!(store.is_hnsw_dirty().unwrap());

        store.set_hnsw_dirty(false).unwrap();
        assert!(!store.is_hnsw_dirty().unwrap());

        store.set_hnsw_dirty(true).unwrap();
        assert!(store.is_hnsw_dirty().unwrap());
    }

    // ===== TC-16: cache invalidation =====

    #[test]
    fn test_cached_notes_empty() {
        let (store, _dir) = setup_store();
        let notes = store.cached_notes_summaries().unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn test_cached_notes_invalidation() {
        let (store, dir) = setup_store();

        let source = dir.path().join("notes.toml");
        std::fs::write(&source, "# dummy").unwrap();

        // Insert first batch of notes
        let note1 = crate::note::Note {
            id: "note:0".to_string(),
            text: "first note".to_string(),
            sentiment: 0.0,
            mentions: vec![],
        };
        store.upsert_notes_batch(&[note1], &source, 100).unwrap();

        // Populate cache
        let cached = store.cached_notes_summaries().unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].text, "first note");

        // Replace notes with a different set (replace_notes_for_file invalidates cache)
        let note2 = crate::note::Note {
            id: "note:0".to_string(),
            text: "replaced note".to_string(),
            sentiment: 0.5,
            mentions: vec!["src/lib.rs".to_string()],
        };
        store
            .replace_notes_for_file(&[note2], &source, 200)
            .unwrap();

        // Cache should reflect the replacement
        let cached = store.cached_notes_summaries().unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].text, "replaced note");
    }

    // ===== TC-17: check_model_version tests =====

    fn make_test_store_initialized() -> (Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&ModelInfo::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn tc17_model_mismatch_returns_error() {
        let (store, _dir) = make_test_store_initialized();
        store
            .set_metadata_opt("model_name", Some("wrong-model/v1"))
            .unwrap();
        let err = store.check_model_version().unwrap_err();
        assert!(
            matches!(err, StoreError::ModelMismatch(..)),
            "Expected ModelMismatch, got: {:?}",
            err
        );
    }

    #[test]
    fn tc17_dimension_mismatch_returns_error() {
        let (store, _dir) = make_test_store_initialized();
        store.set_metadata_opt("dimensions", Some("999")).unwrap();
        let err = store.check_model_version().unwrap_err();
        assert!(
            matches!(err, StoreError::DimensionMismatch(999, _)),
            "Expected DimensionMismatch, got: {:?}",
            err
        );
    }

    #[test]
    fn tc17_corrupt_dimension_returns_corruption() {
        let (store, _dir) = make_test_store_initialized();
        store
            .set_metadata_opt("dimensions", Some("not_a_number"))
            .unwrap();
        let err = store.check_model_version().unwrap_err();
        assert!(
            matches!(err, StoreError::Corruption(..)),
            "Expected Corruption, got: {:?}",
            err
        );
    }

    #[test]
    fn tc17_correct_model_passes() {
        let (store, _dir) = make_test_store_initialized();
        assert!(store.check_model_version().is_ok());
    }

    // ===== TC-18: check_schema_version tests =====

    #[test]
    fn tc18_schema_newer_returns_error() {
        let (store, _dir) = make_test_store_initialized();
        let future_version = (CURRENT_SCHEMA_VERSION + 1).to_string();
        store
            .set_metadata_opt("schema_version", Some(&future_version))
            .unwrap();
        let err = store
            .check_schema_version(std::path::Path::new("/test"))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::SchemaNewerThanCq(..)),
            "Expected SchemaNewerThanCq, got: {:?}",
            err
        );
    }

    #[test]
    fn tc18_corrupt_schema_version_returns_corruption() {
        let (store, _dir) = make_test_store_initialized();
        store
            .set_metadata_opt("schema_version", Some("garbage"))
            .unwrap();
        let err = store
            .check_schema_version(std::path::Path::new("/test"))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Corruption(..)),
            "Expected Corruption, got: {:?}",
            err
        );
    }

    #[test]
    fn tc18_current_schema_passes() {
        let (store, _dir) = make_test_store_initialized();
        assert!(store
            .check_schema_version(std::path::Path::new("/test"))
            .is_ok());
    }

    #[test]
    fn tc18_missing_schema_key_passes() {
        // Fresh DB with metadata table but no schema_version key
        let (store, _dir) = make_test_store_initialized();
        store.rt.block_on(async {
            sqlx::query("DELETE FROM metadata WHERE key = 'schema_version'")
                .execute(&store.pool)
                .await
                .unwrap();
        });
        assert!(store
            .check_schema_version(std::path::Path::new("/test"))
            .is_ok());
    }

    // ===== TC-19: concurrent access and edge-case tests =====

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
