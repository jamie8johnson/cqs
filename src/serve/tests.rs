//! Integration tests for `cqs serve` against an in-memory router.
//!
//! Doesn't bind a TCP port — uses `tower::ServiceExt::oneshot` to
//! drive requests through the axum Router directly. Faster than
//! reqwest + bound socket + multi-thread runtime.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::util::ServiceExt;

use super::{build_router, AppState};
use crate::store::helpers::ModelInfo;
use crate::Store;
use std::sync::Arc;
use tempfile::TempDir;

/// Build a fixture by opening a fresh temp store, initializing it,
/// then re-opening read-only for the handler tree. Returns a
/// [`Fixture`] guard that owns the AppState + TempDir + cleanup
/// behavior — its Drop hands the `Arc<Store>` off to an OS thread
/// so the Store's internal tokio runtime can be dropped without
/// panicking from inside `#[tokio::test]`'s tokio context.
fn fixture_state() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
    let path_for_setup = db_path.clone();
    let ro = std::thread::spawn(move || {
        let store = Store::open(&path_for_setup).expect("open RW");
        store.init(&ModelInfo::default()).expect("init");
        drop(store);
        Store::open_readonly(&path_for_setup).expect("open RO")
    })
    .join()
    .expect("OS thread join");
    Fixture {
        state: Some(AppState {
            store: Arc::new(ro),
        }),
        _dir: Some(dir),
    }
}

/// RAII guard that ensures the contained `AppState` (and therefore the
/// inner `Arc<Store>`) is dropped on a clean OS thread. Required because
/// `Store::Drop` and `Runtime::Drop` both panic inside any tokio context
/// (test runtime, blocking-pool worker, etc.).
struct Fixture {
    state: Option<AppState>,
    _dir: Option<TempDir>,
}

impl Fixture {
    fn state(&self) -> AppState {
        self.state.as_ref().expect("fixture state").clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let state = self.state.take();
        let dir = self._dir.take();
        std::thread::spawn(move || {
            drop(state);
            drop(dir);
        })
        .join()
        .expect("fixture cleanup thread join");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn health_endpoint_returns_ok() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(bytes, "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn index_html_served_at_root() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        ctype.starts_with("text/html"),
        "expected text/html, got {ctype}"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = std::str::from_utf8(&bytes).expect("utf8");
    assert!(body.contains("<title>cqs serve</title>"), "title missing");
    assert!(body.contains("/static/app.css"), "css link missing");
    assert!(body.contains("/static/app.js"), "js link missing");
}

#[tokio::test(flavor = "multi_thread")]
async fn static_asset_serves_css() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/app.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        ctype.starts_with("text/css"),
        "expected text/css, got {ctype}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn static_asset_serves_js() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(ctype.starts_with("application/javascript"));
}

#[tokio::test(flavor = "multi_thread")]
async fn view_modules_serve() {
    // All view modules must be reachable so the router in app.js can
    // dispatch to them.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    for path in &[
        "/static/views/callgraph-2d.js",
        "/static/views/callgraph-3d.js",
        "/static/views/hierarchy-3d.js",
        "/static/views/cluster-3d.js",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(*path).body(Body::empty()).unwrap())
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "view module {path} missing");
        let ctype = resp
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        assert!(
            ctype.starts_with("application/javascript"),
            "{path} has wrong content-type: {ctype}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_3d_bundles_serve() {
    // Three.js + 3d-force-graph must be reachable for the 3D view to
    // boot — the JS module checks for the `ForceGraph3D` global before
    // proceeding, but if the bundle is 404 the global never registers.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    for path in &[
        "/static/vendor/three.min.js",
        "/static/vendor/3d-force-graph.min.js",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(*path).body(Body::empty()).unwrap())
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK, "vendor {path} missing");
        let bytes_count = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        // axum may not always set content-length, so fall back to body len.
        if bytes_count == 0 {
            let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap();
            assert!(
                bytes.len() > 50_000,
                "{path} suspiciously small: {} bytes (vendor bundles are 100s of KB)",
                bytes.len()
            );
        } else {
            assert!(
                bytes_count > 50_000,
                "{path} suspiciously small: {bytes_count} bytes"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn index_html_loads_view_modules() {
    // index.html must reference both view modules and both 3D vendor
    // bundles, otherwise the router can't switch views.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .expect("oneshot");
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = std::str::from_utf8(&bytes).expect("utf8");

    for needle in &[
        "/static/views/callgraph-2d.js",
        "/static/views/callgraph-3d.js",
        "/static/views/hierarchy-3d.js",
        "/static/views/cluster-3d.js",
        "/static/vendor/three.min.js",
        "/static/vendor/3d-force-graph.min.js",
        "view-toggle",
        "view-2d",
        "view-3d",
        "view-cluster",
        "hierarchy-controls",
        "hierarchy-direction",
        "hierarchy-depth",
        "cluster-controls",
        "cluster-color",
    ] {
        assert!(
            body.contains(needle),
            "index.html missing reference to {needle}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_static_asset_returns_404() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/no-such-file.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_endpoint_returns_chunks_count() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert!(json.get("total_chunks").is_some(), "total_chunks missing");
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_returns_empty_for_fresh_store() {
    // Fresh store has no chunks → /api/graph returns the shape but with
    // empty arrays. Real graph rendering is exercised by manual smoke
    // against the cqs corpus; an in-process test would need a populated
    // fixture (~few hundred LOC of chunk inserts) which is more setup
    // than the shape-check is worth at this stage.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/graph")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["nodes"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["edges"].as_array().map(Vec::len), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_accepts_query_filters_without_crash() {
    // Query-param parsing path: fresh store + filters → shape-valid
    // empty response, no 5xx.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/graph?file=src/serve/&type=function&max_nodes=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn search_with_empty_query_returns_empty_matches() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/search?q=")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["matches"].as_array().map(Vec::len), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn chunk_detail_unknown_id_returns_404() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/chunk/no-such-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn hierarchy_unknown_root_returns_404() {
    // Empty fixture has no chunks, so any root id is unknown — we expect
    // a 404 (not a 500 or empty 200) so the frontend can show a clear
    // "no such root" message.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/hierarchy/no-such-id?direction=callees&depth=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn hierarchy_invalid_direction_returns_400() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/hierarchy/anything?direction=sideways&depth=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["error"], "bad_request");
    assert!(
        json["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("direction"),
        "detail should mention 'direction', got {}",
        json["detail"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hierarchy_default_direction_is_callees() {
    // Omitting direction should default to callees (still 404 because
    // no chunks, but the request should be accepted not 400'd).
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/hierarchy/some-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    // Direction defaults to "callees" → not BAD_REQUEST, should be 404 (no chunk).
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn hierarchy_extreme_depth_is_clamped() {
    // depth=999 should be silently clamped to MAX_HIERARCHY_DEPTH (10),
    // not error out. We can't observe the clamp directly without a
    // populated store, but the request should still come back as 404
    // (chunk not found) rather than 400 / 500.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/hierarchy/some-id?direction=callees&depth=999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn cluster_returns_empty_for_fresh_store() {
    // Fresh store has no chunks (and therefore no UMAP coords).
    // The shape should still be valid: nodes:[] and skipped:0.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/embed/2d")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["nodes"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["skipped"].as_u64(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn cluster_accepts_max_nodes_filter() {
    // Query-param parsing path: fresh store + max_nodes → shape-valid
    // empty response, no 5xx.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/embed/2d?max_nodes=100")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
}
