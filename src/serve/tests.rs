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

use super::{allowed_host_set, build_router, AllowedHosts, AppState};
use crate::embedder::Embedding;
use crate::parser::{Chunk, ChunkType, Language};
use crate::store::helpers::ModelInfo;
use crate::Store;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

/// Standard allowlist for unit tests: uses the canonical `127.0.0.1:8080`
/// bind so tests can freely supply that `Host:` header (or none at all)
/// without hitting the DNS-rebinding middleware.
fn test_allowed_hosts() -> AllowedHosts {
    let addr: SocketAddr = "127.0.0.1:8080".parse().expect("parse test bind addr");
    allowed_host_set(&addr)
}

/// Build a router wired up for tests — same production config, but with
/// the fixed test allowlist and auth disabled. Existing handler tests
/// pre-date the auth layer (#1096) and assume routes return 200; new
/// auth-specific tests use [`test_router_with_auth`].
fn test_router(state: AppState) -> axum::Router {
    build_router(state, test_allowed_hosts(), None)
}

/// Build a router with auth enabled. Used by auth-specific tests that
/// pin 401 / cookie-handoff / cross-instance-rejection behavior.
fn test_router_with_auth(state: AppState, token: super::AuthToken) -> axum::Router {
    build_router(state, test_allowed_hosts(), Some(token))
}

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

/// Seed a fresh store with `n_chunks` synthetic Rust function chunks + a
/// ring of caller→callee edges between them, then reopen read-only.
/// When `with_umap` is true, every chunk also gets a deterministic
/// (umap_x, umap_y) pair so `build_cluster` includes it in its response.
/// Returns a [`Fixture`] following the same drop-on-OS-thread discipline
/// as [`fixture_state`].
///
/// Used by the SEC-3 DoS-cap regression tests — needs enough rows for
/// the `LIMIT ?` binding to actually cap, but small enough that the test
/// runs in milliseconds.
fn populated_fixture(n_chunks: usize, with_umap: bool) -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
    let path_for_setup = db_path.clone();
    let ro = std::thread::spawn(move || {
        let store = Store::open(&path_for_setup).expect("open RW");
        store.init(&ModelInfo::default()).expect("init");

        let dim = store.dim();
        for i in 0..n_chunks {
            let name = format!("func_{i:04}");
            let file = format!("src/fake_{}.rs", i % 8);
            let content = format!("fn {name}() {{ /* body */ }}");
            let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
            let chunk = Chunk {
                id: format!("{file}:1:{}:{i}", &hash[..8]),
                file: PathBuf::from(&file),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: name.clone(),
                signature: format!("fn {name}()"),
                content,
                doc: None,
                line_start: 1,
                line_end: 5,
                content_hash: hash,
                parent_id: None,
                window_idx: None,
                parent_type_name: None,
                parser_version: 0,
            };
            // Unit-vector embedding of the store's dim — content doesn't
            // matter for these tests, only that upsert succeeds.
            let mut v = vec![0.0f32; dim];
            if !v.is_empty() {
                v[i % dim] = 1.0;
            }
            let embedding = Embedding::new(v);
            store.upsert_chunk(&chunk, &embedding, Some(100)).unwrap();
        }

        // Populate a ring of call edges so build_graph + build_cluster
        // exercise their edge-fetch paths. Every func_i calls func_(i+1).
        store
            .rt
            .block_on(async {
                for i in 0..n_chunks {
                    let file = format!("src/fake_{}.rs", i % 8);
                    let caller = format!("func_{i:04}");
                    let callee = format!("func_{:04}", (i + 1) % n_chunks);
                    sqlx::query(
                        "INSERT INTO function_calls \
                         (file, caller_name, callee_name, caller_line, call_line) \
                         VALUES (?, ?, ?, 1, 2)",
                    )
                    .bind(&file)
                    .bind(&caller)
                    .bind(&callee)
                    .execute(&store.pool)
                    .await?;
                }

                if with_umap {
                    // Deterministic grid coords — the specific values don't
                    // matter, only that they're non-NULL so the cluster
                    // query's `umap_x IS NOT NULL` filter keeps the row.
                    sqlx::query(
                        "UPDATE chunks \
                         SET umap_x = (rowid * 0.1), umap_y = (rowid * 0.2)",
                    )
                    .execute(&store.pool)
                    .await?;
                }

                Ok::<(), sqlx::Error>(())
            })
            .expect("seed edges/umap");

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

#[tokio::test(flavor = "multi_thread")]
async fn health_endpoint_returns_ok() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    // index.html must reference all view modules + all UI control IDs.
    // The 3D vendor bundles are NOT eagerly loaded — they're injected by
    // app.js's ensureThreeBundle() on first 3D-view activation, so the
    // <script> tags don't appear in the HTML. The vendor paths are still
    // tested separately in `vendor_3d_bundles_serve` to confirm they're
    // reachable when the lazy loader fires.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = test_router(state);

    let resp = app
        .clone()
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

    // Anti-test: the 3D vendor bundles MUST NOT be referenced eagerly in
    // index.html (perf step 4-3 — lazy load via cqsEnsureThreeBundle).
    // Catching a regression here would mean we re-introduced ~1.2 MB of
    // unconditional download on first paint.
    for forbidden in &[
        "<script src=\"/static/vendor/three.min.js\"",
        "<script src=\"/static/vendor/3d-force-graph.min.js\"",
    ] {
        assert!(
            !body.contains(forbidden),
            "index.html eagerly references {forbidden} — should be lazy-loaded"
        );
    }
    // app.js IS expected to contain the lazy-loader plumbing.
    let app_js_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/static/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");
    let app_js_bytes = axum::body::to_bytes(app_js_resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let app_js = std::str::from_utf8(&app_js_bytes).expect("utf8");
    assert!(
        app_js.contains("cqsEnsureThreeBundle"),
        "app.js missing cqsEnsureThreeBundle helper"
    );
    assert!(
        app_js.contains("/static/vendor/three.min.js"),
        "app.js missing three.min.js URL inside lazy loader"
    );
    assert!(
        app_js.contains("/static/vendor/3d-force-graph.min.js"),
        "app.js missing 3d-force-graph.min.js URL inside lazy loader"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gzip_compression_applied_to_json() {
    // Perf step 4-4: CompressionLayer must gzip JSON responses when the
    // client advertises gzip in Accept-Encoding. Without it, the graph
    // payload ships uncompressed (~1-2 MB on the cqs corpus). axum's
    // ServeIcon path doesn't go through CompressionLayer when there's
    // no encoding header, so we explicitly request gzip.
    let fixture = fixture_state();
    let state = fixture.state();
    let app = test_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let encoding = resp
        .headers()
        .get("content-encoding")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert_eq!(
        encoding, "gzip",
        "expected gzip-encoded response when client advertises gzip"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_static_asset_returns_404() {
    let fixture = fixture_state();
    let state = fixture.state();
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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
    let app = test_router(state);

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

// ===== SEC-3: DoS-cap regression tests =====

/// SEC-3: when `max_nodes` is omitted, `build_graph` must still return at
/// most `ABS_MAX_GRAPH_NODES` rows. On a populated corpus this is the
/// behavior that prevents a single unauth GET `/api/graph` from
/// materialising the full chunks table.
///
/// Cheap sanity variant: 150 chunks, verify the response matches the
/// corpus size (150 ≤ 50k cap, so nothing is actually truncated). The
/// important property is that the function runs without needing any
/// explicit cap and the SQL-level LIMIT is bound.
#[test]
fn sec3_build_graph_applies_default_cap_when_max_nodes_omitted() {
    let fixture = populated_fixture(150, false);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let graph = std::thread::spawn(move || super::data::build_graph(&store, None, None, None))
        .join()
        .expect("build_graph join")
        .expect("build_graph ok");

    assert_eq!(
        graph.nodes.len(),
        150,
        "small corpus must pass through the default cap untruncated"
    );
    assert!(
        graph.nodes.len() <= super::data::ABS_MAX_GRAPH_NODES,
        "response exceeded ABS_MAX_GRAPH_NODES"
    );
}

/// SEC-3: an attacker-chosen `max_nodes` that blows past the hard ceiling
/// must be clamped to `ABS_MAX_GRAPH_NODES`. `build_graph` translates this
/// clamp into the SQL `LIMIT` so the over-quota value never reaches the
/// database as-is.
#[test]
fn sec3_build_graph_clamps_excessive_max_nodes() {
    let fixture = populated_fixture(150, false);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    // Arbitrarily large value; cap is 50k so the response is bounded by
    // that, but the corpus is 150 so we see 150 back.
    let graph = std::thread::spawn(move || {
        super::data::build_graph(&store, None, None, Some(1_000_000_000))
    })
    .join()
    .expect("build_graph join")
    .expect("build_graph ok");

    assert_eq!(
        graph.nodes.len(),
        150,
        "populated corpus of 150 < ABS_MAX should return all 150"
    );
    assert!(
        graph.nodes.len() <= super::data::ABS_MAX_GRAPH_NODES,
        "response exceeded ABS_MAX_GRAPH_NODES"
    );
}

/// SEC-3: a modest client-supplied `max_nodes` must still clamp the
/// response even when the corpus is larger. Proves the effective_cap /
/// SQL-LIMIT path works end-to-end for the legitimate UI path
/// (`?max_nodes=50`).
#[test]
fn sec3_build_graph_honors_client_cap_under_hard_limit() {
    let fixture = populated_fixture(150, false);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let graph = std::thread::spawn(move || super::data::build_graph(&store, None, None, Some(50)))
        .join()
        .expect("build_graph join")
        .expect("build_graph ok");

    assert_eq!(
        graph.nodes.len(),
        50,
        "client cap of 50 must truncate the 150-chunk corpus"
    );
}

/// SEC-3: same contract as `build_graph`, applied to `build_cluster`. The
/// cluster endpoint selects from `chunks` WHERE umap_x IS NOT NULL — so
/// the fixture pre-populates UMAP coords to keep every seeded chunk
/// visible to the query.
#[test]
fn sec3_build_cluster_applies_default_cap_when_max_nodes_omitted() {
    let fixture = populated_fixture(120, true);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let cluster = std::thread::spawn(move || super::data::build_cluster(&store, None))
        .join()
        .expect("build_cluster join")
        .expect("build_cluster ok");

    assert_eq!(
        cluster.nodes.len(),
        120,
        "small corpus must pass through the default cap untruncated"
    );
    assert!(
        cluster.nodes.len() <= super::data::ABS_MAX_CLUSTER_NODES,
        "response exceeded ABS_MAX_CLUSTER_NODES"
    );
}

/// SEC-3: an attacker-chosen `max_nodes` that blows past the hard ceiling
/// must be clamped on the cluster endpoint too.
#[test]
fn sec3_build_cluster_clamps_excessive_max_nodes() {
    let fixture = populated_fixture(120, true);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let cluster =
        std::thread::spawn(move || super::data::build_cluster(&store, Some(1_000_000_000)))
            .join()
            .expect("build_cluster join")
            .expect("build_cluster ok");

    assert_eq!(
        cluster.nodes.len(),
        120,
        "populated corpus of 120 < ABS_MAX should return all 120"
    );
    assert!(
        cluster.nodes.len() <= super::data::ABS_MAX_CLUSTER_NODES,
        "response exceeded ABS_MAX_CLUSTER_NODES"
    );
}

/// SEC-3: modest client-supplied cap on cluster endpoint. Drives the
/// post-fetch Rust truncate (since the SQL limit is the default cap,
/// not the client's 40).
#[test]
fn sec3_build_cluster_honors_client_cap_under_hard_limit() {
    let fixture = populated_fixture(120, true);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let cluster = std::thread::spawn(move || super::data::build_cluster(&store, Some(40)))
        .join()
        .expect("build_cluster join")
        .expect("build_cluster ok");

    assert_eq!(
        cluster.nodes.len(),
        40,
        "client cap of 40 must truncate the 120-chunk corpus"
    );
}

// TC-HAP-1.29-1: positive tests for build_graph / build_chunk_detail /
// build_hierarchy / build_cluster against a populated store. The existing
// SEC-3 cases only prove the cap clamps; these prove the functions return
// expected shapes when there's actually data in the store.
//
// `populated_fixture(n, _)` seeds a *ring* of call edges (func_0 → func_1
// → ... → func_{n-1} → func_0), so edge count == node count for every n.

/// Helper: look up the chunk_id for a given function name. Used by the
/// hierarchy + chunk_detail tests which accept an id, not a name.
fn chunk_id_for_name(state: &AppState, name: &str) -> String {
    let store = state.store.clone();
    let name = name.to_string();
    std::thread::spawn(move || {
        store.rt.block_on(async {
            use sqlx::Row;
            let row = sqlx::query("SELECT id FROM chunks WHERE name = ? ORDER BY id LIMIT 1")
                .bind(&name)
                .fetch_one(&store.pool)
                .await
                .expect("chunk row for name");
            row.get::<String, _>("id")
        })
    })
    .join()
    .expect("chunk_id_for_name join")
}

#[test]
fn build_graph_returns_expected_nodes_and_edges_for_populated_store() {
    // `populated_fixture(3, false)` seeds func_0000 → func_0001 → func_0002
    // → func_0000 (3-edge ring).
    let fixture = populated_fixture(3, false);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let graph = std::thread::spawn(move || super::data::build_graph(&store, None, None, None))
        .join()
        .expect("build_graph join")
        .expect("build_graph ok");

    assert_eq!(graph.nodes.len(), 3, "3 seeded chunks");
    assert_eq!(graph.edges.len(), 3, "3-chunk ring → 3 edges");

    // Every node is in the ring, so each should have n_callers == 1 and
    // n_callees == 1. n_callers comes from the global SQL count; n_callees
    // is derived from the resolved visible-edge set.
    for node in &graph.nodes {
        assert_eq!(
            node.n_callers, 1,
            "ring node {} should have 1 caller",
            node.name
        );
        assert_eq!(
            node.n_callees, 1,
            "ring node {} should have 1 callee",
            node.name
        );
    }
}

#[test]
fn build_chunk_detail_returns_callers_callees_tests() {
    // 3-ring: func_0 → func_1 → func_2 → func_0. Pull detail for func_1:
    // exactly one caller (func_0), one callee (func_2), zero tests (the
    // fixture doesn't seed test-kind chunks).
    let fixture = populated_fixture(3, false);
    let state = fixture.state();
    let mid_id = chunk_id_for_name(&state, "func_0001");
    let store = state.store.clone();

    let detail = std::thread::spawn(move || super::data::build_chunk_detail(&store, &mid_id))
        .join()
        .expect("build_chunk_detail join")
        .expect("build_chunk_detail ok")
        .expect("detail present");

    assert_eq!(detail.callers.len(), 1, "one caller (func_0000)");
    assert_eq!(detail.callers[0].name, "func_0000");
    assert_eq!(detail.callees.len(), 1, "one callee (func_0002)");
    assert_eq!(detail.callees[0].name, "func_0002");
    assert_eq!(detail.tests.len(), 0, "no test chunks seeded");
}

#[test]
fn build_hierarchy_walks_callees_to_depth() {
    // 3-ring: BFS from func_0000 along callees with depth=2 visits
    // {func_0000, func_0001, func_0002}. At depth 3 the BFS would wrap
    // back to func_0000 but it's already visited so the frontier stays
    // empty. With depth=1, BFS visits {func_0000, func_0001} — two nodes.
    let fixture = populated_fixture(3, false);
    let state = fixture.state();
    let root_id = chunk_id_for_name(&state, "func_0000");

    let store_d2 = state.store.clone();
    let root_d2 = root_id.clone();
    let h_d2 = std::thread::spawn(move || {
        super::data::build_hierarchy(
            &store_d2,
            &root_d2,
            super::data::HierarchyDirection::Callees,
            2,
        )
    })
    .join()
    .expect("build_hierarchy d=2 join")
    .expect("build_hierarchy d=2 ok")
    .expect("hierarchy d=2 present");

    assert_eq!(
        h_d2.nodes.len(),
        3,
        "depth=2 over a 3-ring covers every node"
    );

    let store_d1 = state.store.clone();
    let root_d1 = root_id.clone();
    let h_d1 = std::thread::spawn(move || {
        super::data::build_hierarchy(
            &store_d1,
            &root_d1,
            super::data::HierarchyDirection::Callees,
            1,
        )
    })
    .join()
    .expect("build_hierarchy d=1 join")
    .expect("build_hierarchy d=1 ok")
    .expect("hierarchy d=1 present");

    assert_eq!(h_d1.nodes.len(), 2, "depth=1 visits root + one callee");
}

#[test]
fn build_cluster_returns_chunks_with_umap_coords() {
    // `with_umap=true` populates umap_x/umap_y for every chunk via the
    // fixture's UPDATE. build_cluster's SELECT filters NULL coords, so
    // all 5 chunks should survive.
    let fixture = populated_fixture(5, true);
    let store = fixture.state.as_ref().expect("fixture state").store.clone();

    let cluster = std::thread::spawn(move || super::data::build_cluster(&store, None))
        .join()
        .expect("build_cluster join")
        .expect("build_cluster ok");

    assert_eq!(cluster.nodes.len(), 5, "5 umap-tagged chunks");

    // Every node carries coords (set to rowid-derived non-zero values).
    // rowid starts at 1, so x = 0.1*rowid and y = 0.2*rowid are both
    // strictly positive finite floats.
    for node in &cluster.nodes {
        assert!(
            node.umap_x.is_finite() && node.umap_x > 0.0,
            "{} umap_x should be positive finite, got {}",
            node.base.name,
            node.umap_x
        );
        assert!(
            node.umap_y.is_finite() && node.umap_y > 0.0,
            "{} umap_y should be positive finite, got {}",
            node.base.name,
            node.umap_y
        );
    }

    // The ring seed produces one incoming + one outgoing edge per node.
    // build_cluster computes per-node degree from the edge scan, so every
    // visible node should reflect those counts — proves the edge query
    // path ran and its results were merged into the response.
    for node in &cluster.nodes {
        assert_eq!(
            node.base.n_callers, 1,
            "{} should reflect one caller from the ring",
            node.base.name
        );
        assert_eq!(
            node.base.n_callees, 1,
            "{} should reflect one callee from the ring",
            node.base.name
        );
    }
}

// SEC-1: DNS-rebinding host-header allowlist tests.
//
// The attack: a page at evil.example.com (DNS-rebound to 127.0.0.1,
// TTL 0) fetches http://evil.example.com:<port>/api/... The browser's
// same-origin model sees this as same-site, but the server is the
// victim's cqs serve on 127.0.0.1. The browser sends `Host: evil.example.com`
// in the request — rejecting that header closes the class.

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_accepts_loopback_ipv4() {
    let fixture = fixture_state();
    let app = test_router(fixture.state());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "127.0.0.1:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_accepts_localhost() {
    let fixture = fixture_state();
    let app = test_router(fixture.state());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "localhost:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_accepts_ipv6_loopback() {
    let fixture = fixture_state();
    let app = test_router(fixture.state());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "[::1]:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_rejects_evil_host() {
    let fixture = fixture_state();
    let app = test_router(fixture.state());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("host", "evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    // Must NOT answer. Status is 400 + plain-text body.
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let body = std::str::from_utf8(&bytes).expect("utf8");
    assert!(
        body.contains("Host"),
        "rejection body should mention Host, got {body:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_rejects_different_port() {
    // Attacker targeting a different port on the same loopback should
    // still be rejected — the allowlist is port-specific.
    let fixture = fixture_state();
    let app = test_router(fixture.state());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("host", "127.0.0.1:9999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_includes_explicit_lan_bind() {
    // When the user explicitly binds to a LAN address, the middleware
    // must accept that exact host:port as well as loopback.
    let fixture = fixture_state();
    let lan_addr: SocketAddr = "192.168.1.50:8080".parse().unwrap();
    let app = build_router(fixture.state(), allowed_host_set(&lan_addr), None);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("host", "192.168.1.50:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_allowlist_passes_when_header_missing() {
    // Requests built via `Request::builder().uri("/...")` without a
    // full URI don't get a Host synthesized. Real HTTP/1.1 traffic
    // always has Host (hyper enforces it); this test documents the
    // defensive-allow for missing headers so unit tests stay ergonomic.
    let fixture = fixture_state();
    let app = test_router(fixture.state());

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
}

// ===== #1096 SEC-7: per-launch auth token integration tests =====
//
// Pins the auth middleware behavior end-to-end through the same
// `build_router` path used in production:
//  - 401 on missing/wrong token (every channel)
//  - 200 with Authorization: Bearer header
//  - 200 with cqs_token cookie
//  - 302/303 + Set-Cookie when ?token=… matches (the redirect handoff)
//  - cross-instance: token from instance A is rejected by instance B
//  - 401 body contains no token-length leak

mod auth_tests {
    use super::*;
    use axum::http::header;

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_required_no_credentials_returns_401() {
        let fixture = fixture_state();
        let token = super::super::AuthToken::from_string("test-token-fixed");
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&bytes[..], b"Unauthorized");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_required_html_root_returns_401() {
        // AC: 401 on HTML routes too, not just /api/*.
        let fixture = fixture_state();
        let token = super::super::AuthToken::from_string("test-token-fixed");
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .expect("oneshot");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_passes_with_bearer_header() {
        let fixture = fixture_state();
        let token_val = "test-token-fixed";
        let token = super::super::AuthToken::from_string(token_val);
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header(header::AUTHORIZATION, format!("Bearer {token_val}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_rejects_wrong_bearer_token() {
        let fixture = fixture_state();
        let token = super::super::AuthToken::from_string("test-token-correct");
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header(header::AUTHORIZATION, "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_passes_with_cookie() {
        let fixture = fixture_state();
        let token_val = "test-cookie-token";
        let token = super::super::AuthToken::from_string(token_val);
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header(header::COOKIE, format!("cqs_token={token_val}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_passes_with_cookie_among_other_cookies() {
        // Real browsers send multiple cookies in one Cookie: header.
        // The middleware splits on `;` and trims each pair.
        let fixture = fixture_state();
        let token_val = "test-cookie-token";
        let token = super::super::AuthToken::from_string(token_val);
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header(
                        header::COOKIE,
                        format!("session=abc; cqs_token={token_val}; pref=dark"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_query_param_redirects_with_set_cookie() {
        let fixture = fixture_state();
        let token_val = "test-query-token";
        let token = super::super::AuthToken::from_string(token_val);
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/?token={token_val}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        // axum's `Redirect::to` defaults to 303 See Other; older
        // versions used 302 Found. Either is acceptable per the
        // RFC for our purposes (the client will follow either with
        // a GET).
        let status = resp.status();
        assert!(
            status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
            "expected 302/303, got {status}"
        );

        let location = resp
            .headers()
            .get(header::LOCATION)
            .expect("Location header on redirect")
            .to_str()
            .unwrap();
        assert_eq!(location, "/", "redirect must strip the token from the URI");

        let cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .expect("Set-Cookie header on redirect")
            .to_str()
            .unwrap();
        assert!(
            cookie.starts_with(&format!("cqs_token={token_val};")),
            "Set-Cookie must include the token: {cookie}"
        );
        assert!(
            cookie.contains("HttpOnly"),
            "Set-Cookie missing HttpOnly: {cookie}"
        );
        assert!(
            cookie.contains("SameSite=Strict"),
            "Set-Cookie missing SameSite=Strict: {cookie}"
        );
        assert!(
            cookie.contains("Path=/"),
            "Set-Cookie missing Path=/: {cookie}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_query_param_strips_token_preserves_other_params() {
        let fixture = fixture_state();
        let token_val = "test-query-token";
        let token = super::super::AuthToken::from_string(token_val);
        let app = test_router_with_auth(fixture.state(), token);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/graph?depth=3&token={token_val}&limit=5"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");

        let location = resp
            .headers()
            .get(header::LOCATION)
            .expect("Location header")
            .to_str()
            .unwrap();
        assert_eq!(location, "/api/graph?depth=3&limit=5");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_token_from_a_is_rejected_by_b() {
        // Two parallel serve instances — token from A must never
        // authenticate against B. AC for #1096.
        let fixture_a = fixture_state();
        let fixture_b = fixture_state();
        let token_a = super::super::AuthToken::from_string("token-instance-a");
        let token_b = super::super::AuthToken::from_string("token-instance-b");
        let app_a = test_router_with_auth(fixture_a.state(), token_a);
        let app_b = test_router_with_auth(fixture_b.state(), token_b);

        // A's token authenticates against A.
        let resp = app_a
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header(header::AUTHORIZATION, "Bearer token-instance-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);

        // A's token is rejected by B.
        let resp = app_b
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header(header::AUTHORIZATION, "Bearer token-instance-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_disabled_when_token_is_none() {
        // Back-compat: `--no-auth` builds the router with `None` and
        // every route works without credentials. The CLI emits a loud
        // warning banner — here we just verify the wire behavior.
        let fixture = fixture_state();
        let app = test_router(fixture.state()); // None auth

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
    }
}
