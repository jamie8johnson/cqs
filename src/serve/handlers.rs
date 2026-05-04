//! axum handlers for `cqs serve`. Each one emits a `tracing::info!`
//! event on entry so request flows show up in the journal.
//!
//! All handlers wrap sync `Store` calls in `tokio::task::spawn_blocking`
//! to avoid the "runtime within a runtime" panic that would otherwise
//! fire when the Store's internal `block_on` is invoked from axum's
//! async context. Heavy SQL queries live in `super::data::build_*`.

use crate::store::{ReadOnly, Store};
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

/// #1376 / CQ-V1.33.0-5: shared scaffolding for every async handler that
/// runs a sync `Store` call inside `spawn_blocking`. Centralizes the
/// `Span::current()` capture, `state.blocking_permits` acquire (with the
/// canonical `"blocking permit: {e}"` error string), `spawn_blocking`
/// dispatch, span re-entry inside the closure, permit-hold-via-move,
/// `await`, and join-error mapping. Each handler shrinks from ~30 LOC
/// of scaffolding to one `with_blocking(...)` call plus its actual
/// per-handler arg shaping.
///
/// `label` is used in the join-error string only (`"{label} join: ..."`).
/// Pre-fix the per-handler labels were `"stats join"` / `"graph join"` /
/// etc. — kept stable so existing log greps still match.
///
/// The closure receives `&Store<ReadOnly>` and returns
/// `Result<T, E: Into<ServeError>>` so callers can pass either
/// `build_*` functions (which return `Result<_, StoreError>`) or
/// direct `store.search_by_name(...)` calls without per-call-site error
/// conversions.
async fn with_blocking<T, E, F>(
    state: &AppState,
    label: &'static str,
    f: F,
) -> Result<T, ServeError>
where
    F: FnOnce(&Store<ReadOnly>) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Into<ServeError> + Send + 'static,
{
    // P2.25: capture the per-request span (TraceLayer assigns one) and
    // re-enter it inside the blocking closure so the inner `build_*`
    // span lands as a child of the http_request span. Without this,
    // `RUST_LOG=info` shows `build_*` orphaned from the request and
    // operators can't correlate slow handler latency.
    let span = tracing::Span::current();
    let store = state.store.clone();

    // P2.76: acquire a semaphore permit before queueing the blocking
    // job. Caps concurrent SQL-bound work at `serve_blocking_permits()`
    // so a fan-out client can't pin the full 512-thread axum default
    // blocking pool with idle SQLite handles. Permit is held for the
    // life of the spawned closure via `acquire_owned()` + closure move.
    let permit = state
        .blocking_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ServeError::Internal(format!("blocking permit: {e}")))?;

    // Store's sync API uses its own internal `block_on`. Wrap in
    // `spawn_blocking` to avoid the "runtime within a runtime" panic
    // when called from axum's async context.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        f(&store)
    })
    .await
    .map_err(|e| ServeError::Internal(format!("{label} join: {e}")))?
    .map_err(Into::into)
}

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
    let stats = with_blocking(&state, "stats", super::data::build_stats).await?;
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

    let GraphQuery {
        file,
        kind,
        max_nodes,
    } = params;
    let graph = with_blocking(&state, "graph", move |store| {
        super::data::build_graph(store, file.as_deref(), kind.as_deref(), max_nodes)
    })
    .await?;
    Ok(Json(graph))
}

/// `GET /api/chunk/:id` — sidebar payload for one chunk.
pub(crate) async fn chunk_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ChunkDetail>, ServeError> {
    tracing::info!(chunk_id = %id, "serve::chunk_detail");

    let id_clone = id.clone();
    let detail = with_blocking(&state, "chunk_detail", move |store| {
        super::data::build_chunk_detail(store, &id_clone)
    })
    .await?;

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

    let q = params.q.clone();
    let limit = params.limit.clamp(1, 200);
    let results = with_blocking(&state, "search", move |store| {
        store.search_by_name(&q, limit)
    })
    .await?;

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

    let id_clone = id.clone();
    let response = with_blocking(&state, "hierarchy", move |store| {
        super::data::build_hierarchy(store, &id_clone, direction, depth)
    })
    .await?;

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

    let max_nodes = params.max_nodes;
    let cluster = with_blocking(&state, "cluster", move |store| {
        super::data::build_cluster(store, max_nodes)
    })
    .await?;

    Ok(Json(cluster))
}
