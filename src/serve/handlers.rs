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

use super::data::{
    ChunkDetail, ClusterResponse, GraphResponse, HierarchyDirection, HierarchyResponse, NodeRef,
    SearchResponse, StatsResponse,
};
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

#[derive(Debug, Deserialize)]
pub(crate) struct HierarchyQuery {
    /// `callers` (BFS up) or `callees` (BFS down). Defaults to `callees`.
    #[serde(default)]
    pub direction: Option<String>,
    /// BFS depth from root. Defaults to 5, clamped to 1..=10.
    #[serde(default)]
    pub depth: Option<u32>,
}

const DEFAULT_HIERARCHY_DEPTH: u32 = 5;
const MAX_HIERARCHY_DEPTH: u32 = 10;

#[derive(Debug, Deserialize)]
pub(crate) struct ClusterQuery {
    /// Optional cap on returned nodes — same convention as `/api/graph`.
    #[serde(default)]
    pub max_nodes: Option<usize>,
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

    // P2.25: capture the per-request span and re-enter it inside the
    // blocking closure so the inner `build_*` span lands as a child of
    // the http_request span (TraceLayer) rather than a detached root.
    // Without this, RUST_LOG=info shows `build_stats` orphaned from
    // its request and operators can't correlate slow handler latency.
    //
    // P2.76: acquire a semaphore permit before queueing the blocking
    // job. Caps concurrent SQL-bound work at `serve_blocking_permits()`
    // so a fan-out client can't pin the full 512-thread axum default
    // blocking pool with idle SQLite handles. Permit is held for the
    // life of the spawned closure via `acquire_owned()` + closure move.
    //
    // Store's sync API uses its own internal `block_on`. Wrap in
    // `spawn_blocking` to avoid the "runtime within a runtime" panic
    // when called from axum's async context.
    let span = tracing::Span::current();
    let store = state.store.clone();
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;
    let stats = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        super::data::build_stats(&store)
    })
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

    // P2.25 + P2.76: see `stats` for span/permit rationale.
    let span = tracing::Span::current();
    let store = state.store.clone();
    let file = params.file.clone();
    let kind = params.kind.clone();
    let max_nodes = params.max_nodes;
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;
    let graph = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
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

    // P2.25 + P2.76: see `stats` for span/permit rationale.
    let span = tracing::Span::current();
    let store = state.store.clone();
    let id_clone = id.clone();
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;
    let detail = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        super::data::build_chunk_detail(&store, &id_clone)
    })
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
    // OB-V1.30.1-10: log only metadata at info; full query at debug
    // so it's available for local debugging but not journal-retained
    // by default. The TraceLayer span already has the redacted URI;
    // this used to emit `query = <full text>` at info, which bypassed
    // that redaction and would persist a credential pasted as a
    // search query straight into the journal.
    tracing::debug!(query = %params.q, "serve::search query received");
    tracing::info!(
        q_len = params.q.len(),
        limit = params.limit,
        "serve::search"
    );

    if params.q.trim().is_empty() {
        return Ok(Json(SearchResponse {
            matches: Vec::new(),
        }));
    }

    // P2.25 + P2.76: see `stats` for span/permit rationale.
    let span = tracing::Span::current();
    let store = state.store.clone();
    let q = params.q.clone();
    let limit = params.limit.clamp(1, 200);
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;
    let results = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        store.search_by_name(&q, limit)
    })
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

/// `GET /api/hierarchy/{id}?direction={callers|callees}&depth=N`
///
/// BFS subgraph from a chunk. Returns nodes annotated with `bfs_depth`
/// so the frontend can lock the Z axis to depth and render a tree-shaped
/// 3D layout. Depth is clamped to 1..=10 to bound response size on
/// densely-connected codebases.
pub(crate) async fn hierarchy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HierarchyQuery>,
) -> Result<Json<HierarchyResponse>, ServeError> {
    let direction_str = params.direction.as_deref().unwrap_or("callees").to_string();
    let direction = HierarchyDirection::parse(&direction_str).ok_or_else(|| {
        ServeError::BadRequest(format!(
            "direction must be 'callers' or 'callees', got '{direction_str}'"
        ))
    })?;
    let depth = params
        .depth
        .unwrap_or(DEFAULT_HIERARCHY_DEPTH)
        .clamp(1, MAX_HIERARCHY_DEPTH);

    tracing::info!(
        chunk_id = %id,
        direction = direction.as_str(),
        depth,
        "serve::hierarchy"
    );

    // P2.25 + P2.76: see `stats` for span/permit rationale.
    let span = tracing::Span::current();
    let store = state.store.clone();
    let id_clone = id.clone();
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;
    let response = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        super::data::build_hierarchy(&store, &id_clone, direction, depth)
    })
    .await
    .map_err(|e| ServeError::Internal(format!("hierarchy join: {e}")))?
    .map_err(ServeError::from)?;

    response
        .map(Json)
        .ok_or_else(|| ServeError::NotFound(format!("chunk: {id}")))
}

/// `GET /api/embed/2d?max_nodes=N` — every chunk that has a UMAP projection
/// stored in `umap_x` / `umap_y`, with degree counts attached. The cluster
/// view consumes this directly. Returns an empty `nodes` list (and
/// `skipped > 0`) when the corpus has chunks but no projection has been
/// computed yet — frontend renders a "run `cqs index --umap`" hint.
pub(crate) async fn cluster_2d(
    State(state): State<AppState>,
    Query(params): Query<ClusterQuery>,
) -> Result<Json<ClusterResponse>, ServeError> {
    tracing::info!(max_nodes = ?params.max_nodes, "serve::cluster_2d");

    // P2.25 + P2.76: see `stats` for span/permit rationale.
    let span = tracing::Span::current();
    let store = state.store.clone();
    let max_nodes = params.max_nodes;
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;
    let cluster = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        super::data::build_cluster(&store, max_nodes)
    })
    .await
    .map_err(|e| ServeError::Internal(format!("cluster join: {e}")))?
    .map_err(ServeError::from)?;

    Ok(Json(cluster))
}
