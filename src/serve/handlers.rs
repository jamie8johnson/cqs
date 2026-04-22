//! axum handlers for `cqs serve`. Each one logs an info span on entry
//! so request flows show up in the journal.
//!
//! v1 implements `/health` + `/api/stats` against the real store and
//! returns stub data for `/api/graph`, `/api/chunk/:id`, `/api/search`.
//! The stubs will be replaced as the implementation order in the spec
//! progresses (step 2 = real `/api/graph`, step 4 = real `/api/chunk/:id`).

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
    let total_chunks = tokio::task::spawn_blocking(move || store.base_embedding_count())
        .await
        .map_err(|e| ServeError::Internal(format!("stats join: {e}")))?
        .map_err(ServeError::from)?;

    // call_edges + type_edges + total_files exposed via store helpers
    // when we wire the real handler in step 6 — for v1 stub them at 0
    // so the header bar still renders.
    Ok(Json(StatsResponse {
        total_chunks,
        total_files: 0,
        call_edges: 0,
        type_edges: 0,
    }))
}

/// `GET /api/graph` — full graph (all chunks + call edges), or filtered
/// subset per query params.
///
/// **STUB**: returns an empty graph in v1-step-1. Wire the real Store
/// query in step 2 of the implementation order. The empty response is
/// a shape-valid placeholder so the frontend can boot against it.
pub(crate) async fn graph(
    State(_state): State<AppState>,
    Query(params): Query<GraphQuery>,
) -> Result<Json<GraphResponse>, ServeError> {
    tracing::info!(
        file = ?params.file,
        kind = ?params.kind,
        max_nodes = ?params.max_nodes,
        "serve::graph"
    );
    tracing::debug!("/api/graph stub returning empty payload");
    Ok(Json(GraphResponse {
        nodes: Vec::new(),
        edges: Vec::new(),
    }))
}

/// `GET /api/chunk/:id` — sidebar payload for one chunk.
///
/// **STUB**: returns NotFound for any id. Wire the real Store fetch in
/// step 4 of the implementation order.
pub(crate) async fn chunk_detail(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ChunkDetail>, ServeError> {
    tracing::info!(chunk_id = %id, "serve::chunk_detail");
    Err(ServeError::NotFound(format!(
        "chunk_detail not yet implemented (id={id})"
    )))
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
