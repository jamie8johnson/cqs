//! `cqs mcp --http` — the Streamable HTTP MCP transport listener (Phase 3a,
//! rung P3a-1: listener + sessions + auth + Origin/Host + plain-JSON, NO SSE).
//!
//! A network security surface. Design fixed by the four-adversary panel in
//! `docs/plans/2026-07-03-mcp-phase3-decisions.md` (3a-D1/D2/D3). Key posture:
//!
//! - **Thin front-end (3a-D1).** Like the stdio bridge, this is a transport-only
//!   client of the daemon unix socket: every `tools/call` relays through the
//!   SAME [`super::bridge::route_with`]. It holds only session state; there is no
//!   in-process fallback (D4a preserved), and a daemon-absent call is a clean
//!   JSON-RPC error, not a panic/500.
//! - **Auth is UNCONDITIONAL (3a-D2).** Any TCP listener widens the daemon's
//!   same-UID `0o600`-socket boundary to every local UID (and, under WSL,
//!   Windows-side processes), and the MCP read set exfiltrates the indexed tree.
//!   So a bearer token is always required — there is no `--no-auth`. The compare
//!   reuses serve's timing-safe `cqs::serve::ct_eq`; the token alphabet is
//!   validated by `cqs::serve::AuthToken`; the token is NEVER logged.
//! - **Loopback default + loopback-only mutations (3a-D2).** A non-loopback bind
//!   fires its OWN loud warning (distinct from serve's `--no-auth` trigger) and
//!   withholds every mutating tool from `tools/list` AND from `tools/call` — the
//!   triple gate `flag ∧ token ∧ loopback`, enforced listener-side by threading
//!   `mutations_enabled() && is_loopback` into `route_with` (the daemon's own
//!   `require_mutations_enabled` is transport-blind).
//! - **Origin/Host (3a-D2).** Present-and-invalid Origin → 403; `Origin: null`
//!   → reject (sandboxed-iframe class); ABSENT Origin → allow (headless agents
//!   send none). serve's `cqs::serve::enforce_host_allowlist` closes
//!   DNS-rebinding for non-browser clients (Host is always present).
//! - **Sessions (3a-D2/D3).** `MCP-Session-Id` is server-issued ONLY — a
//!   client-proposed id on `initialize` is ignored (fixation closed). Ids are
//!   crypto-random visible-ASCII. An unknown/expired session → 404 (client
//!   re-inits). Bearer auth gates POST/GET/DELETE uniformly, so an
//!   unauthenticated DELETE cannot tear down a victim session.
//! - **Protocol floor (3a-D3, no-SSE subset).** Single endpoint; a POST body is a
//!   SINGLE JSON-RPC message (arrays → 400); a POSTed notification/response →
//!   202 no body; a POSTed request → `application/json`; GET → 405 (SSE is
//!   P3a-2); an invalid `MCP-Protocol-Version` → 400, absent → assume
//!   `2025-03-26` (not 400).
//!
//! The whole module is `#[cfg(all(feature = "mcp-http", unix))]` — it relays to
//! the unix daemon socket, so it has no meaning off-unix. A non-unix or
//! feature-off build gets the clean-erroring `serve_http` stub in the parent
//! module.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use serde_json::Value;
use tokio::net::TcpListener;

use super::bridge::route_with;
use super::lifecycle::{
    is_supported_protocol_version, JsonRpcRequest, JsonRpcResponse, DEFAULT_HTTP_PROTOCOL_VERSION,
};
// serve lives in the LIB crate (`cqs::serve`); this listener lives in the
// binary crate, so the reused primitives are named across the lib/bin boundary.
use cqs::serve::{allowed_host_set, ct_eq, enforce_host_allowlist, AllowedHosts, AuthToken};

/// The `MCP-Session-Id` header name (lowercased — HTTP header names are
/// case-insensitive; `HeaderMap::get(&str)` matches case-insensitively and
/// `HeaderName::from_static` requires lowercase).
const SESSION_HEADER: &str = "mcp-session-id";

/// The `MCP-Protocol-Version` header name.
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Idle time-to-live for an HTTP session. A session unused for longer is
/// treated as expired: the next request bearing its id gets a 404 (client
/// re-initializes). Session state is explicitly NON-durable (3a-D1).
const SESSION_IDLE_TTL: Duration = Duration::from_secs(60 * 60);

/// Cap on concurrently-live sessions. A defensive bound so an authenticated but
/// steered client (the RT-RELAY "authorized ≠ benign" threat) cannot exhaust
/// memory by spamming `initialize`. Expired sessions are evicted before the cap
/// is checked, so only genuinely-live sessions count against it.
const MAX_SESSIONS: usize = 4096;

/// Maximum POST body: a single JSON-RPC message is tiny; 1 MiB is ample and
/// bounds a hostile client's per-request buffer.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// The in-memory, non-durable session store: `session-id → last-seen`.
///
/// Server-issued ids only. Lazy expiry on access (no background sweeper for
/// rung 1): [`SessionStore::touch`] removes an expired entry and reports it
/// gone, and [`SessionStore::create`] evicts all expired entries before the cap
/// check. A poisoned mutex is recovered via `into_inner` so one panicked
/// request can't wedge the whole listener.
struct SessionStore {
    inner: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    cap: usize,
}

impl SessionStore {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl: SESSION_IDLE_TTL,
            cap: MAX_SESSIONS,
        }
    }

    /// Issue a fresh server-generated session id, or `None` if the cap is still
    /// reached after evicting expired entries.
    fn create(&self) -> Option<String> {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|_, last| now.duration_since(*last) < self.ttl);
        if map.len() >= self.cap {
            return None;
        }
        let id = new_session_id();
        map.insert(id.clone(), now);
        Some(id)
    }

    /// Touch a session: `true` if it exists and is unexpired (its last-seen is
    /// refreshed); `false` if unknown or expired (an expired entry is removed).
    fn touch(&self, id: &str) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match map.get(id) {
            Some(last) if now.duration_since(*last) < self.ttl => {
                map.insert(id.to_string(), now);
                true
            }
            Some(_) => {
                map.remove(id);
                false
            }
            None => false,
        }
    }

    /// Remove a session; `true` if it was present. Used by DELETE teardown.
    fn remove(&self, id: &str) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(id).is_some()
    }
}

/// Generate a crypto-random session id: 32 random bytes URL-safe base64-encoded
/// (no padding) — 43 chars, all in the visible-ASCII `[A-Za-z0-9_-]` set, which
/// satisfies the Transports session-id charset MUST (0x21–0x7E) and the
/// crypto-secure SHOULD. Same entropy source as [`AuthToken::random`].
fn new_session_id() -> String {
    use base64::Engine;
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Shared handler state. Cloned per layer; the token is `Arc`-backed and the
/// session store is behind an `Arc`, so clones share one store.
#[derive(Clone)]
struct HttpState {
    /// The `.cqs` dir whose daemon socket `route_with` relays to.
    cqs_dir: Arc<PathBuf>,
    /// The required bearer token (unconditional auth — 3a-D2).
    token: AuthToken,
    /// The non-durable session table.
    sessions: Arc<SessionStore>,
    /// Whether mutating tools are permitted on this listener: the triple gate
    /// `mutations_enabled() && bind_is_loopback`. When `false`, mutators are
    /// neither listed nor callable (withheld listener-side, before relay).
    mutations_permitted: bool,
}

/// Run the `cqs mcp --http` listener against the daemon serving `cqs_dir`.
///
/// Binds `bind:port` (default loopback), enforces the unconditional bearer
/// token, and relays each `tools/call` through the daemon socket. Returns when
/// the listener fails or the process is interrupted (SIGINT/SIGTERM).
pub fn serve_http(cqs_dir: &Path, bind: &str, port: u16, token_file: Option<&Path>) -> Result<()> {
    let _span = tracing::info_span!("mcp_serve_http", bind = %bind, port).entered();

    // Resolve "localhost" to a numeric IP before parse (SocketAddr::parse only
    // takes numeric IPs) — mirrors `cmd_serve`.
    let bind_str = if bind == "localhost" {
        "127.0.0.1"
    } else {
        bind
    };
    let bind_addr: SocketAddr = format!("{bind_str}:{port}")
        .parse()
        .with_context(|| format!("failed to parse {bind_str}:{port} as a socket address"))?;
    let is_loopback = bind_addr.ip().is_loopback();

    // Unconditional auth (3a-D2): a token is required to start at all. The
    // token value is never logged — only the source is noted, path-only.
    let token = load_token(token_file)?;

    // Non-loopback bind: its OWN loud-warn trigger (fires WITH a valid token,
    // unlike serve's `--no-auth` trigger) plus the mutation withhold below.
    if !is_loopback {
        tracing::warn!(
            bind = %bind_addr,
            "cqs mcp --http bound to a NON-LOOPBACK address: the bearer token (and any \
             mutation) would travel the LAN in cleartext — there is no TLS in this stack. \
             Mutating tools are withheld from tools/list. Prefer loopback plus an SSH \
             tunnel, or terminate TLS at a reverse proxy."
        );
    }

    // The triple gate, enforced listener-side. The daemon's own mutation gate is
    // transport-blind, so a non-loopback bind narrows the surface here.
    let mutations_permitted = super::mutations_enabled() && is_loopback;

    let state = HttpState {
        cqs_dir: Arc::new(cqs_dir.to_path_buf()),
        token,
        sessions: Arc::new(SessionStore::new()),
        mutations_permitted,
    };
    let allowed_hosts = allowed_host_set(&bind_addr);
    let app = build_router(state, allowed_hosts);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime for the MCP HTTP listener")?;

    runtime.block_on(async move {
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("failed to bind {bind_addr}"))?;
        let actual = listener.local_addr().unwrap_or(bind_addr);
        tracing::info!(
            addr = %actual,
            mutations = mutations_permitted,
            "cqs mcp --http listening (Streamable HTTP transport, no SSE); press Ctrl-C to stop"
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("MCP HTTP listener failed")?;
        tracing::info!("cqs mcp --http shut down cleanly");
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Build the listener's router. Layer order (outermost → innermost): body-limit
/// → Host allowlist → Origin → bearer auth → handler. Auth is the innermost
/// security layer so it runs before any handler for EVERY method — an
/// unauthenticated GET/DELETE never reaches the (inert) GET-405 or the DELETE
/// teardown. Host/Origin sit outside auth (cheaper header rejects first), which
/// leaks no more than serve's own posture.
fn build_router(state: HttpState, allowed_hosts: AllowedHosts) -> Router {
    Router::new()
        .route(
            "/mcp",
            post(handle_post).get(handle_get).delete(handle_delete),
        )
        .with_state(state.clone())
        .layer(from_fn_with_state(state, enforce_bearer_auth))
        .layer(from_fn(enforce_origin))
        .layer(from_fn_with_state(allowed_hosts, enforce_host_allowlist))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            MAX_BODY_BYTES,
        ))
}

/// Graceful-shutdown future: resolves on Ctrl-C or SIGTERM (systemd stop).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = ctrl_c => tracing::info!("ctrl-c received; MCP HTTP listener shutting down"),
        _ = terminate => tracing::info!("SIGTERM received; MCP HTTP listener shutting down"),
    }
}

// ─── auth / origin middleware ────────────────────────────────────────────────

/// Bearer-token auth, applied uniformly to POST/GET/DELETE. The token compare
/// is serve's timing-safe [`ct_eq`]; the server token is never logged, and the
/// reject warn carries only method + path (not the presented credential).
async fn enforce_bearer_auth(State(state): State<HttpState>, req: Request, next: Next) -> Response {
    // Compute the verdict as an owned bool so the header borrow ends before
    // `req` is moved into `next.run`.
    let authed = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|presented| ct_eq(presented, state.token.as_str()))
        .unwrap_or(false);
    if authed {
        next.run(req).await
    } else {
        tracing::warn!(
            method = %req.method(),
            path = %req.uri().path(),
            "mcp-http: rejected unauthenticated request",
        );
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

/// Origin enforcement (3a-D2). ABSENT Origin → allow (headless agents send
/// none). `Origin: null` → reject (sandboxed-iframe class), distinct from
/// absent. A present, non-localhost Origin → 403. An empty Origin value carries
/// no cross-origin information and is treated as absent.
async fn enforce_origin(req: Request, next: Next) -> Response {
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let o = origin.to_str().unwrap_or("");
        if !o.is_empty() && (o == "null" || !is_localhost_origin(o)) {
            tracing::warn!(origin = %o, "mcp-http: rejected request with invalid Origin");
            return (StatusCode::FORBIDDEN, "invalid Origin").into_response();
        }
    }
    next.run(req).await
}

/// Whether `origin` is a localhost origin. Guards the suffix boundary so
/// `http://localhost.evil.com` does NOT match (`localhost` must be followed by
/// end, `:port`, or `/path`).
fn is_localhost_origin(origin: &str) -> bool {
    let origin = origin.to_ascii_lowercase();
    const PREFIXES: [&str; 6] = [
        "http://localhost",
        "http://127.0.0.1",
        "https://localhost",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = origin.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') {
                return true;
            }
        }
    }
    false
}

// ─── method handlers ─────────────────────────────────────────────────────────

/// POST `/mcp` — the single JSON-RPC endpoint (no SSE in rung 1).
async fn handle_post(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    // Protocol-version gate: present-and-unsupported → 400; absent → assume the
    // backward-compat default (NOT a 400).
    if let Some(resp) = check_protocol_version(&headers) {
        return resp;
    }

    // A single JSON-RPC message. Reject arrays (batches) with 400.
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("parse error: body is not valid JSON ({e})"),
            )
                .into_response()
        }
    };
    if value.is_array() {
        return (
            StatusCode::BAD_REQUEST,
            "JSON-RPC batches are not supported; send a single message",
        )
            .into_response();
    }

    // A message with no `method` is a JSON-RPC response (server→client requests
    // are P3a-2+). Accept and ignore → 202 no body.
    let Some(method) = value.get("method").and_then(|m| m.as_str()) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let session_header = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok());

    if method == "initialize" {
        // Server-issued session ONLY: any client-proposed MCP-Session-Id header
        // is IGNORED here (fixation closed) — a fresh id is always minted.
        let new_id = match state.sessions.create() {
            Some(id) => id,
            None => {
                tracing::warn!("mcp-http: session cap reached; refusing new initialize");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "session capacity reached; retry later",
                )
                    .into_response();
            }
        };
        let request: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => {
                state.sessions.remove(&new_id);
                return (
                    StatusCode::BAD_REQUEST,
                    format!("malformed initialize request: {e}"),
                )
                    .into_response();
            }
        };
        let response = match route_blocking(&state, request).await {
            Ok(r) => r,
            Err(resp) => {
                state.sessions.remove(&new_id);
                return resp;
            }
        };
        return json_response(&response, Some(&new_id));
    }

    // Non-initialize: a session is required. Missing header → 400 (client must
    // initialize first); present-but-unknown/expired → 404 (client re-inits).
    let session_id = match session_header {
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "missing MCP-Session-Id header (initialize first)",
            )
                .into_response()
        }
        Some(id) => id.to_string(),
    };
    if !state.sessions.touch(&session_id) {
        return (
            StatusCode::NOT_FOUND,
            "unknown or expired MCP session; re-initialize",
        )
            .into_response();
    }

    let request: JsonRpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("malformed JSON-RPC request: {e}"),
            )
                .into_response()
        }
    };
    // A notification (no id) gets a 202 with no body; a request (id present)
    // gets its JSON-RPC response as application/json.
    let is_notification = request.id.is_none();
    let response = match route_blocking(&state, request).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if is_notification {
        StatusCode::ACCEPTED.into_response()
    } else {
        json_response(&response, None)
    }
}

/// GET `/mcp` — SSE arrives in P3a-2; until then GET opens no stream → 405.
async fn handle_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "GET (SSE) is not supported in this build; POST a JSON-RPC message to this endpoint",
    )
        .into_response()
}

/// DELETE `/mcp` — explicit session teardown. Auth is enforced by the layer, so
/// an UNAUTHENTICATED DELETE never reaches here (it cannot tear down a victim
/// session). A missing session header → 400; an unknown session → 404.
async fn handle_delete(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let session_id = match headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "missing MCP-Session-Id header").into_response(),
    };
    if state.sessions.remove(session_id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "unknown MCP session").into_response()
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Route one request through the shared bridge, off the async worker: the
/// `tools/call` leg does a blocking daemon-socket round-trip, so it runs on a
/// blocking thread. A routing-task panic maps to a 500 rather than aborting.
async fn route_blocking(
    state: &HttpState,
    request: JsonRpcRequest,
) -> Result<JsonRpcResponse, Response> {
    let cqs_dir = Arc::clone(&state.cqs_dir);
    let mutations = state.mutations_permitted;
    match tokio::task::spawn_blocking(move || route_with(&cqs_dir, request, mutations)).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            tracing::error!(error = %e, "mcp-http: routing task panicked");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response())
        }
    }
}

/// Serialize a JSON-RPC response as `application/json` (200), attaching the
/// `MCP-Session-Id` header when `session_id` is `Some` (the initialize reply).
fn json_response(response: &JsonRpcResponse, session_id: Option<&str>) -> Response {
    let body = match serde_json::to_vec(response) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "mcp-http: failed to serialize response");
            return (StatusCode::INTERNAL_SERVER_ERROR, "serialization error").into_response();
        }
    };
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(id) = session_id {
        // The id is base64 URL-safe (visible ASCII) → always a valid header
        // value; the fallible path only logs, never panics.
        match HeaderValue::from_str(id) {
            Ok(v) => {
                resp.headers_mut()
                    .insert(HeaderName::from_static(SESSION_HEADER), v);
            }
            Err(e) => {
                tracing::error!(error = %e, "mcp-http: session id is not a valid header value");
            }
        }
    }
    resp
}

/// Validate the `MCP-Protocol-Version` header. Present-and-unsupported → 400
/// (the Transports MUST); an empty value is treated as absent; absent assumes
/// [`DEFAULT_HTTP_PROTOCOL_VERSION`] (a 400-on-missing would break the
/// backward-compat SHOULD). Returns `Some(response)` for the 400 case, `None`
/// when the header is acceptable (an `Option` rather than `Result<(), Response>`
/// because the axum `Response` Err-variant is large — `result_large_err`).
fn check_protocol_version(headers: &HeaderMap) -> Option<Response> {
    let v = headers.get(PROTOCOL_VERSION_HEADER)?;
    let s = v.to_str().unwrap_or("");
    if !s.is_empty() && !is_supported_protocol_version(s) {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "unsupported MCP-Protocol-Version: {} (absent assumes {})",
                    clip(s, 32),
                    DEFAULT_HTTP_PROTOCOL_VERSION
                ),
            )
                .into_response(),
        );
    }
    None
}

/// Load the required bearer token: `--token-file` (preferred, ownership+mode
/// checked) then `CQS_MCP_HTTP_TOKEN`. Neither present → error (auth is
/// unconditional). The token VALUE is never logged.
fn load_token(token_file: Option<&Path>) -> Result<AuthToken> {
    if let Some(path) = token_file {
        let raw = read_token_file_checked(path)?;
        return AuthToken::try_from_string(raw.trim()).map_err(|e| {
            anyhow::anyhow!(
                "token file {} does not contain a valid token: {e}",
                path.display()
            )
        });
    }
    if let Ok(raw) = std::env::var("CQS_MCP_HTTP_TOKEN") {
        // Path-only tracing: the value is never logged; the env source is noted
        // (and discouraged) so an operator sees where the token came from.
        tracing::info!(
            "cqs mcp --http: bearer token loaded from CQS_MCP_HTTP_TOKEN (env source is \
             discouraged — its value leaks through unit files / exec-capture / child env; \
             prefer --token-file)"
        );
        return AuthToken::try_from_string(raw.trim()).map_err(|e| {
            anyhow::anyhow!("CQS_MCP_HTTP_TOKEN does not contain a valid token: {e}")
        });
    }
    anyhow::bail!(
        "cqs mcp --http requires a bearer token (auth is unconditional): pass --token-file \
         <path> (preferred) or set CQS_MCP_HTTP_TOKEN"
    )
}

/// Read a token file after an ownership + mode check (mirrors the
/// `CQS_PDF_SCRIPT` precedent): the file must be owned by the effective user and
/// not group/world-accessible. Only on a genuine WSL drvfs mount (`/mnt/c/...`),
/// where unix modes are synthetic `0777` and cannot be tightened, is the mode
/// check skipped (with a loud warning) — a path-scoped skip, not a host-global
/// one; the ownership check still applies.
fn read_token_file_checked(path: &Path) -> Result<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::metadata(path)
        .with_context(|| format!("cannot stat token file {}", path.display()))?;

    // Size guard: a token file is tiny; refuse an implausibly large one before
    // reading it into memory.
    if meta.len() > 64 * 1024 {
        anyhow::bail!(
            "token file {} is implausibly large ({} bytes); refusing to read it as a token",
            path.display(),
            meta.len()
        );
    }

    // Ownership: the file must be owned by the effective user. A file another
    // user owns could be swapped out from under us.
    let euid = u64::from(unsafe { libc::geteuid() });
    if u64::from(meta.uid()) != euid {
        anyhow::bail!(
            "token file {} is not owned by the current user (file uid {}, effective uid {}); \
             refusing a token from a file another user controls",
            path.display(),
            meta.uid(),
            euid
        );
    }

    // Mode: not group- or world-accessible. Skipped ONLY on genuine WSL drvfs
    // mounts (mode is synthetic 0777 there and cannot be tightened) — the
    // ownership check above still stands. The skip is deliberately PATH-scoped
    // (`is_wsl_drvfs_path`), never the host-global `is_wsl()`: a bearer token on
    // a native unix fs (tmpfs / ext4) reports real modes even on a WSL host, and
    // skipping the refusal there would disclose it to every local UID — the exact
    // boundary a TCP listener widens. (config.rs ORs in `is_wsl()` for its config
    // permission NAG, a soft warning; this is a hard security refusal, so it must
    // not inherit that host-global lenience.)
    let on_wsl_mount = cqs::config::is_wsl_drvfs_path(path);
    if on_wsl_mount {
        tracing::warn!(
            path = %path.display(),
            "token file is on a WSL drvfs mount; the group/world-readable mode check is \
             skipped (NTFS reports synthetic 0777). Keep the token file on a native unix \
             filesystem (e.g. $XDG_RUNTIME_DIR) for the mode check to apply."
        );
    } else {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "token file {} is group/world-accessible (mode {:o}); tighten it with \
                 `chmod 600 {}`",
                path.display(),
                mode & 0o777,
                path.display()
            );
        }
    }

    std::fs::read_to_string(path)
        .with_context(|| format!("cannot read token file {}", path.display()))
}

/// Clip a string to at most `max` chars on a char boundary — bounds an oversized
/// header value echoed into an error message.
fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::util::ServiceExt;

    /// A valid-alphabet server token used across the router tests.
    const TEST_TOKEN: &str = "test-token-abcDEF-123_xyz";
    /// A wrong-but-valid-alphabet credential a client might present.
    const WRONG_TOKEN: &str = "wrong-token-000";
    /// The port the test router's Host allowlist is built for.
    const TEST_PORT: u16 = 8099;

    fn test_addr() -> SocketAddr {
        format!("127.0.0.1:{TEST_PORT}").parse().expect("addr")
    }

    fn test_state(mutations_permitted: bool) -> HttpState {
        HttpState {
            cqs_dir: Arc::new(PathBuf::from("/nonexistent-cqs-test-dir/.cqs")),
            token: AuthToken::try_from_string(TEST_TOKEN).expect("valid test token"),
            sessions: Arc::new(SessionStore::new()),
            mutations_permitted,
        }
    }

    fn test_router(mutations_permitted: bool) -> Router {
        build_router(
            test_state(mutations_permitted),
            allowed_host_set(&test_addr()),
        )
    }

    /// Builder for a `/mcp` request with a valid Host and optional headers.
    #[derive(Default)]
    struct Req {
        method: &'static str,
        body: String,
        bearer: Option<String>,
        session: Option<String>,
        origin: Option<String>,
        protocol: Option<String>,
        host: Option<String>,
    }

    impl Req {
        fn post(body: &str) -> Self {
            Self {
                method: "POST",
                body: body.to_string(),
                bearer: Some(TEST_TOKEN.to_string()),
                ..Default::default()
            }
        }
        fn get() -> Self {
            Self {
                method: "GET",
                bearer: Some(TEST_TOKEN.to_string()),
                ..Default::default()
            }
        }
        fn delete() -> Self {
            Self {
                method: "DELETE",
                bearer: Some(TEST_TOKEN.to_string()),
                ..Default::default()
            }
        }
        fn bearer(mut self, t: Option<&str>) -> Self {
            self.bearer = t.map(str::to_string);
            self
        }
        fn session(mut self, s: &str) -> Self {
            self.session = Some(s.to_string());
            self
        }
        fn origin(mut self, o: &str) -> Self {
            self.origin = Some(o.to_string());
            self
        }
        fn protocol(mut self, p: &str) -> Self {
            self.protocol = Some(p.to_string());
            self
        }
        fn host(mut self, h: &str) -> Self {
            self.host = Some(h.to_string());
            self
        }
        fn build(self) -> HttpRequest<Body> {
            let host = self
                .host
                .unwrap_or_else(|| format!("127.0.0.1:{TEST_PORT}"));
            let mut b = HttpRequest::builder()
                .method(self.method)
                .uri("/mcp")
                .header("host", host);
            if let Some(t) = self.bearer {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            if let Some(s) = self.session {
                b = b.header(SESSION_HEADER, s);
            }
            if let Some(o) = self.origin {
                b = b.header("origin", o);
            }
            if let Some(p) = self.protocol {
                b = b.header(PROTOCOL_VERSION_HEADER, p);
            }
            b.body(Body::from(self.body)).expect("build request")
        }
    }

    async fn send(router: &Router, req: Req) -> axum::http::Response<Body> {
        router.clone().oneshot(req.build()).await.expect("oneshot")
    }

    async fn body_json(resp: axum::http::Response<Body>) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("collect body");
        if bytes.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&bytes).expect("parse json body")
    }

    const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#;

    /// Initialize and return the server-issued session id.
    async fn init_session(router: &Router) -> String {
        let resp = send(router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK, "initialize must return 200");
        resp.headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("initialize must issue an MCP-Session-Id")
            .to_string()
    }

    // ── endpoint methods ──────────────────────────────────────────────

    #[tokio::test]
    async fn initialize_issues_session_and_returns_json() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let sid = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("session id header");
        assert!(!sid.is_empty());
        // Visible-ASCII charset MUST (0x21–0x7E).
        assert!(sid.bytes().all(|b| (0x21..=0x7e).contains(&b)));
        let body = body_json(resp).await;
        assert_eq!(
            body.get("result")
                .and_then(|r| r.get("protocolVersion"))
                .and_then(|v| v.as_str()),
            Some(super::super::lifecycle::MCP_PROTOCOL_VERSION)
        );
    }

    #[tokio::test]
    async fn post_notification_returns_202_no_body() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(bytes.is_empty(), "202 notification must carry no body");
    }

    #[tokio::test]
    async fn post_response_no_method_returns_202() {
        // A JSON-RPC response (no `method`) posted by the client is inert → 202.
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn get_returns_405_no_sse_yet() {
        let router = test_router(true);
        let resp = send(&router, Req::get()).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn delete_tears_down_session() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(&router, Req::delete().session(&sid)).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        // The session is gone: a follow-up non-initialize request 404s.
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_method_verb_returns_405() {
        let router = test_router(true);
        let req = HttpRequest::builder()
            .method("PUT")
            .uri("/mcp")
            .header("host", format!("127.0.0.1:{TEST_PORT}"))
            .header("authorization", format!("Bearer {TEST_TOKEN}"))
            .body(Body::empty())
            .expect("req");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ── auth ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn valid_token_authenticates() {
        let router = test_router(true);
        // A valid token reaches the handler (initialize succeeds).
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_token_is_401() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).bearer(Some(WRONG_TOKEN))).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_token_is_401() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).bearer(None)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unauthenticated_delete_is_401_not_teardown() {
        // A session exists; an UNAUTH DELETE naming it must 401, not tear it down.
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(&router, Req::delete().bearer(None).session(&sid)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // The victim session survives: an authed request on it still works.
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unauthenticated_get_is_401() {
        let router = test_router(true);
        let resp = send(&router, Req::get().bearer(None)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// The token (server's OR a presented one) never appears in any log line.
    #[test]
    fn token_never_logged_on_auth_reject() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CaptureWriter(Arc::clone(&buf)))
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async {
                let router = test_router(true);
                // Reject path logs the warn (method+path only).
                let resp = send(&router, Req::post(INIT_BODY).bearer(Some(WRONG_TOKEN))).await;
                assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            });
        });
        let logs = String::from_utf8(buf.lock().expect("lock").clone()).expect("utf8");
        assert!(
            !logs.contains(TEST_TOKEN),
            "server token must never appear in logs; got:\n{logs}"
        );
        assert!(
            !logs.contains(WRONG_TOKEN),
            "presented credential must never appear in logs; got:\n{logs}"
        );
    }

    /// A capturing `MakeWriter` that appends to a shared buffer.
    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    // ── token source (file mode / env) ────────────────────────────────

    #[test]
    fn token_file_good_mode_loads() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(f, "{TEST_TOKEN}").expect("write"); // trailing newline is trimmed
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        // On a native unix fs (tempdir → /tmp) the mode check applies and passes.
        let token = load_token(Some(&path)).expect("token file should load");
        assert_eq!(token.as_str(), TEST_TOKEN);
    }

    #[test]
    fn token_file_group_readable_refused() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, "{TEST_TOKEN}").expect("write");
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        // Skip ONLY on a genuine drvfs tempdir where modes are synthetic (the
        // mode check is intentionally skipped there, so this negative case can't
        // fire). Path-scoped, NOT host-global `is_wsl()`: on a native tempdir a
        // WSL host still reports real modes, and the refusal must apply — else
        // this guard is vacuous on the project's own dev box.
        if cqs::config::is_wsl_drvfs_path(&path) {
            return;
        }
        let err = load_token(Some(&path)).expect_err("group-readable token must be refused");
        assert!(
            err.to_string().contains("group/world-accessible"),
            "unexpected error: {err}"
        );
    }

    #[test]
    #[serial_test::serial(cqs_mcp_http_token_env)]
    fn env_token_fallback_loads() {
        // SAFETY: serial-gated; restored below.
        let prior = std::env::var("CQS_MCP_HTTP_TOKEN").ok();
        std::env::set_var("CQS_MCP_HTTP_TOKEN", TEST_TOKEN);
        let token = load_token(None).expect("env token should load");
        assert_eq!(token.as_str(), TEST_TOKEN);
        match prior {
            Some(p) => std::env::set_var("CQS_MCP_HTTP_TOKEN", p),
            None => std::env::remove_var("CQS_MCP_HTTP_TOKEN"),
        }
    }

    #[test]
    #[serial_test::serial(cqs_mcp_http_token_env)]
    fn no_token_source_is_error() {
        let prior = std::env::var("CQS_MCP_HTTP_TOKEN").ok();
        std::env::remove_var("CQS_MCP_HTTP_TOKEN");
        let err = load_token(None).expect_err("no token source must error");
        assert!(err.to_string().contains("requires a bearer token"));
        if let Some(p) = prior {
            std::env::set_var("CQS_MCP_HTTP_TOKEN", p);
        }
    }

    /// REGRESSION GUARD — RT token-file breach.
    ///
    /// A group/world-readable token file on a NATIVE unix filesystem must be
    /// REFUSED (3a-D2: the mode check is the protection that keeps the bearer
    /// token from every local UID, the boundary a TCP listener widens).
    ///
    /// This guards against a reintroduction of the host-global skip disjunct:
    /// `read_token_file_checked` once ORed `is_wsl()` (host-global) with
    /// `is_wsl_drvfs_path(path)` (path-scoped), so on ANY WSL host the mode
    /// check was skipped for EVERY path — a 0644 token on tmpfs/ext4 (e.g.
    /// `$XDG_RUNTIME_DIR`) was ACCEPTED, disclosing it to every local UID. The
    /// skip is now path-scoped (`is_wsl_drvfs_path` alone), so a native-fs
    /// tempdir must trip the refusal on a WSL dev box and non-WSL CI alike. The
    /// `assert!` below pins the precondition (tempdir is native, not drvfs) so a
    /// green pass can only mean the refusal actually fired.
    #[test]
    fn token_file_group_readable_refused_on_native_fs() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).expect("create");
        write!(f, "{TEST_TOKEN}").expect("write");
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        // The precondition the finding rests on: a tempdir is a NATIVE fs, NOT
        // a `/mnt/<x>` drvfs mount, so the mode check MUST apply here — the skip
        // must be path-scoped, never host-global.
        assert!(
            !cqs::config::is_wsl_drvfs_path(&path),
            "test dir must be native fs (not a drvfs mount): {}",
            path.display()
        );
        let err = load_token(Some(&path))
            .expect_err("a group/world-readable token file on a NATIVE unix fs must be refused");
        assert!(
            err.to_string().contains("group/world-accessible"),
            "unexpected error: {err}"
        );
    }

    // ── origin / host ─────────────────────────────────────────────────

    #[tokio::test]
    async fn origin_present_valid_ok() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(INIT_BODY).origin("http://localhost:5173"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_present_invalid_403() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(INIT_BODY).origin("http://evil.example.com"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_absent_ok() {
        // No Origin header → allowed (headless agents send none).
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn origin_null_rejected() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).origin("null")).await;
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "Origin: null is rejected (distinct from absent)"
        );
    }

    #[tokio::test]
    async fn localhost_subdomain_origin_rejected() {
        // `http://localhost.evil.com` must NOT pass the suffix-boundary check.
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(INIT_BODY).origin("http://localhost.evil.com"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn missing_host_rejected() {
        // The Host allowlist (reused from serve) rejects a missing/disallowed Host.
        let router = test_router(true);
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", format!("Bearer {TEST_TOKEN}"))
            .body(Body::from(INIT_BODY))
            .expect("req"); // no Host header
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn disallowed_host_rejected() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).host("evil.example.com")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── sessions ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn client_proposed_session_id_ignored_fixation() {
        // A client presents its OWN MCP-Session-Id on initialize; the server
        // MUST issue a fresh one instead (fixation closed).
        let router = test_router(true);
        let attacker_id = "attacker-chosen-session-id";
        let resp = send(&router, Req::post(INIT_BODY).session(attacker_id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let issued = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("issued session id");
        assert_ne!(
            issued, attacker_id,
            "server must NOT adopt a client-proposed session id"
        );
    }

    #[tokio::test]
    async fn unknown_session_is_404() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
                .session("never-issued-session"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn non_initialize_without_session_is_400() {
        let router = test_router(true);
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn session_store_expiry_and_teardown() {
        let store = SessionStore {
            inner: Mutex::new(HashMap::new()),
            ttl: Duration::from_millis(0), // everything is immediately expired
            cap: MAX_SESSIONS,
        };
        let id = store.create().expect("create");
        // ttl=0 → any elapsed time expires it; touch reports gone and removes it.
        assert!(!store.touch(&id), "zero-ttl session must read as expired");
        assert!(
            !store.remove(&id),
            "expired session already evicted by touch"
        );

        let live = SessionStore::new();
        let id = live.create().expect("create");
        assert!(live.touch(&id), "fresh session is live");
        assert!(live.remove(&id), "remove reports the session was present");
        assert!(!live.touch(&id), "removed session is gone");
    }

    #[test]
    fn session_cap_refuses_beyond_limit() {
        let store = SessionStore {
            inner: Mutex::new(HashMap::new()),
            ttl: SESSION_IDLE_TTL,
            cap: 2,
        };
        assert!(store.create().is_some());
        assert!(store.create().is_some());
        assert!(store.create().is_none(), "third create must hit the cap");
    }

    // ── mutation withhold (listener-side triple gate) ─────────────────

    #[tokio::test]
    async fn mutation_tools_absent_when_not_permitted() {
        // mutations_permitted=false models a non-loopback bind (or flag off):
        // tools/list must NOT advertise cqs_notes_add.
        let router = test_router(false);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let names = tool_names(&body);
        assert!(
            !names.iter().any(|n| n == "cqs_notes_add"),
            "mutators must be withheld from tools/list off-loopback; got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "cqs_search"),
            "read tools must still be present"
        );
    }

    #[tokio::test]
    async fn mutation_tools_present_when_permitted() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"{"jsonrpc":"2.0","id":6,"method":"tools/list"}"#).session(&sid),
        )
        .await;
        let body = body_json(resp).await;
        let names = tool_names(&body);
        assert!(
            names.iter().any(|n| n == "cqs_notes_add"),
            "mutators must be present when permitted; got {names:?}"
        );
    }

    #[tokio::test]
    async fn mutation_call_withheld_is_method_not_found() {
        // Off-loopback, a tools/call naming a mutator resolves to no tool →
        // -32601, BEFORE any daemon relay (so the daemon-missing -32603 the
        // permitted path would give never appears).
        let router = test_router(false);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"cqs_notes_add","arguments":{"text":"x","sentiment":0.5}}}"#,
            )
            .session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32601),
            "withheld mutator must be method-not-found, not relayed: {body}"
        );
    }

    fn tool_names(body: &Value) -> Vec<String> {
        body.get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    // ── protocol version ──────────────────────────────────────────────

    #[tokio::test]
    async fn protocol_version_invalid_400() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).protocol("1999-01-01")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn protocol_version_absent_assumes_default_not_400() {
        // No MCP-Protocol-Version header → assume 2025-03-26, NOT a 400.
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY)).await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "absent protocol version must assume the default, not 400"
        );
    }

    #[tokio::test]
    async fn protocol_version_supported_ok() {
        let router = test_router(true);
        let resp = send(&router, Req::post(INIT_BODY).protocol("2025-06-18")).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── batch rejection / relay ───────────────────────────────────────

    #[tokio::test]
    async fn json_array_batch_rejected_400() {
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).session(&sid),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// REGRESSION GUARD — a POST body over [`MAX_BODY_BYTES`] (1 MiB) that
    /// DECLARES its size via `Content-Length` is refused by the outermost
    /// body-limit layer with 413 BEFORE the handler (or auth) allocates it,
    /// bounding a hostile client's per-request buffer. The layer sits OUTSIDE
    /// auth, so the cap applies even to an unauthed request (no OOM-then-401).
    /// Verified against the live listener (a real 2 MiB POST, which curl sends
    /// with `Content-Length`, → `413 length limit exceeded`); the header is set
    /// explicitly here because the in-process `oneshot` path does not synthesize
    /// one, and without a declared length the limit layer defers to lazy body
    /// reads (auth then 401s before the body is ever buffered — also safe, but a
    /// different code path). Pinned so a future loosening of the cap — or a
    /// reorder that moves the limit inside auth — trips.
    #[tokio::test]
    async fn oversized_body_rejected_by_body_limit() {
        let router = test_router(true);
        let pad = "x".repeat(MAX_BODY_BYTES + 1);
        let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","pad":"{pad}"}}"#);
        let declared_len = body.len();
        // Unauthenticated on purpose: the body cap must bite before auth. The
        // declared Content-Length mirrors what a real HTTP client sends.
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", format!("127.0.0.1:{TEST_PORT}"))
            .header(header::CONTENT_LENGTH, declared_len)
            .body(Body::from(body))
            .expect("req");
        let resp = router.clone().oneshot(req).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn tool_call_daemon_absent_is_clean_jsonrpc_error() {
        // A real tool call relays through bridge::route_with to the daemon;
        // with no daemon at the (nonexistent) cqs_dir it is a clean JSON-RPC
        // -32603 in the body — NOT a panic or a 500.
        let router = test_router(true);
        let sid = init_session(&router).await;
        let resp = send(
            &router,
            Req::post(
                r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"cqs_search","arguments":{"query":"retry backoff"}}}"#,
            )
            .session(&sid),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "daemon-absent is a body-level error, not HTTP 500"
        );
        let body = body_json(resp).await;
        assert_eq!(
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
            Some(-32603),
            "daemon-absent relay must be a clean -32603: {body}"
        );
        let msg = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(
            msg.contains("daemon"),
            "error must mention the daemon: {msg}"
        );
    }

    // ── origin helper unit ────────────────────────────────────────────

    #[test]
    fn is_localhost_origin_boundaries() {
        assert!(is_localhost_origin("http://localhost"));
        assert!(is_localhost_origin("http://localhost:3000"));
        assert!(is_localhost_origin("http://127.0.0.1:8080"));
        assert!(is_localhost_origin("https://[::1]:9000"));
        assert!(is_localhost_origin("http://LOCALHOST:3000")); // case-folded
        assert!(!is_localhost_origin("http://localhost.evil.com"));
        assert!(!is_localhost_origin("http://notlocalhost"));
        assert!(!is_localhost_origin("null"));
        assert!(!is_localhost_origin("https://example.com"));
    }
}
