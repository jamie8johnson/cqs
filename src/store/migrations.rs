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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(CURRENT_SCHEMA_VERSION, 11);
    }
}
