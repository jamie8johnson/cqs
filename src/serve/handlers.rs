//! axum handlers for `cqs serve`. Each one emits a `tracing::info!`
//! event on entry so request flows show up in the journal.
//!
//! All handlers wrap sync `Store` calls in `tokio::task::spawn_blocking`
//! to avoid the "runtime within a runtime" panic that would otherwise
//! fire when the Store's internal `block_on` is invoked from axum's
//! async context. Heavy SQL queries live in `super::data::build_*`.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use super::data::{ChunkDetail, GraphResponse, NodeRef, SearchResponse, StatsResponse};
use super::error::ServeError;
use super::AppState;

#[derive(Debug, Deserialize)]
pub(crate) struct GraphQuery {
    /// Optional file-path filter — extensibility seam for the future
    /// file/module view (`/api/graph?file=src/store/`).
    #[serde(default)]
    pub file: Option<String>,
    /// Optional chunk-type filter (`/api/graph?type=function`).
    #[serde(default)]
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Optional cap on returned nodes — defensive default for huge
    /// corpora. Spec mentions `?max_nodes=N` as a fallback if 16k
    /// turns out to be too slow even with WebGL.
    #[serde(default)]
    pub max_nodes: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SearchQuery {
    pub q: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    20
}

/// `GET /health` — always returns 200. Used by orchestration / monitoring.
pub(crate) async fn health() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

/// `GET /api/stats` — small payload for the header bar.
pub(crate) async fn stats(
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, ServeError> {
    tracing::info!("serve::stats");

    // Store's sync API uses its own internal `block_on`. Wrap in
    // `spawn_blocking` to avoid the "runtime within a runtime" panic
    // when called from axum's async context.
    let store = state.store.clone();
    let stats = tokio::task::spawn_blocking(move || super::data::build_stats(&store))
        .await
        .map_err(|e| ServeError::Internal(format!("stats join: {e}")))?
        .map_err(ServeError::from)?;

    Ok(Json(stats))
}

/// `GET /api/graph` — full graph (all chunks + call edges), or filtered
/// subset per query params.
pub(crate) async fn graph(
    State(state): State<AppState>,
    Query(params): Query<GraphQuery>,
) -> Result<Json<GraphResponse>, ServeError> {
    tracing::info!(
        file = ?params.file,
        kind = ?params.kind,
        max_nodes = ?params.max_nodes,
        "serve::graph"
    );

    let store = state.store.clone();
    let file = params.file.clone();
    let kind = params.kind.clone();
    let max_nodes = params.max_nodes;
    let graph = tokio::task::spawn_blocking(move || {
        super::data::build_graph(&store, file.as_deref(), kind.as_deref(), max_nodes)
    })
    .await
    .map_err(|e| ServeError::Internal(format!("graph join: {e}")))?
    .map_err(ServeError::from)?;

    Ok(Json(graph))
}

/// `GET /api/chunk/:id` — sidebar payload for one chunk.
pub(crate) async fn chunk_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ChunkDetail>, ServeError> {
    tracing::info!(chunk_id = %id, "serve::chunk_detail");

    let store = state.store.clone();
    let id_clone = id.clone();
    let detail =
        tokio::task::spawn_blocking(move || super::data::build_chunk_detail(&store, &id_clone))
            .await
            .map_err(|e| ServeError::Internal(format!("chunk_detail join: {e}")))?
            .map_err(ServeError::from)?;

    detail
        .map(Json)
        .ok_or_else(|| ServeError::NotFound(format!("chunk: {id}")))
}

/// `GET /api/search?q=foo` — name-based search via FTS5 prefix match.
///
/// Already wired — `Store::search_by_name` is a fast existing path.
/// Highlights matching nodes in the UI.
pub(crate) async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ServeError> {
    tracing::info!(query = %params.q, limit = params.limit, "serve::search");

    if params.q.trim().is_empty() {
        return Ok(Json(SearchResponse {
            matches: Vec::new(),
        }));
    }

    let store = state.store.clone();
    let q = params.q.clone();
    let limit = params.limit.clamp(1, 200);
    let results = tokio::task::spawn_blocking(move || store.search_by_name(&q, limit))
        .await
        .map_err(|e| ServeError::Internal(format!("search join: {e}")))?
        .map_err(ServeError::from)?;

    let matches: Vec<NodeRef> = results
        .into_iter()
        .map(|r| NodeRef {
            id: r.chunk.id.clone(),
            name: r.chunk.name.clone(),
            file: r.chunk.file.display().to_string(),
            line_start: r.chunk.line_start,
        })
        .collect();

    tracing::info!(matches = matches.len(), "search returned");
    Ok(Json(SearchResponse { matches }))
}
