//! Store error types.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("System time error: file mtime before Unix epoch")]
    SystemTime,
    #[error("JSON serialization: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Runtime error: {0}")]
    /// Catch-all for internal assertions (e.g., embedding dimension validation).
    Runtime(String),
    #[error("Not found: {0}")]
    /// Lookup failures: missing metadata keys, unresolved function targets,
    /// file-scoped resolution misses. Lets callers distinguish "doesn't exist"
    /// from other runtime errors for retry/suggest logic.
    NotFound(String),
    #[error("Schema version mismatch in {0}: index is v{1}, cqs expects v{2}. Run 'cqs index --force' to rebuild.")]
    SchemaMismatch(String, i32, i32),
    #[error("Index created by newer cqs version (schema v{0}). Please upgrade cqs.")]
    SchemaNewerThanCq(i32),
    #[error("No migration path from schema v{0} to v{1}. Run 'cqs index --force' to rebuild.")]
    MigrationNotSupported(i32, i32),
    #[error(
        "Model mismatch: index uses \"{0}\" but configured model is \"{1}\".\nRun `cqs index --force` to reindex with the new model."
    )]
    ModelMismatch(String, String),
    #[error(
        "Dimension mismatch: index has {0}-dim embeddings, current model expects {1}. Run 'cqs index --force' to rebuild."
    )]
    DimensionMismatch(u32, u32),
    #[error("Database integrity check failed: {0}")]
    Corruption(String),
    #[error("Embedding blob dimension mismatch: expected {expected}-dim ({expected_bytes} bytes), got {actual_bytes} bytes")]
    EmbeddingBlobMismatch {
        expected: usize,
        expected_bytes: usize,
        actual_bytes: usize,
    },
}
