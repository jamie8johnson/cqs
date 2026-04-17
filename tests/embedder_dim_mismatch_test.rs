//! Tests for embedder/index dim-mismatch error handling.
//!
//! Two related guarantees are exercised:
//!
//! 1. **Fix 1 — index-aware embedder resolution.** Given a store recorded
//!    with model X, `ModelConfig::resolve_for_query` returns X regardless of
//!    `CQS_EMBEDDING_MODEL`, CLI flag, or config-file selection. Falls
//!    through to the legacy resolution chain when no stored model is
//!    present.
//!
//! 2. **Fix 2 — hard error on dim mismatch.** When a query embedding's dim
//!    differs from the index's recorded dim, `Store::search_filtered` and
//!    `Store::search_filtered_with_index` return
//!    [`StoreError::QueryDimMismatch`] with an actionable message. This is
//!    the defensive net for when Fix 1 doesn't apply (corrupt meta, custom
//!    config, etc.).

mod common;

use cqs::embedder::{Embedding, EmbeddingConfig, ModelConfig};
use cqs::store::{ModelInfo, SearchFilter, Store, StoreError};
use std::sync::Mutex;
use tempfile::TempDir;

/// Mutex to serialize tests that manipulate `CQS_EMBEDDING_MODEL`. Env vars
/// are process-global so concurrent test threads race on set/remove. Mirrors
/// the same pattern used in `src/embedder/models.rs`'s test mod.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Build a unit-norm `Embedding` of the requested dimension. Caller picks
/// `dim` (cannot rely on `EMBEDDING_DIM` because the whole point of these
/// tests is exercising mismatched dims).
fn mock_embedding_dim(dim: usize, seed: f32) -> Embedding {
    let mut v = vec![seed; dim];
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding::new(v)
}

// ────────────────────────────────────────────────────────────────────────────
// Fix 1 — resolve_for_query
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn resolve_for_query_prefers_stored_model_over_env() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::set_var("CQS_EMBEDDING_MODEL", "v9-200k");
    let resolved = ModelConfig::resolve_for_query(Some("bge-large"), None, None);
    std::env::remove_var("CQS_EMBEDDING_MODEL");
    assert_eq!(resolved.name, "bge-large");
    assert_eq!(resolved.dim, 1024);
}

#[test]
fn resolve_for_query_prefers_stored_model_over_cli_flag() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::remove_var("CQS_EMBEDDING_MODEL");
    let resolved = ModelConfig::resolve_for_query(Some("bge-large"), Some("v9-200k"), None);
    assert_eq!(resolved.name, "bge-large");
    assert_eq!(resolved.dim, 1024);
}

#[test]
fn resolve_for_query_accepts_repo_id_as_stored_name() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::remove_var("CQS_EMBEDDING_MODEL");
    let resolved =
        ModelConfig::resolve_for_query(Some("BAAI/bge-large-en-v1.5"), Some("v9-200k"), None);
    assert_eq!(resolved.name, "bge-large");
    assert_eq!(resolved.dim, 1024);
}

#[test]
fn resolve_for_query_falls_through_when_no_stored_model() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::remove_var("CQS_EMBEDDING_MODEL");
    let resolved = ModelConfig::resolve_for_query(None, Some("v9-200k"), None);
    assert_eq!(resolved.name, "v9-200k");
    assert_eq!(resolved.dim, 768);
}

#[test]
fn resolve_for_query_falls_through_when_stored_unknown() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::remove_var("CQS_EMBEDDING_MODEL");
    let resolved = ModelConfig::resolve_for_query(
        Some("unknown-future-model"),
        Some("v9-200k"),
        Some(&EmbeddingConfig::default()),
    );
    assert_eq!(resolved.name, "v9-200k");
}

#[test]
fn resolve_for_query_falls_through_to_default_when_chain_empty() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::remove_var("CQS_EMBEDDING_MODEL");
    let resolved = ModelConfig::resolve_for_query(None, None, None);
    assert_eq!(resolved.name, "bge-large");
}

// ────────────────────────────────────────────────────────────────────────────
// Fix 2 — Store guards search paths against query/index dim mismatch
// ────────────────────────────────────────────────────────────────────────────

fn store_with_dim(model_name: &str, dim: usize) -> (Store, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
    let store = Store::open(&db_path).expect("open store");
    store
        .init(&ModelInfo::new(model_name, dim))
        .expect("init store");
    drop(store);
    let store = Store::open(&db_path).expect("reopen store");
    (store, dir)
}

#[test]
fn search_filtered_errors_on_dim_mismatch() {
    let (store, _dir) = store_with_dim("test/v9-200k", 768);
    assert_eq!(store.dim(), 768);
    let bad_query = mock_embedding_dim(1024, 0.5);
    let result = store.search_filtered(&bad_query, &SearchFilter::default(), 10, 0.0);
    let err = result.expect_err("dim mismatch must error, not return empty Ok");
    match err {
        StoreError::QueryDimMismatch {
            index_dim,
            query_dim,
            index_model,
            ..
        } => {
            assert_eq!(index_dim, 768);
            assert_eq!(query_dim, 1024);
            assert_eq!(index_model, "test/v9-200k");
        }
        other => panic!("expected QueryDimMismatch, got: {other:?}"),
    }
}

#[test]
fn search_filtered_with_index_errors_on_dim_mismatch() {
    let (store, _dir) = store_with_dim("test/v9-200k", 768);
    let bad_query = mock_embedding_dim(1024, 0.5);
    let err = store
        .search_filtered_with_index(&bad_query, &SearchFilter::default(), 10, 0.0, None)
        .expect_err("dim mismatch must error");
    assert!(matches!(err, StoreError::QueryDimMismatch { .. }));
}

#[test]
fn search_by_candidate_ids_errors_on_dim_mismatch() {
    let (store, _dir) = store_with_dim("test/v9-200k", 768);
    let bad_query = mock_embedding_dim(1024, 0.5);
    let candidate_ids: Vec<&str> = vec!["fake_chunk_id"];
    let err = store
        .search_by_candidate_ids(
            &candidate_ids,
            &bad_query,
            &SearchFilter::default(),
            10,
            0.0,
        )
        .expect_err("dim mismatch must error");
    assert!(matches!(err, StoreError::QueryDimMismatch { .. }));
}

#[test]
fn search_filtered_passes_with_matching_dim() {
    let (store, _dir) = store_with_dim("test/v9-200k", 768);
    let good_query = mock_embedding_dim(768, 0.5);
    let result = store
        .search_filtered(&good_query, &SearchFilter::default(), 10, 0.0)
        .expect("matching dim must succeed");
    assert!(result.is_empty(), "no chunks indexed → empty results");
}

#[test]
fn dim_mismatch_error_message_is_actionable() {
    let (store, _dir) = store_with_dim("test/v9-200k", 768);
    let bad_query = mock_embedding_dim(1024, 0.5);
    let err = store
        .search_filtered(&bad_query, &SearchFilter::default(), 10, 0.0)
        .expect_err("dim mismatch must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("test/v9-200k"),
        "error message must name the index model: {msg}"
    );
    assert!(
        msg.contains("768") && msg.contains("1024"),
        "error message must include both dims: {msg}"
    );
    assert!(
        msg.contains("--force") || msg.contains("CQS_EMBEDDING_MODEL"),
        "error message must point at a fix: {msg}"
    );
}

#[test]
fn search_filtered_message_handles_repo_id_format() {
    let (store, _dir) = store_with_dim("BAAI/bge-large-en-v1.5", 1024);
    let bad_query = mock_embedding_dim(768, 0.5);
    let err = store
        .search_filtered(&bad_query, &SearchFilter::default(), 10, 0.0)
        .expect_err("must error");
    let msg = format!("{err}");
    assert!(msg.contains("BAAI/bge-large-en-v1.5"), "{msg}");
    assert!(msg.contains("1024") && msg.contains("768"), "{msg}");
}
