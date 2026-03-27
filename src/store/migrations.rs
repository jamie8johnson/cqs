//! Schema migrations for cqs index database
//!
//! When the schema version changes, migrations allow upgrading existing indexes
//! without requiring a full rebuild (`cqs index --force`).
//!
//! ## Adding a new migration
//!
//! 1. Increment `CURRENT_SCHEMA_VERSION` in `helpers.rs`
//! 2. Add a new migration function: `async fn migrate_vN_to_vM(pool: &SqlitePool) -> Result<()>`
//! 3. Add the case to `run_migration()`: `(N, M) => migrate_vN_to_vM(pool).await`
//! 4. Update `schema.sql` with the new schema
//!
//! ## Migration guidelines
//!
//! - Most changes are additive (new columns, new tables) - these preserve data
//! - For new columns with NOT NULL, use DEFAULT or populate from existing data
//! - Test migrations with real indexes before release
//! - Keep migrations idempotent where possible (use IF NOT EXISTS)

use sqlx::SqlitePool;

use super::helpers::StoreError;

// Used by tests and future migrations
#[allow(unused_imports)]
use super::helpers::CURRENT_SCHEMA_VERSION;

/// Run all migrations from stored version to current version
pub async fn migrate(pool: &SqlitePool, from: i32, to: i32) -> Result<(), StoreError> {
    if from == to {
        return Ok(()); // Already at target version
    }
    if from > to {
        return Err(StoreError::SchemaNewerThanCq(from));
    }

    tracing::info!(
        from_version = from,
        to_version = to,
        "Starting schema migration"
    );

    let mut tx = pool.begin().await?;
    for version in from..to {
        tracing::info!(from = version, to = version + 1, "Running migration step");
        run_migration(&mut tx, version, version + 1).await?;
    }
    sqlx::query("UPDATE metadata SET value = ?1 WHERE key = 'schema_version'")
        .bind(to.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::info!(new_version = to, "Schema migration complete");

    Ok(())
}

/// Run a single migration step
#[allow(clippy::match_single_binding)] // Intentional: migration arms will be added here
async fn run_migration(
    conn: &mut sqlx::SqliteConnection,
    from: i32,
    to: i32,
) -> Result<(), StoreError> {
    match (from, to) {
        (10, 11) => migrate_v10_to_v11(conn).await,
        (11, 12) => migrate_v11_to_v12(conn).await,
        (12, 13) => migrate_v12_to_v13(conn).await,
        (13, 14) => migrate_v13_to_v14(conn).await,
        (14, 15) => migrate_v14_to_v15(conn).await,
        (15, 16) => migrate_v15_to_v16(conn).await,
        _ => Err(StoreError::MigrationNotSupported(from, to)),
    }
}

// ============================================================================
// Migration functions
// ============================================================================

/// Migrate from v10 to v11: add type_edges table
///
/// Adds type-level dependency tracking. Each edge records which chunk references
/// which type, with an edge_kind classification (Param, Return, Field, Impl, Bound, Alias).
/// Catch-all types (inside generics, etc.) use empty string '' for edge_kind instead of NULL
/// to simplify WHERE clause filtering.
///
/// The table will be empty after migration — run `cqs index --force` to populate.
async fn migrate_v10_to_v11(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS type_edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_chunk_id TEXT NOT NULL,
            target_type_name TEXT NOT NULL,
            edge_kind TEXT NOT NULL DEFAULT '',
            line_number INTEGER NOT NULL,
            FOREIGN KEY (source_chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
        )",
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_type_edges_source ON type_edges(source_chunk_id)")
        .execute(&mut *conn)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_type_edges_target ON type_edges(target_type_name)")
        .execute(&mut *conn)
        .await?;

    tracing::info!("Created type_edges table. Run 'cqs index --force' to populate type edges.");
    Ok(())
}

/// Migrate from v11 to v12: add parent_type_name column to chunks
///
/// Stores the enclosing class/struct/impl name for method chunks.
/// The column will be NULL after migration — run `cqs index --force` to populate.
async fn migrate_v11_to_v12(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    sqlx::query("ALTER TABLE chunks ADD COLUMN parent_type_name TEXT")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Added parent_type_name column. Run 'cqs index --force' to populate method→class links."
    );
    Ok(())
}

/// Migrate from v12 to v13: enrichment idempotency + HNSW dirty flag
///
/// - `enrichment_hash` column on chunks: blake3 hash of call context used during
///   enrichment. NULL means not yet enriched. Allows skipping already-enriched
///   chunks on re-index and detecting partial enrichment after crash.
/// - `hnsw_dirty` metadata key: set to "1" before SQLite chunk writes, cleared
///   to "0" after successful HNSW save. Detects crash between the two writes.
async fn migrate_v12_to_v13(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    sqlx::query("ALTER TABLE chunks ADD COLUMN enrichment_hash TEXT")
        .execute(&mut *conn)
        .await?;

    sqlx::query("INSERT OR IGNORE INTO metadata (key, value) VALUES ('hnsw_dirty', '0')")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Added enrichment_hash column and hnsw_dirty flag. \
         Run 'cqs index --force' to populate enrichment hashes."
    );
    Ok(())
}

/// Migrate from v13 to v14: LLM summaries cache table (SQ-6)
///
/// Stores one-sentence LLM-generated summaries keyed by content_hash.
/// Summaries survive chunk deletion and --force rebuilds.
async fn migrate_v13_to_v14(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS llm_summaries (
            content_hash TEXT PRIMARY KEY,
            summary TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .execute(&mut *conn)
    .await?;

    tracing::info!("Created llm_summaries table for LLM-generated function summaries.");
    Ok(())
}

/// Migrate from v14 to v15: 768-dim embeddings (SQ-9)
///
/// Dropped the sentiment dimension — embeddings are now pure 768-dim E5-base-v2 output.
/// - Updates dimensions metadata from 769 to 768
/// - Sets hnsw_dirty to trigger HNSW rebuild (old index has 769-dim vectors)
/// - Notes embedding column is left as-is (we write empty blobs now, old data is harmless)
async fn migrate_v14_to_v15(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    sqlx::query("UPDATE metadata SET value = '768' WHERE key = 'dimensions'")
        .execute(&mut *conn)
        .await?;

    sqlx::query("UPDATE metadata SET value = '1' WHERE key = 'hnsw_dirty'")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Updated dimensions to 768 and marked HNSW dirty. \
         Run 'cqs index --force' to rebuild with 768-dim embeddings."
    );
    Ok(())
}

/// Migrate from v15 to v16: composite PK on llm_summaries (content_hash, purpose)
///
/// Recreates llm_summaries with a composite primary key so the same content_hash
/// can have multiple summaries for different purposes (e.g., 'summary', 'doc-comment').
/// Existing rows get purpose='summary' as the default.
///
/// Safety: CREATE TABLE, INSERT INTO ... SELECT, DROP TABLE, and ALTER TABLE RENAME
/// are all transactional in SQLite (they write to sqlite_master within the same
/// transaction). If any step fails, the entire migration rolls back and the original
/// llm_summaries table remains intact. The caller (`migrate`) wraps all steps in a
/// single BEGIN/COMMIT via `pool.begin()`.
async fn migrate_v15_to_v16(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS llm_summaries_v2 (
            content_hash TEXT NOT NULL,
            purpose TEXT NOT NULL DEFAULT 'summary',
            summary TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (content_hash, purpose)
        )",
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        "INSERT OR IGNORE INTO llm_summaries_v2 (content_hash, purpose, summary, model, created_at) \
         SELECT content_hash, 'summary', summary, model, created_at FROM llm_summaries",
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query("DROP TABLE llm_summaries")
        .execute(&mut *conn)
        .await?;

    sqlx::query("ALTER TABLE llm_summaries_v2 RENAME TO llm_summaries")
        .execute(&mut *conn)
        .await?;

    tracing::info!("Recreated llm_summaries with composite PK (content_hash, purpose).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn test_migration_not_supported_error() {
        // Verify unknown migrations produce clear errors
        let err = StoreError::MigrationNotSupported(5, 6);
        let msg = err.to_string();
        assert!(msg.contains("5"));
        assert!(msg.contains("6"));
    }

    #[test]
    fn test_current_schema_version_documented() {
        // Ensure the current version matches what we document
        assert_eq!(CURRENT_SCHEMA_VERSION, 16);
    }

    #[test]
    fn test_migrate_noop_same_version() {
        // Migration from N to N should be a no-op
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            let result = migrate(&pool, 15, 15).await;
            assert!(result.is_ok(), "same-version migration should be no-op");
        });
    }

    #[test]
    fn test_migrate_rejects_downgrade() {
        // from > to should error with SchemaNewerThanCq
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            let result = migrate(&pool, 15, 14).await;
            assert!(result.is_err(), "downgrade should fail");
            match result.unwrap_err() {
                StoreError::SchemaNewerThanCq(v) => assert_eq!(v, 15),
                other => panic!("Expected SchemaNewerThanCq, got: {:?}", other),
            }
        });
    }

    #[test]
    fn test_migrate_v10_to_v11_creates_type_edges() {
        // Full migration test: set up a v10 schema, run migration, verify type_edges exists
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create the minimal schema that a v10 store would have
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    language TEXT NOT NULL DEFAULT '',
                    chunk_type TEXT NOT NULL DEFAULT '',
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL DEFAULT '',
                    content TEXT NOT NULL,
                    doc TEXT,
                    line_start INTEGER NOT NULL DEFAULT 0,
                    line_end INTEGER NOT NULL DEFAULT 0,
                    parent_id TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Set schema_version to 10
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '10')")
                .execute(&pool)
                .await
                .unwrap();

            // Verify type_edges does NOT exist before migration
            let table_check: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='type_edges'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(table_check.is_none(), "type_edges should not exist yet");

            // Run migration from v10 to v11
            migrate(&pool, 10, 11).await.unwrap();

            // Verify type_edges now exists
            let table_check: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='type_edges'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(
                table_check.is_some(),
                "type_edges should exist after migration"
            );

            // Verify schema_version was updated to 11
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "11");

            // Verify the indexes were created
            let idx_source: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='index' AND name='idx_type_edges_source'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(idx_source.is_some(), "source index should exist");

            let idx_target: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='index' AND name='idx_type_edges_target'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(idx_target.is_some(), "target index should exist");
        });
    }

    #[test]
    fn test_migrate_v12_to_v13() {
        // Full migration test: set up a v12 schema, run migration, verify enrichment_hash + hnsw_dirty
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create v12 schema: chunks WITHOUT enrichment_hash, metadata WITHOUT hnsw_dirty
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    doc TEXT,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    source_mtime INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    parent_id TEXT,
                    window_idx INTEGER,
                    parent_type_name TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '12')")
                .execute(&pool)
                .await
                .unwrap();

            // Run migration from v12 to v13
            migrate(&pool, 12, 13).await.unwrap();

            // Verify enrichment_hash column exists by inserting a row that uses it
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at, enrichment_hash) \
                 VALUES ('test', 'file:test.rs', 'file', 'rust', 'function', 'test_fn', \
                 '', 'fn test() {}', 'abc123', 0, 1, X'00', '2026-01-01', '2026-01-01', 'hash123')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Verify hnsw_dirty metadata key exists with value '0'
            let dirty: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'hnsw_dirty'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(dirty.0, "0");

            // Verify schema_version was updated to 13
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "13");
        });
    }

    #[test]
    fn test_migrate_v13_to_v14() {
        // Full migration test: set up a v13 schema, run migration, verify llm_summaries table
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create v13 schema: chunks WITH enrichment_hash, metadata WITH hnsw_dirty
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    doc TEXT,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    source_mtime INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    parent_id TEXT,
                    window_idx INTEGER,
                    parent_type_name TEXT,
                    enrichment_hash TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '13')")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('hnsw_dirty', '0')")
                .execute(&pool)
                .await
                .unwrap();

            // Verify llm_summaries does NOT exist before migration
            let table_check: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='llm_summaries'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(table_check.is_none(), "llm_summaries should not exist yet");

            // Run migration from v13 to v14
            migrate(&pool, 13, 14).await.unwrap();

            // Verify llm_summaries table exists
            let table_check: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='llm_summaries'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(
                table_check.is_some(),
                "llm_summaries should exist after migration"
            );

            // Verify we can insert into llm_summaries
            sqlx::query(
                "INSERT INTO llm_summaries (content_hash, summary, model, created_at) \
                 VALUES ('abc123', 'Test summary', 'claude-4', '2026-01-01')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Verify schema_version was updated to 14
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "14");
        });
    }

    #[test]
    fn test_migrate_v14_to_v15() {
        // Full migration test: set up a v14 schema, run migration, verify dimensions + hnsw_dirty
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create v14 schema: chunks WITH enrichment_hash, llm_summaries table
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    doc TEXT,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    source_mtime INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    parent_id TEXT,
                    window_idx INTEGER,
                    parent_type_name TEXT,
                    enrichment_hash TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS llm_summaries (
                    content_hash TEXT PRIMARY KEY,
                    summary TEXT NOT NULL,
                    model TEXT NOT NULL,
                    created_at TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '14')")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('dimensions', '769')")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('hnsw_dirty', '0')")
                .execute(&pool)
                .await
                .unwrap();

            // Run migration from v14 to v15
            migrate(&pool, 14, 15).await.unwrap();

            // Verify dimensions updated to 768
            let dims: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'dimensions'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(dims.0, "768", "dimensions should be updated to 768");

            // Verify hnsw_dirty set to 1 (triggers rebuild)
            let dirty: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'hnsw_dirty'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(dirty.0, "1", "hnsw_dirty should be set to 1");

            // Verify schema_version was updated to 15
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "15");
        });
    }

    #[test]
    fn test_migrate_v15_to_v16() {
        // Full migration test: set up a v15 schema, run migration, verify composite PK
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create v15 schema with llm_summaries (single PK on content_hash)
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS llm_summaries (
                    content_hash TEXT PRIMARY KEY,
                    summary TEXT NOT NULL,
                    model TEXT NOT NULL,
                    created_at TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '15')")
                .execute(&pool)
                .await
                .unwrap();

            // Insert two test summaries
            sqlx::query(
                "INSERT INTO llm_summaries (content_hash, summary, model, created_at) \
                 VALUES ('hash_a', 'Summary A', 'claude-4', '2026-01-01')",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO llm_summaries (content_hash, summary, model, created_at) \
                 VALUES ('hash_b', 'Summary B', 'claude-4', '2026-01-02')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Run migration from v15 to v16
            migrate(&pool, 15, 16).await.unwrap();

            // Verify existing rows have purpose='summary'
            let count: (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM llm_summaries WHERE purpose = 'summary'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                count.0, 2,
                "both existing rows should have purpose='summary'"
            );

            // Verify composite PK: same content_hash with different purpose should succeed
            sqlx::query(
                "INSERT INTO llm_summaries (content_hash, purpose, summary, model, created_at) \
                 VALUES ('hash_a', 'doc-comment', 'Doc comment A', 'claude-4', '2026-01-03')",
            )
            .execute(&pool)
            .await
            .expect("inserting same content_hash with different purpose should succeed");

            // Verify we now have 3 rows total
            let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM llm_summaries")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(count.0, 3, "should have 3 rows after inserting doc-comment");

            // Verify schema_version was updated to 16
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "16");
        });
    }

    #[test]
    fn test_migrate_v12_to_v14_full_chain() {
        // Full chain migration: v12 → v13 → v14 in one call
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create v12 schema: chunks WITHOUT enrichment_hash, no hnsw_dirty
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    doc TEXT,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    source_mtime INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    parent_id TEXT,
                    window_idx INTEGER,
                    parent_type_name TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '12')")
                .execute(&pool)
                .await
                .unwrap();

            // Run full chain migration from v12 to v14
            migrate(&pool, 12, 14).await.unwrap();

            // Verify enrichment_hash column exists (from v12→v13)
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at, enrichment_hash) \
                 VALUES ('test', 'file:test.rs', 'file', 'rust', 'function', 'test_fn', \
                 '', 'fn test() {}', 'abc123', 0, 1, X'00', '2026-01-01', '2026-01-01', 'hash123')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Verify llm_summaries table exists (from v13→v14)
            let table_check: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='llm_summaries'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(
                table_check.is_some(),
                "llm_summaries should exist after full chain migration"
            );

            // Verify schema_version was updated to 14
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "14");
        });
    }

    #[test]
    fn test_migrate_unsupported_version_range() {
        // Migration from an unsupported range should fail with MigrationNotSupported
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            // Create metadata table so the SQL doesn't fail on table-not-found
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '8')")
                .execute(&pool)
                .await
                .unwrap();

            let result = migrate(&pool, 8, 11).await;
            assert!(result.is_err(), "unsupported range should fail");
            match result.unwrap_err() {
                StoreError::MigrationNotSupported(from, to) => {
                    assert_eq!(from, 8);
                    assert_eq!(to, 9);
                }
                other => panic!("Expected MigrationNotSupported, got: {:?}", other),
            }
        });
    }
}
