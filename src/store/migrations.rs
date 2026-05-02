//! Schema migrations for cqs index database
//!
//! When the schema version changes, migrations allow upgrading existing indexes
//! without requiring a full rebuild (`cqs index --force`).
//!
//! ## Adding a new migration
//!
//! 1. Increment `CURRENT_SCHEMA_VERSION` in `helpers/mod.rs`
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

use std::path::Path;

use sqlx::SqlitePool;

use super::backup;
use super::helpers::StoreError;

// Used by tests and future migrations
#[allow(unused_imports)]
use super::helpers::CURRENT_SCHEMA_VERSION;

/// Run all migrations from stored version to current version.
///
/// ## Backup and recovery (issue #953)
///
/// Before any DDL runs, a filesystem snapshot of `db_path` (and its
/// `-wal`/`-shm` sidecars) is taken to a sibling `.bak-v{from}-v{to}-{ts}.db`
/// file via `crate::fs::atomic_replace`. This covers two failure modes that
/// SQLite's transactional rollback does not:
///
/// 1. A commit-time I/O failure mid-WAL-write (disk full, fs quota, network
///    FS disconnect, user pulling USB). The in-memory pool state can think
///    the rollback completed while the on-disk file sees partial pages.
/// 2. A bug inside a migration function that writes logically-inconsistent
///    state — the transaction commits cleanly but the data is wrong.
///
/// On any migration error, the DB is restored from the backup atomically so
/// the caller sees either pre-migrate or post-migrate state, never a
/// partial write. On success, the newest two backups (including this one)
/// are retained and older ones are pruned.
///
/// If the backup step itself fails (e.g. parent dir is read-only, disk full),
/// the migration aborts by default with `StoreError::Io` — destructive
/// migrations (v18→v19 drops the old `sparse_vectors` table) without a
/// recovery snapshot are a data-loss hazard on a subsequent commit failure.
/// Setting `CQS_MIGRATE_REQUIRE_BACKUP=0` downgrades that to a `warn!` and
/// proceeds without a snapshot for users who accept the risk (tight-quota
/// filesystems, CI rebuilding from source).
///
/// ## Concurrent-migrate safety (v1.22.0 audit DS-W6)
///
/// Re-reads `schema_version` inside the migration transaction before executing
/// DDL. Two concurrent processes both reading version=17 from the pool, then
/// both running `ALTER TABLE ADD COLUMN`, would crash the second with
/// "duplicate column name" on a perfectly healthy DB. The double-check under
/// the transaction's implicit exclusive lock prevents this: the second
/// process sees the version has already advanced and short-circuits.
///
/// ## Pool ownership (P2.59 / issue #1125)
///
/// `migrate` takes ownership of `pool` by value because the failure path must
/// `.close().await` the pool before `restore_from_backup` runs `atomic_replace`
/// over `db_path`. SQLite's in-process pool holds file descriptors against the
/// old inode; if those descriptors stay open across the file replace, queries
/// through them see the unlinked-old inode while readers from new processes
/// see the restored DB. The WAL/SHM sidecars copied alongside the main DB land
/// on the new inode, but the pool's mmap'd sidecars belong to the old —
/// silent two-state divergence.
///
/// Returns:
/// - `Ok(pool)` on success: the pool is the same one passed in, still usable.
/// - `Err(_)` on failure: the pool has been **consumed and closed** as part of
///   the restore protocol (when a backup was taken) or is dropped silently
///   (when no backup was available). The caller must reopen a fresh pool
///   against `db_path` to continue. The DB on disk is in its pre-migrate state
///   on the with-backup path, or in whatever state the rolled-back transaction
///   left it on the no-backup path (typically pre-migrate because all DDL ran
///   inside a single `pool.begin()`).
pub async fn migrate(
    pool: SqlitePool,
    db_path: &Path,
    from: i32,
    to: i32,
) -> Result<SqlitePool, StoreError> {
    let _span = tracing::info_span!("migrate", from, to).entered();

    if from == to {
        // Fast path: no work to do. Do NOT take a backup — this path runs
        // on every `cqs` command when the DB is already at the current
        // version, and a disk write here would be unacceptable overhead.
        return Ok(pool);
    }
    if from > to {
        return Err(StoreError::SchemaNewerThanCq(from));
    }

    tracing::info!(
        from_version = from,
        to_version = to,
        "Starting schema migration"
    );

    // Snapshot the DB before any DDL runs. On failure the restore path uses
    // `atomic_replace` to put the DB back in its pre-migrate state.
    //
    // Borrowing &pool here is fine: backup_before_migrate runs a
    // `wal_checkpoint(FULL)` and a file copy; we still own the pool when it
    // returns, and the failure-path close-and-restore happens below.
    let backup_path = backup::backup_before_migrate(&pool, db_path, from, to).await?;

    match run_migration_tx(&pool, from, to).await {
        Ok(()) => {
            tracing::info!(new_version = to, "Schema migration complete");
            // Best-effort prune of older backups; failure here is not a
            // migration failure — the user's DB is at the correct version.
            if let Err(e) = backup::prune_old_backups(db_path) {
                tracing::warn!(error = %e, "Failed to prune old migration backups");
            }
            Ok(pool)
        }
        Err(e) => {
            // P2.59: close the pool BEFORE atomic_replace overwrites the DB
            // file. Otherwise pool descriptors keep mmap'ing the unlinked
            // old inode while subsequent opens see the restored backup —
            // silent two-state divergence. Drain WAL first so the on-disk
            // bytes after close reflect the post-DDL state we're about to
            // discard, and the restore overwrites a quiesced file.
            if let Some(ref bak) = backup_path {
                if let Err(checkpoint_err) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                    .execute(&pool)
                    .await
                {
                    tracing::warn!(
                        error = %checkpoint_err,
                        "wal_checkpoint(TRUNCATE) before restore failed (non-fatal)"
                    );
                }
                pool.close().await;
                // The pool is closed; descriptors against the old inode are
                // released. Now atomic_replace can safely swap the DB file.

                match backup::restore_from_backup(db_path, bak) {
                    Ok(()) => {
                        tracing::warn!(
                            error = %e,
                            backup = %bak.display(),
                            "Migration failed; pool closed and DB restored from backup"
                        );
                    }
                    Err(restore_err) => {
                        // We can't put the DB back. Surface both errors in
                        // the log — the user needs to manually `cqs index
                        // --force` or copy `bak` into place.
                        tracing::error!(
                            migration_error = %e,
                            restore_error = %restore_err,
                            backup = %bak.display(),
                            db = %db_path.display(),
                            "Migration failed AND restore failed. \
                             Manually copy the backup into place or run \
                             'cqs index --force'."
                        );
                    }
                }
                Err(e)
            } else {
                // No backup was taken (env opt-out): the migration's DDL ran
                // inside a transaction that already rolled back, so the DB
                // file is still in its pre-migrate state. The pool was never
                // closed, but we drop it here to keep the API uniform —
                // callers receiving Err must reopen regardless of which
                // failure path fired.
                tracing::warn!(
                    error = %e,
                    db = %db_path.display(),
                    "Migration failed and no backup was available for restore. \
                     Run 'cqs index --force' to rebuild from source."
                );
                drop(pool);
                Err(e)
            }
        }
    }
}

/// Read the stored `schema_version` and migrate to the current version if
/// needed. Designed to be called from `Store::open` *before* the `Store`
/// struct is constructed so the pool can be handed off to `migrate()` by
/// value — a hard requirement of the P2.59 close-and-restore protocol.
///
/// Returns the (possibly-the-same, possibly-migrated) pool on success.
/// On migration failure the pool is consumed (see [`migrate`]); the caller
/// must reopen if they want to continue.
///
/// `current_version` is normally [`super::helpers::CURRENT_SCHEMA_VERSION`],
/// passed in as a parameter so tests can target intermediate versions
/// without flipping a process-global constant.
pub async fn check_and_migrate_schema(
    pool: SqlitePool,
    db_path: &Path,
    current_version: i32,
) -> Result<SqlitePool, StoreError> {
    let _span = tracing::info_span!("check_and_migrate_schema").entered();

    // Read the stored schema version. A "no such table" error means the
    // metadata table hasn't been created yet (fresh DB pre-init), which is
    // a legitimate post-open state — return the pool untouched.
    let row: Option<(String,)> =
        match sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
            .fetch_optional(&pool)
            .await
        {
            Ok(r) => r,
            Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => {
                return Ok(pool);
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
        // EH-22: missing key is OK — init() hasn't been called yet on a
        // fresh DB. After init(), schema_version is guaranteed present.
        None => 0,
    };

    if version > current_version {
        return Err(StoreError::SchemaNewerThanCq(version));
    }
    if version <= 0 || version >= current_version {
        // Either fresh-DB sentinel (0) or already current — no migration.
        return Ok(pool);
    }

    // Migration needed. Hand the pool off by value so migrate() can close it
    // around the file replace. On failure the pool is consumed; surface
    // SchemaMismatch for unsupported migrations so the CLI gets a clearer
    // error than the raw MigrationNotSupported (which encodes from/to as
    // anonymous integers).
    match migrate(pool, db_path, version, current_version).await {
        Ok(p) => {
            tracing::info!(
                path = %db_path.display(),
                from = version,
                to = current_version,
                "Schema migrated successfully"
            );
            Ok(p)
        }
        Err(StoreError::MigrationNotSupported { from, to }) => Err(StoreError::SchemaMismatch {
            db_path: db_path.display().to_string(),
            found: from,
            expected: to,
        }),
        Err(e) => Err(e),
    }
}

/// Run the migration transaction: re-check version under the write lock,
/// dispatch each version step, stamp the new `schema_version`, commit.
///
/// Split out of `migrate()` so the caller can always invoke the
/// backup-and-restore pipeline regardless of how the transaction fails.
async fn run_migration_tx(pool: &SqlitePool, from: i32, to: i32) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;

    // DS-W6: re-read version under the write lock. A concurrent process may
    // have already migrated between our caller's pool-level read and our
    // transaction start. If the version is already at or past `to`, bail.
    let current_in_tx: Option<(String,)> =
        sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
            .fetch_optional(&mut *tx)
            .await?;
    let actual_from: i32 = current_in_tx
        .and_then(|(s,)| s.parse().ok())
        .unwrap_or(from);
    if actual_from >= to {
        tracing::info!(
            actual_from,
            to,
            "Schema already migrated by another process, skipping"
        );
        tx.rollback().await?;
        return Ok(());
    }

    for version in actual_from..to {
        tracing::info!(from = version, to = version + 1, "Running migration step");
        run_migration(&mut tx, version, version + 1).await?;

        // Test-only hook: when the thread-local `TEST_FAIL_AFTER_VERSION`
        // is set to N by a test, return an error without committing so
        // the test can exercise the backup-restore path. Per-thread (not
        // a process-global atomic or env var) so tests running in parallel
        // don't inject failures into each other.
        //
        // Gated on `cfg(test)` so it cannot be triggered in a release binary.
        #[cfg(test)]
        {
            let target = tests::TEST_FAIL_AFTER_VERSION.with(|c| c.get());
            if target != 0 && target == version + 1 {
                return Err(StoreError::Runtime(format!(
                    "test hook: injected failure after migration step v{} -> v{}",
                    version,
                    version + 1
                )));
            }
        }
    }
    // E.1 (P1 #16): use UPSERT instead of UPDATE so a DB without an existing
    // `schema_version` metadata row gets one stamped on first migration. The
    // UPDATE form silently affected zero rows, leaving the version unstamped
    // and causing the next open to re-run the same DDL ("duplicate column
    // name" / "table already exists" errors). Mirrors the pattern used for
    // `splade_generation` in `migrate_v18_to_v19` / `migrate_v19_to_v20`.
    sqlx::query(
        "INSERT INTO metadata (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(to.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(())
}

/// P3-48: registered migration step. Each row pairs a `(from, to)` pair
/// with a function that builds a boxed future running the step. Using the
/// `Pin<Box<dyn Future>>` shape lets us hold a slice of `fn` pointers (no
/// closures, no trait objects) — adding a step in v26 is now one row
/// append rather than editing a hand-coded `match` ladder.
// `+ Send` is intentionally absent — every per-version migration enters a
// `tracing::info_span!(...).entered()` whose `EnteredSpan` guard is `!Send`,
// so the resulting future isn't `Send` either. `run_migration` is awaited
// from the same task that holds the SQLite connection, so a non-`Send`
// future is fine. If a future migration moves the connection across
// `tokio::spawn`, that step would need its own restructure regardless.
type MigrationFn = for<'c> fn(
    &'c mut sqlx::SqliteConnection,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), StoreError>> + 'c>,
>;

const MIGRATIONS: &[(i32, i32, MigrationFn)] = &[
    (10, 11, |c| Box::pin(migrate_v10_to_v11(c))),
    (11, 12, |c| Box::pin(migrate_v11_to_v12(c))),
    (12, 13, |c| Box::pin(migrate_v12_to_v13(c))),
    (13, 14, |c| Box::pin(migrate_v13_to_v14(c))),
    (14, 15, |c| Box::pin(migrate_v14_to_v15(c))),
    (15, 16, |c| Box::pin(migrate_v15_to_v16(c))),
    (16, 17, |c| Box::pin(migrate_v16_to_v17(c))),
    (17, 18, |c| Box::pin(migrate_v17_to_v18(c))),
    (18, 19, |c| Box::pin(migrate_v18_to_v19(c))),
    (19, 20, |c| Box::pin(migrate_v19_to_v20(c))),
    (20, 21, |c| Box::pin(migrate_v20_to_v21(c))),
    (21, 22, |c| Box::pin(migrate_v21_to_v22(c))),
    (22, 23, |c| Box::pin(migrate_v22_to_v23(c))),
    (23, 24, |c| Box::pin(migrate_v23_to_v24(c))),
    (24, 25, |c| Box::pin(migrate_v24_to_v25(c))),
];

/// Run a single migration step
async fn run_migration(
    conn: &mut sqlx::SqliteConnection,
    from: i32,
    to: i32,
) -> Result<(), StoreError> {
    match MIGRATIONS.iter().find(|(f, t, _)| *f == from && *t == to) {
        Some((_, _, run)) => run(conn).await,
        None => Err(StoreError::MigrationNotSupported { from, to }),
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
/// Dropped the sentiment dimension — embeddings are now pure model-native output.
/// - Updates dimensions metadata from 769 to model dim (was 768 for E5-base-v2)
/// - Sets hnsw_dirty to trigger HNSW rebuild (old index has 769-dim vectors)
/// - Notes embedding column is left as-is (we write empty blobs now, old data is harmless)
async fn migrate_v14_to_v15(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    // DS-4: Only update dimensions from 769→768 (the old sentiment-augmented size).
    // Databases already using a different model dim (e.g. 1024 for BGE-large) must
    // not be overwritten to 768.
    sqlx::query("UPDATE metadata SET value = '768' WHERE key = 'dimensions' AND value = '769'")
        .execute(&mut *conn)
        .await?;

    sqlx::query("UPDATE metadata SET value = '1' WHERE key = 'hnsw_dirty'")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Updated dimensions and marked HNSW dirty. \
         Run 'cqs index --force' to rebuild embeddings."
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

    sqlx::query("DROP TABLE IF EXISTS llm_summaries")
        .execute(&mut *conn)
        .await?;

    sqlx::query("ALTER TABLE llm_summaries_v2 RENAME TO llm_summaries")
        .execute(&mut *conn)
        .await?;

    tracing::info!("Recreated llm_summaries with composite PK (content_hash, purpose).");
    Ok(())
}

/// Migrate from v16 to v17: sparse_vectors table + enrichment_version column
///
/// - `sparse_vectors`: stores SPLADE sparse vectors for hybrid search.
///   Each chunk gets a set of (token_id, weight) pairs from the learned sparse encoder.
/// - `enrichment_version`: RT-DATA-2 idempotency marker. Tracks which enrichment pass
///   last processed each chunk, preventing double-application of call graph context.
async fn migrate_v16_to_v17(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v16_to_v17").entered();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sparse_vectors (
            chunk_id TEXT NOT NULL,
            token_id INTEGER NOT NULL,
            weight REAL NOT NULL,
            PRIMARY KEY (chunk_id, token_id)
        )",
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_sparse_token ON sparse_vectors(token_id)")
        .execute(&mut *conn)
        .await?;

    // RT-DATA-2: enrichment idempotency marker
    sqlx::query("ALTER TABLE chunks ADD COLUMN enrichment_version INTEGER NOT NULL DEFAULT 0")
        .execute(&mut *conn)
        .await?;

    tracing::info!("Migrated to v17: sparse_vectors table + enrichment_version column");
    Ok(())
}

/// Migrate from v17 to v18: add embedding_base column to chunks
///
/// Phase 5 of adaptive retrieval: dual embeddings. Each chunk gets a second
/// embedding built from the raw NL description (without LLM summary or call-graph
/// enrichment). Conceptual/behavioral/negation queries route to the base index,
/// structural/multi-step queries keep the enriched index.
///
/// NULL is a valid state post-migration — chunks haven't been re-embedded yet.
/// The base HNSW index is only built once the column is populated; until then
/// the router silently falls back to the enriched index.
async fn migrate_v17_to_v18(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v17_to_v18").entered();

    sqlx::query("ALTER TABLE chunks ADD COLUMN embedding_base BLOB")
        .execute(&mut *conn)
        .await?;

    tracing::info!("Migrated to v18: embedding_base column (NULL until next index pass)");
    Ok(())
}

/// Migrate from v18 to v19: add FK(chunk_id) ON DELETE CASCADE to sparse_vectors
///
/// v1.22.0 audit finding DS-W3: the v17 `sparse_vectors` table was declared
/// without a foreign key to `chunks`, so three code paths in
/// `src/store/chunks/crud.rs` (`delete_by_origin`, `delete_phantom_chunks`,
/// `upsert_chunks_and_calls`) leaked orphan sparse rows — every chunks-delete
/// produced sparse_vectors rows that no query could reach, and `prune_missing`
/// / `prune_all` had to clean them up manually. Worse, the cleanup paths
/// forgot to bump `splade_generation` (DS-W1), so the persisted
/// `splade.index.bin` kept serving stale chunk_ids after a GC.
///
/// This migration makes the invariant structural: any delete from `chunks`
/// now cascades to `sparse_vectors` automatically, the same way
/// `calls.source_chunk_id` and `type_edges.source_chunk_id` already cascade
/// since v10/v11. Memory rule: invalidation counters attached to mutable
/// state must be enforced at the schema layer, not instrumented at specific
/// call sites.
///
/// SQLite does not support `ALTER TABLE ADD FOREIGN KEY`, so we rebuild the
/// table in place. Orphan rows (sparse_vectors whose chunk_id no longer
/// exists in `chunks`) are dropped during the copy — they were leaked data
/// by definition. Row count before and after is logged for transparency.
///
/// After the table swap, the SPLADE generation counter is bumped
/// unconditionally so any persisted `splade.index.bin` from a pre-v19 schema
/// is invalidated on the next load (the on-disk file's embedded generation
/// won't match, forcing a clean rebuild from the new FK-protected table).
async fn migrate_v18_to_v19(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v18_to_v19").entered();

    // Count rows before the rebuild so the migration log makes any silent
    // orphan purge visible instead of invisible.
    let (before_rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sparse_vectors")
        .fetch_one(&mut *conn)
        .await?;

    // Create the new table with the FK constraint. Same PRIMARY KEY shape as
    // v17 so the idx_sparse_token rebuild below is a drop-in replacement.
    sqlx::query(
        "CREATE TABLE sparse_vectors_v19 (
            chunk_id TEXT NOT NULL,
            token_id INTEGER NOT NULL,
            weight REAL NOT NULL,
            PRIMARY KEY (chunk_id, token_id),
            FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
        )",
    )
    .execute(&mut *conn)
    .await?;

    // Copy only rows whose chunk_id exists in chunks — the INNER JOIN is the
    // orphan filter. Any row that doesn't match was already unreachable; the
    // migration is the right place to drop them.
    sqlx::query(
        "INSERT INTO sparse_vectors_v19 (chunk_id, token_id, weight)
         SELECT s.chunk_id, s.token_id, s.weight
         FROM sparse_vectors s
         INNER JOIN chunks c ON c.id = s.chunk_id",
    )
    .execute(&mut *conn)
    .await?;

    let (after_rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sparse_vectors_v19")
        .fetch_one(&mut *conn)
        .await?;
    let dropped = before_rows - after_rows;
    // DS2-8: escalate to `error!` when the INNER JOIN filter drops more than
    // 10% of rows. A small orphan set is expected (pre-v19 delete paths
    // leaked rows); a large drop suggests the chunks table was truncated
    // out-of-band, or an unrelated bug produced widespread chunk/sparse
    // inconsistency. Surface it at `error` so it's visible in logs even
    // when the migration itself succeeds. Not a hard fail — the rebuild is
    // still strictly an improvement on the old unconstrained shape — but
    // the user should know.
    let threshold = (before_rows as f64 * 0.10) as i64;
    if before_rows > 0 && dropped > threshold {
        tracing::error!(
            before = before_rows,
            after = after_rows,
            dropped_orphans = dropped,
            threshold,
            "v18→v19 migration dropped more than 10% of sparse_vectors rows \
             as orphans — this may indicate prior corruption or an out-of-band \
             chunks truncation. Review the log and consider rebuilding from \
             source via 'cqs index --force' if the drop looks wrong."
        );
    } else if dropped > 0 {
        tracing::warn!(
            before = before_rows,
            after = after_rows,
            dropped_orphans = dropped,
            "v18→v19 migration dropped orphan sparse_vectors rows (chunks no longer exist). \
             These were leaks from pre-v19 delete paths."
        );
    } else {
        tracing::info!(
            rows = before_rows,
            "v18→v19 sparse_vectors row count unchanged after FK filter"
        );
    }

    // Drop the old idx_sparse_token first (it's tied to the old table) so the
    // swap doesn't trip the UNIQUE-index-name constraint.
    sqlx::query("DROP INDEX IF EXISTS idx_sparse_token")
        .execute(&mut *conn)
        .await?;
    sqlx::query("DROP TABLE sparse_vectors")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE sparse_vectors_v19 RENAME TO sparse_vectors")
        .execute(&mut *conn)
        .await?;
    sqlx::query("CREATE INDEX idx_sparse_token ON sparse_vectors(token_id)")
        .execute(&mut *conn)
        .await?;

    // Bump splade_generation so any on-disk splade.index.bin from pre-v19
    // is invalidated — its header generation won't match the new value.
    // The UPSERT seeds the row if it wasn't present (older DBs predating
    // the PR #895 counter never had this metadata key).
    sqlx::query(
        "INSERT INTO metadata (key, value) VALUES ('splade_generation', '1')
         ON CONFLICT(key) DO UPDATE SET
             value = CAST((CAST(value AS INTEGER) + 1) AS TEXT)",
    )
    .execute(&mut *conn)
    .await?;

    tracing::info!(
        "Migrated to v19: sparse_vectors has FK(chunk_id) → chunks(id) ON DELETE CASCADE"
    );
    Ok(())
}

/// Migrate from v19 to v20: AFTER DELETE trigger on chunks bumps splade_generation
///
/// v1.22.0 audit DS-W2 / OB-22 / PB-NEW-6 (triple-confirmed by three
/// independent auditors): `cqs watch` never touched SPLADE. When watch
/// detected a file edit and called `delete_phantom_chunks` or
/// `delete_by_origin`, the v19 FK CASCADE correctly removed the orphan
/// sparse rows, but nothing bumped `splade_generation`. The on-disk
/// `splade.index.bin` still matched the unchanged counter, so readers
/// trusted the stale file and served chunk_ids that no longer existed.
///
/// This trigger fires on every `DELETE FROM chunks` statement, once per
/// deleted row, and bumps the generation via a single metadata UPDATE.
/// For a watch cycle that touches 1-200 chunks, that's 1-200 metadata
/// updates — negligible. For `cqs index --force`, the new DB is fresh
/// and receives no DELETE statements at all, so the trigger cost is
/// zero on the bulk-reindex path. The only concern is `delete_by_origin`
/// during normal reindex when many chunks are displaced; even then the
/// write amplification is ~1-5s per 10k deletions, vs. the minutes the
/// actual rebuild takes.
///
/// The trigger is scoped to `chunks` deletions specifically. sparse_vectors
/// writes are still bumped explicitly by `bump_splade_generation_tx` (one
/// call per upsert transaction, not per row) because that's the only site
/// the trigger-on-sparse_vectors alternative would have caught and it
/// would have fired millions of times during a bulk upsert — row-level
/// triggers on the high-cardinality table were the wrong design.
async fn migrate_v19_to_v20(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v19_to_v20").entered();

    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS bump_splade_on_chunks_delete \
         AFTER DELETE ON chunks \
         BEGIN \
             INSERT INTO metadata (key, value) VALUES ('splade_generation', '1') \
             ON CONFLICT(key) DO UPDATE SET \
                 value = CAST((CAST(value AS INTEGER) + 1) AS TEXT); \
         END",
    )
    .execute(&mut *conn)
    .await?;

    // Bump generation immediately so any pre-v20 persisted splade.index.bin
    // (possibly already out of sync with sparse_vectors because watch-mode
    // deletes since v19 landed never bumped the counter) gets invalidated
    // on the next load.
    sqlx::query(
        "INSERT INTO metadata (key, value) VALUES ('splade_generation', '1')
         ON CONFLICT(key) DO UPDATE SET
             value = CAST((CAST(value AS INTEGER) + 1) AS TEXT)",
    )
    .execute(&mut *conn)
    .await?;

    tracing::info!(
        "Migrated to v20: AFTER DELETE trigger on chunks bumps splade_generation (DS-W2/OB-22 fix)"
    );
    Ok(())
}

/// Migrate from v20 to v21: add `parser_version` column to chunks
///
/// v1.28.0 audit P2 #29 (recovery wave): the watch path UPSERTs chunks with an
/// `ON CONFLICT(id) DO UPDATE ... WHERE chunks.content_hash != excluded.content_hash`
/// short-circuit, which is correct when the only thing that ever changes is the
/// source bytes. But `extract_doc_fallback_for_short_chunk` (PR #1040) can
/// change `doc` for a chunk whose source bytes are byte-identical to a
/// previously-indexed version — and that change was being silently discarded
/// on every incremental update. A `parser_version` stamp lets the UPSERT
/// invalidate rows whose parser logic moved on, mirroring `splade_generation`.
///
/// Defaults to 0 for existing rows so the next `cqs index` (or watch reindex)
/// will write the live PARSER_VERSION value and refresh the affected fields.
async fn migrate_v20_to_v21(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v20_to_v21").entered();

    sqlx::query("ALTER TABLE chunks ADD COLUMN parser_version INTEGER NOT NULL DEFAULT 0")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Migrated to v21: parser_version column on chunks (P2 #29 — content-hash-stable doc refresh)"
    );
    Ok(())
}

/// Migrate from v21 to v22: add umap_x and umap_y REAL columns to chunks.
///
/// Both columns are nullable (REAL is nullable by default in SQLite). They
/// stay NULL until `cqs index --umap` runs the umap-learn projection
/// (`scripts/run_umap.py`) over the persisted chunk embeddings and writes
/// the 2D coordinates back. The /api/embed/2d endpoint in `cqs serve`
/// filters to `umap_x IS NOT NULL`, so the cluster view is dark until the
/// projection has been computed at least once but the rest of cqs is
/// unaffected.
async fn migrate_v21_to_v22(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v21_to_v22").entered();

    sqlx::query("ALTER TABLE chunks ADD COLUMN umap_x REAL")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE chunks ADD COLUMN umap_y REAL")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Migrated to v22: umap_x/umap_y columns on chunks (cqs serve cluster view, opt-in via `cqs index --umap`)"
    );
    Ok(())
}

/// v22 → v23: Add `source_size` (INTEGER) + `source_content_hash` (BLOB)
/// columns on `chunks` to power the reconcile fingerprint (issue #1219 /
/// EX-V1.30.1-6). Both nullable: pre-v23 rows stay valid; first re-embed
/// populates them. Reconcile uses these as tiebreakers when mtimes are
/// identical (FAT32/NTFS/HFS+/SMB ≥1s mtime resolution) or when mtime
/// changed but content didn't (`git checkout`, formatter passes).
async fn migrate_v22_to_v23(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v22_to_v23").entered();

    sqlx::query("ALTER TABLE chunks ADD COLUMN source_size INTEGER")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE chunks ADD COLUMN source_content_hash BLOB")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Migrated to v23: source_size + source_content_hash columns on chunks (reconcile fingerprint, #1219)"
    );
    Ok(())
}

/// v23 → v24: Add `vendored` (INTEGER NOT NULL DEFAULT 0) column on
/// `chunks` to power the third-party-content trust-level downgrade
/// (issue #1221 / SEC-V1.30.1-5). Pre-migration rows default to
/// `vendored = 0` (treated as user-code); the next reindex flags chunks
/// whose `origin` matches a configured vendored-path prefix
/// (`vendor/`, `node_modules/`, etc.). Search/scout/onboard JSON output
/// then emits `trust_level: "vendored-code"` for those chunks instead
/// of the bare `user-code` claim — the structural fix the SEC-V1.30.1-5
/// doc-only stop-gap acknowledged was needed.
async fn migrate_v23_to_v24(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v23_to_v24").entered();

    sqlx::query("ALTER TABLE chunks ADD COLUMN vendored INTEGER NOT NULL DEFAULT 0")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Migrated to v24: vendored column on chunks (trust-level downgrade for vendored content, #1221). \
         Existing rows default to vendored=0; reindex to flag chunks under configured vendored-path prefixes."
    );
    Ok(())
}

/// v24 → v25: Add `kind` (TEXT, nullable) column on `notes` to enable
/// the kind/tag taxonomy that #1133 (audit P2.91) tracked. Pre-v25
/// rows stay valid with `kind = NULL`; the parser's
/// `sentiment_to_prefix` mapping continues to drive embedding-text
/// prefixes when `kind = None`. New notes added via
/// `cqs notes add --kind <kind>` populate the column at insert time
/// and take precedence over the sentiment-based prefix.
///
/// An index on `notes(kind)` powers the future `cqs notes list --kind`
/// filter — created at the same migration step rather than separately
/// to keep v25 a single self-contained schema bump.
async fn migrate_v24_to_v25(conn: &mut sqlx::SqliteConnection) -> Result<(), StoreError> {
    let _span = tracing::info_span!("migrate_v24_to_v25").entered();

    sqlx::query("ALTER TABLE notes ADD COLUMN kind TEXT")
        .execute(&mut *conn)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_notes_kind ON notes(kind)")
        .execute(&mut *conn)
        .await?;

    tracing::info!(
        "Migrated to v25: kind column on notes (structured tag taxonomy, #1133). \
         Existing rows default to kind=NULL; new notes via `cqs notes add --kind <kind>` populate it."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    std::thread_local! {
        /// Test-only failure-injection flag read by `run_migration_tx`.
        /// Tests that want to exercise the `migrate()` backup/restore path
        /// set this to `target_version`; after the migration step that
        /// bumps the version to that value completes, `run_migration_tx`
        /// returns an error without committing. `0` = disabled.
        ///
        /// Thread-local rather than global so parallel tests don't inject
        /// failures into each other — each `#[test]` runs on its own
        /// thread, and the hook only fires on threads that explicitly set
        /// this cell.
        pub(super) static TEST_FAIL_AFTER_VERSION: std::cell::Cell<i32> =
            const { std::cell::Cell::new(0) };
    }

    #[test]
    fn test_migration_not_supported_error() {
        // Verify unknown migrations produce clear errors
        let err = StoreError::MigrationNotSupported { from: 5, to: 6 };
        let msg = err.to_string();
        assert!(msg.contains("5"));
        assert!(msg.contains("6"));
    }

    #[test]
    fn test_current_schema_version_documented() {
        // Ensure the current version matches what we document
        assert_eq!(CURRENT_SCHEMA_VERSION, 25);
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

            let result = migrate(pool, &db_path, 15, 15).await;
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

            let result = migrate(pool, &db_path, 15, 14).await;
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

            // Run migration from v10 to v11. P2.59: migrate consumes the pool
            // by value so the failure path can close it before the file
            // replace; we rebind to the returned pool to keep using it.
            let pool = migrate(pool, &db_path, 10, 11).await.unwrap();

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
            let pool = migrate(pool, &db_path, 12, 13).await.unwrap();

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
            let pool = migrate(pool, &db_path, 13, 14).await.unwrap();

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
            let pool = migrate(pool, &db_path, 14, 15).await.unwrap();

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
            let pool = migrate(pool, &db_path, 15, 16).await.unwrap();

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
            let pool = migrate(pool, &db_path, 12, 14).await.unwrap();

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

            let result = migrate(pool, &db_path, 8, 11).await;
            assert!(result.is_err(), "unsupported range should fail");
            match result.unwrap_err() {
                StoreError::MigrationNotSupported { from, to } => {
                    assert_eq!(from, 8);
                    assert_eq!(to, 9);
                }
                other => panic!("Expected MigrationNotSupported, got: {:?}", other),
            }
        });
    }

    /// Phase 5 regression: v17→v18 adds embedding_base column without touching
    /// existing rows, and the migration is idempotent-ish in the sense that a
    /// follow-up attempt errors on the duplicate ALTER (caller must not re-run).
    #[test]
    fn test_migrate_v17_to_v18_adds_embedding_base_column() {
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

            // Minimal v17 schema (chunks + metadata); only the columns
            // touched by the v17→v18 migration matter here.
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '17')")
                .execute(&pool)
                .await
                .unwrap();

            // Insert a row with a non-trivial embedding so we can verify it
            // survives the migration untouched.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at) \
                 VALUES ('chunk-1', 'file:lib.rs', 'file', 'rust', 'function', 'foo', \
                 'fn foo()', 'fn foo() {}', 'hash1', 10, 20, X'deadbeef', \
                 '2026-04-10', '2026-04-10')",
            )
            .execute(&pool)
            .await
            .unwrap();

            let pool = migrate(pool, &db_path, 17, 18).await.unwrap();

            // Column exists and defaults to NULL for pre-existing rows.
            let (embedding_existing, embedding_base): (Vec<u8>, Option<Vec<u8>>) =
                sqlx::query_as("SELECT embedding, embedding_base FROM chunks WHERE id = 'chunk-1'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                embedding_existing,
                vec![0xde, 0xad, 0xbe, 0xef],
                "existing embedding must survive migration untouched"
            );
            assert!(
                embedding_base.is_none(),
                "embedding_base must be NULL for pre-existing rows (base pass hasn't run yet)"
            );

            // Schema version bumped.
            let version: (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(version.0, "18");

            // NULL is a writeable state — caller can populate it later.
            sqlx::query("UPDATE chunks SET embedding_base = X'cafef00d' WHERE id = 'chunk-1'")
                .execute(&pool)
                .await
                .unwrap();
            let (base_after,): (Option<Vec<u8>>,) =
                sqlx::query_as("SELECT embedding_base FROM chunks WHERE id = 'chunk-1'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(base_after, Some(vec![0xca, 0xfe, 0xf0, 0x0d]));
        });
    }

    /// Phase 5: full migrate() chain is idempotent at the dispatcher level.
    /// Calling `migrate(pool, 18, 18)` after a successful upgrade must be a
    /// no-op — the schema_version metadata gates re-execution of the ALTER.
    /// (The raw migration function itself is NOT idempotent — `ALTER TABLE
    /// ADD COLUMN` errors on duplicate column. This test exercises the
    /// dispatcher contract that protects users from the underlying limitation.)
    #[test]
    fn test_migrate_v17_to_v18_dispatcher_is_idempotent() {
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

            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '17')")
                .execute(&pool)
                .await
                .unwrap();

            // First call: 17→18 succeeds.
            let pool = migrate(pool, &db_path, 17, 18).await.unwrap();

            // Second call at the same target version: should be a no-op.
            // This is the property users actually depend on — re-running
            // `cqs index` should not fail just because the schema is current.
            let _pool = migrate(pool, &db_path, 18, 18).await.unwrap();
        });
    }

    /// v1.22.0 audit DS-W3: v18→v19 migration adds FK(chunk_id) ON DELETE
    /// CASCADE on sparse_vectors. Test covers:
    /// - Orphan sparse rows (chunks no longer exist) are dropped during
    ///   the rebuild
    /// - Non-orphan sparse rows survive the rebuild
    /// - The idx_sparse_token index is recreated
    /// - splade_generation is bumped so any pre-v19 persisted index file
    ///   is invalidated
    /// - FK CASCADE works after the migration: deleting a chunk auto-deletes
    ///   its sparse rows
    #[test]
    fn test_migrate_v18_to_v19_adds_fk_cascade_and_purges_orphans() {
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
                        .create_if_missing(true)
                        // PRAGMA foreign_keys must be ON for ON DELETE CASCADE to fire.
                        .foreign_keys(true),
                )
                .await
                .unwrap();

            // Minimal v18 schema — chunks with embedding_base, sparse_vectors
            // WITHOUT the FK constraint (as shipped in v17).
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    embedding_base BLOB,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE TABLE sparse_vectors (
                    chunk_id TEXT NOT NULL,
                    token_id INTEGER NOT NULL,
                    weight REAL NOT NULL,
                    PRIMARY KEY (chunk_id, token_id)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("CREATE INDEX idx_sparse_token ON sparse_vectors(token_id)")
                .execute(&pool)
                .await
                .unwrap();

            sqlx::query(
                "INSERT INTO metadata (key, value) VALUES ('schema_version', '18'),
                                                          ('splade_generation', '5')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Insert two chunks and sparse rows for both of them.
            for id in ["chunk-live", "chunk-also-live"] {
                sqlx::query(
                    "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                     signature, content, content_hash, line_start, line_end, embedding, \
                     created_at, updated_at) \
                     VALUES (?1, 'file:lib.rs', 'file', 'rust', 'function', ?1, \
                     '', '', 'hash', 1, 10, X'00', '2026-04-11', '2026-04-11')",
                )
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            }
            sqlx::query(
                "INSERT INTO sparse_vectors (chunk_id, token_id, weight) VALUES
                    ('chunk-live', 1, 0.5),
                    ('chunk-live', 2, 0.3),
                    ('chunk-also-live', 3, 0.8),
                    ('chunk-orphan', 4, 0.9),
                    ('chunk-orphan', 5, 0.1)",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Run the migration.
            let pool = migrate(pool, &db_path, 18, 19).await.unwrap();

            // Orphan rows dropped, live rows survive.
            let (count,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM sparse_vectors WHERE chunk_id = 'chunk-orphan'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 0, "orphan rows should have been dropped");
            let (count_live,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM sparse_vectors WHERE chunk_id = 'chunk-live'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(count_live, 2, "chunk-live rows should survive");

            // idx_sparse_token exists after the rebuild.
            let idx: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='index' AND name='idx_sparse_token'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(idx.is_some(), "idx_sparse_token must be recreated");

            // splade_generation bumped.
            let (gen_val,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let gen_parsed: u64 = gen_val.parse().unwrap();
            assert!(gen_parsed > 5, "splade_generation must be bumped past 5");

            // schema_version updated.
            let (v,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(v, "19");

            // FK CASCADE contract: deleting a chunk removes its sparse rows.
            sqlx::query("DELETE FROM chunks WHERE id = 'chunk-also-live'")
                .execute(&pool)
                .await
                .unwrap();
            let (remaining,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM sparse_vectors WHERE chunk_id = 'chunk-also-live'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                remaining, 0,
                "CASCADE should have removed chunk-also-live's sparse rows"
            );
        });
    }

    /// v1.22.0 audit DS-W2 / OB-22 / PB-NEW-6: v19→v20 adds a trigger on
    /// chunks that bumps splade_generation on every DELETE. Test covers:
    /// - Migration bumps the generation immediately
    /// - The trigger fires on subsequent DELETE FROM chunks
    /// - Multiple deletes produce multiple bumps (cardinality check)
    /// - A no-op reindex (all INSERT no DELETE) does NOT bump via the trigger
    #[test]
    fn test_migrate_v19_to_v20_adds_trigger_that_bumps_on_chunks_delete() {
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
                        .create_if_missing(true)
                        .foreign_keys(true),
                )
                .await
                .unwrap();

            // Minimal v19 schema: chunks, sparse_vectors with v19 FK, metadata.
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    embedding_base BLOB,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE TABLE sparse_vectors (
                    chunk_id TEXT NOT NULL,
                    token_id INTEGER NOT NULL,
                    weight REAL NOT NULL,
                    PRIMARY KEY (chunk_id, token_id),
                    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("CREATE INDEX idx_sparse_token ON sparse_vectors(token_id)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO metadata (key, value) VALUES ('schema_version', '19'),
                                                          ('splade_generation', '10')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Seed a chunk so we have something to delete.
            for id in ["c1", "c2", "c3"] {
                sqlx::query(
                    "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                     signature, content, content_hash, line_start, line_end, embedding, \
                     created_at, updated_at) \
                     VALUES (?1, 'file:lib.rs', 'file', 'rust', 'function', ?1, \
                     '', '', 'h', 1, 10, X'00', '2026-04-12', '2026-04-12')",
                )
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            }

            // Run v19 → v20 migration.
            let pool = migrate(pool, &db_path, 19, 20).await.unwrap();

            // Migration itself bumps the generation once.
            let (gen_after_migration,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let gen_after_migration: u64 = gen_after_migration.parse().unwrap();
            assert!(
                gen_after_migration > 10,
                "migration should bump generation past starting value 10"
            );

            // Trigger exists.
            let trigger: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type='trigger' \
                 AND name='bump_splade_on_chunks_delete'",
            )
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(trigger.is_some(), "v20 trigger must exist after migration");

            // Schema version.
            let (v,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(v, "20");

            // Deleting a chunk fires the trigger once and bumps the
            // generation.
            let before_one_delete = gen_after_migration;
            sqlx::query("DELETE FROM chunks WHERE id = 'c1'")
                .execute(&pool)
                .await
                .unwrap();
            let (gen_after_one_delete,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let gen_after_one_delete: u64 = gen_after_one_delete.parse().unwrap();
            assert_eq!(
                gen_after_one_delete,
                before_one_delete + 1,
                "one chunk delete should bump generation by exactly one"
            );

            // Deleting two chunks bumps by two (trigger is row-level).
            sqlx::query("DELETE FROM chunks WHERE id IN ('c2', 'c3')")
                .execute(&pool)
                .await
                .unwrap();
            let (gen_after_two_delete,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let gen_after_two_delete: u64 = gen_after_two_delete.parse().unwrap();
            assert_eq!(
                gen_after_two_delete,
                gen_after_one_delete + 2,
                "two chunk deletes should bump generation by exactly two"
            );

            // INSERTs do NOT bump via this trigger (new chunks don't
            // invalidate existing sparse data).
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at) \
                 VALUES ('c4', 'file:lib.rs', 'file', 'rust', 'function', 'c4', \
                 '', '', 'h', 1, 10, X'00', '2026-04-12', '2026-04-12')",
            )
            .execute(&pool)
            .await
            .unwrap();
            let (gen_after_insert,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'splade_generation'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let gen_after_insert: u64 = gen_after_insert.parse().unwrap();
            assert_eq!(
                gen_after_insert, gen_after_two_delete,
                "INSERT should NOT bump the generation (no DELETE happened)"
            );
        });
    }

    // ========================================================================
    // Issue #953: filesystem backup before migrate() runs DDL.
    //
    // The four tests below cover the happy-path backup creation, the
    // failure-path restore, the pruning policy, and the absent-WAL edge case.
    // ========================================================================

    /// Helper: build a minimal v19 schema on `pool` so migrate(19 -> 20) can
    /// run against it. Each #953 test uses this so the test body focuses on
    /// backup/restore behaviour rather than schema setup.
    async fn seed_v19_schema(pool: &sqlx::SqlitePool) {
        sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE chunks (
                id TEXT PRIMARY KEY,
                origin TEXT NOT NULL,
                source_type TEXT NOT NULL,
                language TEXT NOT NULL,
                chunk_type TEXT NOT NULL,
                name TEXT NOT NULL,
                signature TEXT NOT NULL,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                embedding BLOB NOT NULL,
                embedding_base BLOB,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                enrichment_version INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE sparse_vectors (
                chunk_id TEXT NOT NULL,
                token_id INTEGER NOT NULL,
                weight REAL NOT NULL,
                PRIMARY KEY (chunk_id, token_id),
                FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
            )",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("CREATE INDEX idx_sparse_token ON sparse_vectors(token_id)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO metadata (key, value) VALUES ('schema_version', '19'),
                                                      ('splade_generation', '7')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// Issue #953, happy path: a successful migrate() leaves a
    /// `.bak-v{from}-v{to}-{ts}.db` file in the DB's parent directory.
    ///
    /// This is what gives users a recovery path when a future migration
    /// fails on real data — they can `mv` the backup back into place
    /// instead of re-indexing their corpus from source.
    #[test]
    fn test_migrate_writes_backup_file_on_success() {
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
                        .create_if_missing(true)
                        .foreign_keys(true),
                )
                .await
                .unwrap();
            seed_v19_schema(&pool).await;

            // Seed one chunk so the DB is not empty — lets us catch a bug
            // where the backup captures a zero-length file instead of the
            // live one.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at) \
                 VALUES ('c1', 'file:lib.rs', 'file', 'rust', 'function', 'c1', \
                 '', '', 'h', 1, 10, X'00', '2026-04-15', '2026-04-15')",
            )
            .execute(&pool)
            .await
            .unwrap();

            let _pool = migrate(pool, &db_path, 19, 20).await.unwrap();

            // Enumerate backups — exactly one should exist after this run.
            let backups: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .filter(|n| n.starts_with("test.bak-v19-v20-") && n.ends_with(".db"))
                .collect();
            assert_eq!(
                backups.len(),
                1,
                "expected exactly one .bak-v19-v20-*.db file, got: {:?}",
                backups
            );

            // Backup should be non-empty (not a placeholder for a failed copy).
            let bak_path = dir.path().join(&backups[0]);
            let bak_bytes = std::fs::read(&bak_path).unwrap();
            assert!(
                !bak_bytes.is_empty(),
                "backup DB file should not be zero-length"
            );
            // SQLite files start with the 16-byte header "SQLite format 3\0".
            assert!(
                bak_bytes.starts_with(b"SQLite format 3\0"),
                "backup should be a valid SQLite database file"
            );
        });
    }

    /// Issue #953, failure path: when a migration step returns `Err` mid-way
    /// (simulating either a bug inside a migration function or a
    /// commit-time I/O failure), the restore-from-backup path runs and the
    /// DB file is byte-identical to its pre-migrate state.
    ///
    /// Uses the thread-local `TEST_FAIL_AFTER_VERSION` hook (gated on
    /// `cfg(test)` inside `run_migration_tx`) to trigger the failure
    /// deterministically. Thread-local so parallel v17→v18 tests on other
    /// threads don't see the injected failure.
    #[test]
    fn test_migrate_restores_db_on_failure() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Phase 1: set up a v17 DB, close the pool so SQLite checkpoints
        // and removes the WAL, then snapshot the main DB file bytes. These
        // bytes are what restore must reproduce.
        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
                )
                .await
                .unwrap();
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '17')")
                .execute(&pool)
                .await
                .unwrap();
            // Checkpoint and close so the WAL is drained into the main DB.
            // After this, the on-disk bytes capture the full pre-migrate state.
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        });

        let pre_migrate_bytes = std::fs::read(&db_path).unwrap();

        // Phase 2: reopen, set the failure hook, run migrate(17 -> 19).
        // The hook makes step v18 succeed then fail the overall migration.
        // On error, restore should put the DB file back to pre_migrate_bytes.
        TEST_FAIL_AFTER_VERSION.with(|c| c.set(18));

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(false)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
                )
                .await
                .unwrap();

            // P2.59: migrate consumes the pool by value. On the failure
            // path it closes the pool internally before restore runs, so
            // we don't need a separate close here.
            let err = migrate(pool, &db_path, 17, 19).await.unwrap_err();
            match err {
                StoreError::Runtime(msg) => assert!(
                    msg.contains("injected failure"),
                    "expected injected-failure error, got: {}",
                    msg
                ),
                other => panic!("expected Runtime(injected failure), got: {:?}", other),
            }
        });

        TEST_FAIL_AFTER_VERSION.with(|c| c.set(0));

        // The DB file must be byte-identical to the pre-migrate snapshot.
        // If restore didn't fire, the ALTER from migrate_v17_to_v18 would
        // have changed the schema pages and the bytes would differ.
        let post_migrate_bytes = std::fs::read(&db_path).unwrap();
        assert_eq!(
            post_migrate_bytes, pre_migrate_bytes,
            "DB file bytes must match pre-migrate state after failed migration + restore"
        );
    }

    /// P2.59 / issue #1125: when a migration fails, the live pool must be
    /// closed BEFORE `restore_from_backup`'s `atomic_replace` runs over the
    /// DB file. Otherwise pool descriptors keep mmap'ing the unlinked old
    /// inode while subsequent opens see the restored backup — silent
    /// two-state divergence where in-process queries can read stale rows
    /// from the orphaned old inode while readers from new processes see the
    /// restored DB.
    ///
    /// This test simulates the daemon scenario from the issue: a long-lived
    /// pool is open, a migration runs and fails, then we verify that:
    /// 1. A fresh pool opened against the same path AFTER migrate returns
    ///    sees the restored pre-migrate state — proves the file replace
    ///    landed correctly.
    /// 2. The pool that was passed into migrate has been consumed (compile-
    ///    time enforcement via the value-taking signature). The error path
    ///    inside migrate closes the pool before atomic_replace, so the
    ///    descriptors against the orphaned inode are released.
    /// 3. The sentinel row written pre-migrate is readable through the
    ///    fresh pool. If migrate had skipped the close-and-restore protocol,
    ///    the file replace could have left the sentinel readable only
    ///    through the orphaned pool — which by now is gone.
    #[test]
    fn test_migrate_failure_closes_pool_before_restore_no_phantom_inode() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Phase 1: build a v17 DB with a sentinel row, checkpoint+close so
        // the on-disk bytes reflect the full state. The sentinel is what
        // we read back via the fresh pool after migration failure to
        // verify the restore landed on the inode the fresh open sees.
        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
                )
                .await
                .unwrap();
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '17')")
                .execute(&pool)
                .await
                .unwrap();
            // Sentinel row — the value is what we assert post-restore.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, \
                    name, signature, content, content_hash, line_start, line_end, \
                    embedding, created_at, updated_at) \
                 VALUES ('sentinel-1', 'file:lib.rs', 'file', 'rust', 'function', \
                    'sentinel_marker', 'fn sentinel()', 'fn sentinel() {}', \
                    'pre_migrate_hash', 1, 5, X'cafe', '2026-04-25', '2026-04-25')",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        });

        // Phase 2: open a FRESH pool and run migrate(17 -> 19). The hook
        // makes migration step v18 fire its DDL then fail — the failure
        // path must close this pool internally before atomic_replace
        // restores the backup. We don't keep a handle to the pool after
        // migrate consumes it; the value-taking signature enforces that.
        TEST_FAIL_AFTER_VERSION.with(|c| c.set(18));

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(false)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
                )
                .await
                .unwrap();

            let err = migrate(pool, &db_path, 17, 19).await.unwrap_err();
            // pool is consumed at this point — the borrow checker would
            // reject any further use. That alone enforces the close-before-
            // restore invariant: migrate cannot return Err with a still-open
            // pool because the signature returns SqlitePool only on Ok.
            assert!(
                matches!(&err, StoreError::Runtime(msg) if msg.contains("injected failure")),
                "expected injected-failure error, got: {:?}",
                err
            );
        });

        TEST_FAIL_AFTER_VERSION.with(|c| c.set(0));

        // Phase 3: open a FRESH pool against the same path. This simulates
        // a CLI invocation arriving after the daemon's failed migration —
        // it must see the restored DB on the inode the path resolves to,
        // not an orphaned post-DDL state.
        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(false)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
                )
                .await
                .unwrap();

            // Schema must be back at v17 — the v18 ALTER was rolled back
            // and the DB on disk is whatever the backup captured.
            let (version,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                version, "17",
                "fresh pool must see pre-migrate schema_version on the restored inode"
            );

            // Sentinel row from phase 1 must be readable. If atomic_replace
            // had landed on a different inode than the fresh pool resolves,
            // either the row would be missing or the WAL/SHM mismatch
            // would yield a "database disk image is malformed" error.
            let row: Option<(String, String)> =
                sqlx::query_as("SELECT name, content_hash FROM chunks WHERE id = 'sentinel-1'")
                    .fetch_optional(&pool)
                    .await
                    .expect(
                        "fresh-pool query against the restored DB must succeed — \
                 if it errors, atomic_replace landed on a phantom inode",
                    );
            assert_eq!(
                row,
                Some((
                    "sentinel_marker".to_string(),
                    "pre_migrate_hash".to_string()
                )),
                "sentinel row from pre-migrate state must be readable via fresh pool — \
                 if it's missing or mutated, the file replace happened against an \
                 orphaned inode while the live pool was still mmap'd"
            );

            // The v18 column must NOT exist — confirms the failed v17→v18
            // ALTER did not leak through the restore.
            let columns: Vec<(String,)> =
                sqlx::query_as("SELECT name FROM pragma_table_info('chunks')")
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            let has_embedding_base = columns.iter().any(|(n,)| n == "embedding_base");
            assert!(
                !has_embedding_base,
                "embedding_base (v18 column) must NOT exist after failed v17→v18 + restore — \
                 if present, the DDL leaked past the restore"
            );

            pool.close().await;
        });
    }

    /// Issue #953, prune policy: after a successful migrate, only the newest
    /// `KEEP_BACKUPS` (2) backups survive. Older ones are deleted.
    ///
    /// Setup seeds 5 fake `.bak-v*.db` files with staggered mtimes so there
    /// is a deterministic ordering, runs migrate which creates a 6th (the
    /// one produced by this run), and asserts that 3 survive: the two
    /// newest pre-existing backups and this run's backup.
    #[test]
    fn test_migrate_prunes_old_backups_keeping_newest_two() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Seed 5 pre-existing fake backups with distinct mtimes. Using
        // different filenames per from/to spans because the filename
        // already encodes a unique ts suffix in real usage; here we set
        // mtimes explicitly to control the sort order.
        let now = std::time::SystemTime::now();
        let five_secs = std::time::Duration::from_secs(5);
        let fake_backups = [
            ("test.bak-v10-v11-100.db", now - five_secs * 5), // oldest
            ("test.bak-v11-v12-200.db", now - five_secs * 4),
            ("test.bak-v12-v13-300.db", now - five_secs * 3),
            ("test.bak-v13-v14-400.db", now - five_secs * 2),
            ("test.bak-v14-v15-500.db", now - five_secs), // newest pre-existing
        ];
        for (name, mtime) in &fake_backups {
            let path = dir.path().join(name);
            // Valid SQLite header so any future "is this a real DB" check
            // wouldn't reject these synthetic ones during pruning.
            std::fs::write(&path, b"SQLite format 3\0filler").unwrap();
            // File::set_modified is stable as of Rust 1.75; cqs MSRV is 1.93.
            let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.set_modified(*mtime).unwrap();
        }

        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true)
                        .foreign_keys(true),
                )
                .await
                .unwrap();
            seed_v19_schema(&pool).await;

            let _pool = migrate(pool, &db_path, 19, 20).await.unwrap();
        });

        // After migrate, the prune keeps KEEP_BACKUPS (2) newest. This run
        // just produced a new backup (newer mtime than any fake), so the
        // survivors must be: the two newest fakes (v13-v14, v14-v15) +
        // this run's v19-v20. That is three files total.
        let mut survivors: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| n.starts_with("test.bak-v") && n.ends_with(".db"))
            .collect();
        survivors.sort();
        assert_eq!(
            survivors.len(),
            3,
            "expected 3 surviving backups (2 newest pre-existing + this run), got: {:?}",
            survivors
        );
        // Explicit: the oldest three pre-existing backups must have been pruned.
        for deleted in &[
            "test.bak-v10-v11-100.db",
            "test.bak-v11-v12-200.db",
            "test.bak-v12-v13-300.db",
        ] {
            assert!(
                !survivors.iter().any(|s| s == deleted),
                "{} should have been pruned",
                deleted
            );
        }
        // The two newest pre-existing backups must still be present.
        for kept in &["test.bak-v13-v14-400.db", "test.bak-v14-v15-500.db"] {
            assert!(
                survivors.iter().any(|s| s == kept),
                "{} should have been kept (among newest 2)",
                kept
            );
        }
    }

    /// Issue #953, absent-WAL edge case: on a cleanly-closed SQLite DB the
    /// `-wal` and `-shm` sidecars can be absent. The backup must succeed
    /// without them (don't error on missing source), and the happy-path
    /// prune should leave a usable backup .db file behind.
    ///
    /// Regression for the shape where `copy_triplet` would bail on the
    /// first missing sidecar and leave the backup half-written.
    #[test]
    fn test_migrate_backup_works_when_wal_is_absent() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Phase 1: fully write the v19 schema, then TRUNCATE the WAL so
        // there are no sidecar files on disk before migrate() runs.
        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(true)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .foreign_keys(true),
                )
                .await
                .unwrap();
            seed_v19_schema(&pool).await;
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        });

        // Confirm the pre-migration state matches the scenario we want to
        // exercise: main DB present, no sidecars. (On a WAL-mode DB,
        // opening the pool below will recreate them.)
        assert!(db_path.exists(), "main DB should exist");
        let wal_sidecar = {
            let mut s = db_path.as_os_str().to_os_string();
            s.push("-wal");
            std::path::PathBuf::from(s)
        };
        assert!(
            !wal_sidecar.exists(),
            "precondition: no -wal sidecar on disk"
        );

        // Phase 2: reopen and migrate. Backup path should succeed.
        rt.block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&db_path)
                        .create_if_missing(false)
                        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                        .foreign_keys(true),
                )
                .await
                .unwrap();
            let pool = migrate(pool, &db_path, 19, 20).await.unwrap();
            pool.close().await;
        });

        // A valid backup .db exists and parses as SQLite.
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| n.starts_with("test.bak-v19-v20-") && n.ends_with(".db"))
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "expected one backup .db from this run, got: {:?}",
            backups
        );
        let bak_path = dir.path().join(&backups[0]);
        let bak_bytes = std::fs::read(&bak_path).unwrap();
        assert!(
            bak_bytes.starts_with(b"SQLite format 3\0"),
            "backup should be a valid SQLite database file"
        );
    }

    /// E.1 (P1 #16): a DB without a `schema_version` metadata row must end up
    /// with one stamped after `run_migration_tx` completes. The previous
    /// `UPDATE metadata SET value = ?1 WHERE key = 'schema_version'` silently
    /// affected zero rows in this case; the next open would re-run all the
    /// migrations and crash on "duplicate column name" / "table already exists".
    #[test]
    fn test_migration_creates_schema_version_row_if_missing() {
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

            // Build a minimal v10 schema WITHOUT inserting a schema_version row.
            // (We use v10 as the migration baseline because that's where the
            // DDL chain starts in this codebase.)
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

            // Verify the metadata row is absent before migration.
            let pre: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            assert!(
                pre.is_none(),
                "precondition: schema_version row must be absent"
            );

            // Run a single-step migration (10 -> 11). The caller's `from`
            // argument is what `run_migration_tx` falls back to when the
            // metadata row is missing.
            run_migration_tx(&pool, 10, 11).await.unwrap();

            // The row must now exist with the new version stamped on it.
            let post: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                post.map(|(v,)| v),
                Some("11".to_string()),
                "schema_version row must be created with the new version"
            );
        });
    }

    /// v1.28.0 audit P2 #29: v20→v21 adds a `parser_version` column that
    /// defaults to 0 for existing rows. New rows can write any u32 value;
    /// the UPSERT path uses `OR parser_version != excluded.parser_version`
    /// to refresh chunks whose source bytes are unchanged but whose parser
    /// emitted a different `doc` (or other non-content field).
    ///
    /// Round-trip:
    /// - Pre-migration v20 chunk gets `parser_version = 0` after ALTER.
    /// - Post-migration v21 INSERT can stamp any value (e.g. 5) and read it back.
    #[test]
    fn test_migrate_v20_to_v21_adds_parser_version_column() {
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
                        .create_if_missing(true)
                        .foreign_keys(true),
                )
                .await
                .unwrap();

            // Minimal v20 schema: chunks with the v20 column set, metadata.
            // Notably no `parser_version` column — that's what v20→v21 adds.
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    embedding_base BLOB,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '20')")
                .execute(&pool)
                .await
                .unwrap();

            // Seed a pre-migration v20 chunk.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at) \
                 VALUES ('pre_v21', 'file:lib.rs', 'file', 'rust', 'function', 'pre_v21', \
                 '', '', 'h', 1, 10, X'00', '2026-04-12', '2026-04-12')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Run v20 → v21 migration.
            let pool = migrate(pool, &db_path, 20, 21).await.unwrap();

            // Schema version bumped.
            let (v,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(v, "21");

            // Pre-migration row has parser_version = 0 (default).
            let (pre_pv,): (i64,) =
                sqlx::query_as("SELECT parser_version FROM chunks WHERE id = 'pre_v21'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                pre_pv, 0,
                "v20 chunk after migration must default to parser_version = 0"
            );

            // New row written with parser_version = 5 round-trips correctly.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at, parser_version) \
                 VALUES ('post_v21', 'file:lib.rs', 'file', 'rust', 'function', 'post_v21', \
                 '', '', 'h', 1, 10, X'00', '2026-04-12', '2026-04-12', 5)",
            )
            .execute(&pool)
            .await
            .unwrap();
            let (post_pv,): (i64,) =
                sqlx::query_as("SELECT parser_version FROM chunks WHERE id = 'post_v21'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                post_pv, 5,
                "v21 INSERT with parser_version = 5 must round-trip"
            );
        });
    }

    /// v21 → v22: ALTER TABLE chunks ADD umap_x/umap_y REAL (both nullable).
    /// Round-trip: pre-migration v21 chunk gets NULL coords; post-migration
    /// v22 INSERT can stamp arbitrary floats and read them back. Negative,
    /// large-magnitude, and zero values all preserve round-trip.
    #[test]
    fn test_migrate_v21_to_v22_adds_umap_columns() {
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
                        .create_if_missing(true)
                        .foreign_keys(true),
                )
                .await
                .unwrap();

            // Minimal v21 schema — same shape as v20 plus parser_version.
            // Notably no umap_x/umap_y columns — that's what v22 adds.
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    embedding_base BLOB,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0,
                    parser_version INTEGER NOT NULL DEFAULT 0
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '21')")
                .execute(&pool)
                .await
                .unwrap();

            // Seed a pre-migration v21 chunk.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at) \
                 VALUES ('pre_v22', 'file:lib.rs', 'file', 'rust', 'function', 'pre_v22', \
                 '', '', 'h', 1, 10, X'00', '2026-04-21', '2026-04-21')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Run v21 → v22 migration.
            let pool = migrate(pool, &db_path, 21, 22).await.unwrap();

            // Schema version bumped.
            let (v,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(v, "22");

            // Pre-migration row has NULL coords (no DEFAULT — UMAP is opt-in).
            let (pre_x, pre_y): (Option<f64>, Option<f64>) =
                sqlx::query_as("SELECT umap_x, umap_y FROM chunks WHERE id = 'pre_v22'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(pre_x.is_none(), "pre-migration umap_x must be NULL");
            assert!(pre_y.is_none(), "pre-migration umap_y must be NULL");

            // Round-trip a few representative values: positive, negative, zero.
            for (id, x, y) in [
                ("post_v22_a", 1.5_f64, -2.25_f64),
                ("post_v22_b", 0.0_f64, 0.0_f64),
                ("post_v22_c", 1234.567_f64, -9876.54321_f64),
            ] {
                sqlx::query(
                    "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                     signature, content, content_hash, line_start, line_end, embedding, \
                     created_at, updated_at, umap_x, umap_y) \
                     VALUES (?, 'file:lib.rs', 'file', 'rust', 'function', 'post_v22', \
                     '', '', 'h', 1, 10, X'00', '2026-04-21', '2026-04-21', ?, ?)",
                )
                .bind(id)
                .bind(x)
                .bind(y)
                .execute(&pool)
                .await
                .unwrap();
                let (rx, ry): (f64, f64) =
                    sqlx::query_as("SELECT umap_x, umap_y FROM chunks WHERE id = ?")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                assert_eq!(rx, x, "umap_x round-trip for {id}");
                assert_eq!(ry, y, "umap_y round-trip for {id}");
            }
        });
    }

    /// v22 → v23: ALTER TABLE chunks ADD source_size INTEGER + source_content_hash BLOB
    /// (both nullable). Round-trip: pre-migration v22 chunk gets NULL fingerprint;
    /// post-migration v23 INSERT can stamp arbitrary size + 32-byte hash and read
    /// them back. Reconcile fingerprint can decide divergence using mtime, size, or
    /// content_hash interchangeably (issue #1219 / EX-V1.30.1-6).
    #[test]
    fn test_migrate_v22_to_v23_adds_fingerprint_columns() {
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
                        .create_if_missing(true)
                        .foreign_keys(true),
                )
                .await
                .unwrap();

            // Minimal v22 schema — same shape as v21 plus umap_x/umap_y.
            // Notably no source_size/source_content_hash columns — that's what v23 adds.
            sqlx::query("CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    origin TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    language TEXT NOT NULL,
                    chunk_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    signature TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    embedding BLOB NOT NULL,
                    embedding_base BLOB,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    enrichment_version INTEGER NOT NULL DEFAULT 0,
                    parser_version INTEGER NOT NULL DEFAULT 0,
                    umap_x REAL,
                    umap_y REAL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO metadata (key, value) VALUES ('schema_version', '22')")
                .execute(&pool)
                .await
                .unwrap();

            // Seed a pre-migration v22 chunk.
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                 signature, content, content_hash, line_start, line_end, embedding, \
                 created_at, updated_at) \
                 VALUES ('pre_v23', 'file:lib.rs', 'file', 'rust', 'function', 'pre_v23', \
                 '', '', 'h', 1, 10, X'00', '2026-04-29', '2026-04-29')",
            )
            .execute(&pool)
            .await
            .unwrap();

            // Run v22 → v23 migration.
            let pool = migrate(pool, &db_path, 22, 23).await.unwrap();

            // Schema version bumped.
            let (v,): (String,) =
                sqlx::query_as("SELECT value FROM metadata WHERE key = 'schema_version'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(v, "23");

            // Pre-migration row has NULL fingerprint (no DEFAULT — populated on
            // first re-embed of the file).
            let (pre_size, pre_hash): (Option<i64>, Option<Vec<u8>>) = sqlx::query_as(
                "SELECT source_size, source_content_hash FROM chunks WHERE id = 'pre_v23'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(pre_size.is_none(), "pre-migration source_size must be NULL");
            assert!(
                pre_hash.is_none(),
                "pre-migration source_content_hash must be NULL"
            );

            // Round-trip representative fingerprints: zero size, large size, all-zero
            // hash, BLAKE3-shaped 32-byte hash.
            let cases: &[(&str, i64, Vec<u8>)] = &[
                ("post_v23_empty", 0_i64, vec![0u8; 32]),
                ("post_v23_typical", 4096_i64, (0u8..32).collect::<Vec<u8>>()),
                ("post_v23_huge", 10_485_760_i64, vec![0xffu8; 32]),
            ];
            for (id, size, hash) in cases {
                sqlx::query(
                    "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name, \
                     signature, content, content_hash, line_start, line_end, embedding, \
                     created_at, updated_at, source_size, source_content_hash) \
                     VALUES (?, 'file:lib.rs', 'file', 'rust', 'function', 'post_v23', \
                     '', '', 'h', 1, 10, X'00', '2026-04-29', '2026-04-29', ?, ?)",
                )
                .bind(id)
                .bind(*size)
                .bind(hash)
                .execute(&pool)
                .await
                .unwrap();
                let (rsize, rhash): (i64, Vec<u8>) = sqlx::query_as(
                    "SELECT source_size, source_content_hash FROM chunks WHERE id = ?",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(rsize, *size, "source_size round-trip for {id}");
                assert_eq!(&rhash, hash, "source_content_hash round-trip for {id}");
            }
        });
    }
}
