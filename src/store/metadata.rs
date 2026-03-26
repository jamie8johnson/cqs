//! Metadata get/set and version validation for the Store.

use std::path::Path;

use super::migrations;
use super::{
    NoteSummary, Store, StoreError, CURRENT_SCHEMA_VERSION, EXPECTED_DIMENSIONS, MODEL_NAME,
};

impl Store {
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
    pub(crate) fn check_schema_version(&self, path: &Path) -> Result<(), StoreError> {
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
    pub(crate) fn check_model_version(&self) -> Result<(), StoreError> {
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
    pub(crate) fn check_cq_version(&self) {
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
    pub(crate) fn set_metadata_opt(
        &self,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), StoreError> {
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
    pub(crate) fn get_metadata_opt(&self, key: &str) -> Result<Option<String>, StoreError> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::helpers::ModelInfo;
    use crate::test_helpers::setup_store;

    // ===== TC-8: pending batch ID =====

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
}
