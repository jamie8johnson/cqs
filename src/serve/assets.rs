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

// Embedded vendor bundles. See assets/vendor/LICENSES.md for sources +
// versions. ~670 KB total — noise vs the ~150 MB cqs binary.
const CYTOSCAPE_JS: &str = include_str!("assets/vendor/cytoscape.min.js");
const DAGRE_JS: &str = include_str!("assets/vendor/dagre.min.js");
const CYTOSCAPE_DAGRE_JS: &str = include_str!("assets/vendor/cytoscape-dagre.min.js");

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
        "vendor/cytoscape.min.js" => (CYTOSCAPE_JS, "application/javascript; charset=utf-8"),
        "vendor/dagre.min.js" => (DAGRE_JS, "application/javascript; charset=utf-8"),
        "vendor/cytoscape-dagre.min.js" => {
            (CYTOSCAPE_DAGRE_JS, "application/javascript; charset=utf-8")
        }
        _ => return Err(ServeError::NotFound(format!("static asset: {path}"))),
    };

    asset_response(body, content_type)
}
