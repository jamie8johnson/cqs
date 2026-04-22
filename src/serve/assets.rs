//! Static asset serving — all files baked into the binary at compile
//! time via `include_str!`. No filesystem reads at request time.
//!
//! v1 ships index.html + app.css + app.js. Step 3 of the implementation
//! order adds the Cytoscape.js + dagre vendor bundles (~500KB combined).

use axum::{
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::Response,
};

use super::error::ServeError;

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_CSS: &str = include_str!("assets/app.css");
const APP_JS: &str = include_str!("assets/app.js");

// View modules — one per renderer. The router (app.js) dispatches
// between them based on the URL `?view=` parameter. See
// `docs/plans/2026-04-22-cqs-serve-3d-progressive.md` (step 1 added
// the renderer abstraction; step 2 added the hierarchy view).
const CALLGRAPH_2D_JS: &str = include_str!("assets/views/callgraph-2d.js");
const CALLGRAPH_3D_JS: &str = include_str!("assets/views/callgraph-3d.js");
const HIERARCHY_3D_JS: &str = include_str!("assets/views/hierarchy-3d.js");

// Embedded vendor bundles. See assets/vendor/LICENSES.md for sources +
// versions. Total ~2.4 MB — noise vs the ~150 MB cqs binary.
const CYTOSCAPE_JS: &str = include_str!("assets/vendor/cytoscape.min.js");
const DAGRE_JS: &str = include_str!("assets/vendor/dagre.min.js");
const CYTOSCAPE_DAGRE_JS: &str = include_str!("assets/vendor/cytoscape-dagre.min.js");
const THREE_JS: &str = include_str!("assets/vendor/three.min.js");
const FORCE_GRAPH_3D_JS: &str = include_str!("assets/vendor/3d-force-graph.min.js");

/// Build the HTML/CSS/JS response. Helper used by both
/// the `/` route and `/static/*` paths.
fn asset_response(body: &'static str, content_type: &'static str) -> Result<Response, ServeError> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(axum::body::Body::from(body))
        .map_err(|e| ServeError::Internal(format!("failed to build response: {e}")))
}

/// `GET /` — embedded SPA shell.
pub(crate) async fn index_html() -> Result<Response, ServeError> {
    tracing::info!("serve::index_html");
    asset_response(INDEX_HTML, "text/html; charset=utf-8")
}

/// `GET /static/{path}` — serves embedded CSS / JS / vendor bundles.
///
/// Paths outside the embedded set return 404. There's no filesystem
/// fallthrough — keeps the binary the single source of truth.
pub(crate) async fn static_asset(Path(path): Path<String>) -> Result<Response, ServeError> {
    tracing::info!(path = %path, "serve::static_asset");

    let (body, content_type) = match path.as_str() {
        "app.css" => (APP_CSS, "text/css; charset=utf-8"),
        "app.js" => (APP_JS, "application/javascript; charset=utf-8"),
        "views/callgraph-2d.js" => (CALLGRAPH_2D_JS, "application/javascript; charset=utf-8"),
        "views/callgraph-3d.js" => (CALLGRAPH_3D_JS, "application/javascript; charset=utf-8"),
        "views/hierarchy-3d.js" => (HIERARCHY_3D_JS, "application/javascript; charset=utf-8"),
        "vendor/cytoscape.min.js" => (CYTOSCAPE_JS, "application/javascript; charset=utf-8"),
        "vendor/dagre.min.js" => (DAGRE_JS, "application/javascript; charset=utf-8"),
        "vendor/cytoscape-dagre.min.js" => {
            (CYTOSCAPE_DAGRE_JS, "application/javascript; charset=utf-8")
        }
        "vendor/three.min.js" => (THREE_JS, "application/javascript; charset=utf-8"),
        "vendor/3d-force-graph.min.js" => {
            (FORCE_GRAPH_3D_JS, "application/javascript; charset=utf-8")
        }
        _ => return Err(ServeError::NotFound(format!("static asset: {path}"))),
    };

    asset_response(body, content_type)
}
