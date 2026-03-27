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

mod calls;
mod chunks;
mod metadata;
mod migrations;
mod notes;
mod search;
mod types;

/// Helper types and embedding conversion functions.
///
/// This module is `pub(crate)` - external consumers should use the re-exported
/// types from `cqs::store` instead of accessing `cqs::store::helpers` directly.
pub(crate) mod helpers;

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

/// Name of the embedding model (compile-time default for E5-base-v2).
///
/// Runtime code should use `Store::stored_model_name()` or `ModelInfo::new()`.
/// This constant exists for callers outside the store (e.g. `doctor.rs`).
pub const MODEL_NAME: &str = crate::embedder::DEFAULT_MODEL_REPO;

/// Expected embedding dimensions (compile-time default for E5-base-v2).
///
/// Runtime code should use `Store::dim` instead. This constant exists for
/// callers outside the store that need a compile-time value.
pub const EXPECTED_DIMENSIONS: u32 = crate::EMBEDDING_DIM as u32;

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
    /// Embedding dimension for this store (read from metadata on open, default `EMBEDDING_DIM`).
    pub(crate) dim: usize,
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
    /// Embedding dimension for vectors in this store.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Update the embedding dimension after init (fresh DB only).
    ///
    /// `Store::open` defaults to `EMBEDDING_DIM` when the metadata table doesn't
    /// exist yet. After `init()` writes the correct dim, call this to sync.
    pub fn set_dim(&mut self, dim: usize) {
        self.dim = dim;
    }

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
        rt.block_on(async {
            let result: (String,) = sqlx::query_as("PRAGMA integrity_check(1)")
                .fetch_one(&pool)
                .await?;
            if result.0 != "ok" {
                return Err(StoreError::Corruption(result.0));
            }
            Ok::<_, StoreError>(())
        })?;

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
            call_graph_cache: std::sync::OnceLock::new(),
            test_chunks_cache: std::sync::OnceLock::new(),
        };

        // Skip model name validation on open — dimension is validated at embed time,
        // and configurable models (v1.7.0) can legitimately use any model name.
        // Model mismatch is checked at index time via check_model_version_with().
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
