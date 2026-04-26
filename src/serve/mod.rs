//! `cqs serve` — interactive web UI for exploring the cqs index.
//!
//! Spec: `docs/plans/2026-04-21-cqs-serve-v1.md`. v1 ships a single
//! interactive view (call graph + chunk-detail sidebar + search) bound
//! to 127.0.0.1 by default, with the architecture deliberately open
//! for the parked `graph-visualization.md` 6-view design.
//!
//! # Architecture
//! - `axum` HTTP server reading from a `Store<ReadOnly>`
//! - Frontend is one HTML page + Cytoscape.js, all embedded in the binary
//!   via `include_str!` / `include_bytes!`
//! - No auth, no WebSocket, no live updates — single-user local exploration
//!
//! # Threading
//! `run_server` is async-friendly but synchronous from the caller's
//! perspective. It builds its own tokio runtime and blocks on the server
//! future until SIGINT or the listener exits.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::Response,
    routing::get,
    Router,
};
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::store::{ReadOnly, Store};

mod assets;
mod auth;
mod data;
mod error;
mod handlers;

#[cfg(test)]
mod tests;

pub use auth::AuthToken;
pub use error::ServeError;

/// Shared state passed to every axum handler. Wraps a read-only store
/// behind an `Arc` so the handler tree can read concurrently.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<Store<ReadOnly>>,
}

/// Allowed `Host` header values, built at router-build time from the
/// bind address. Shared via `Arc` so the middleware closure is cheap to
/// clone per-request.
pub(crate) type AllowedHosts = Arc<HashSet<String>>;

/// Run the `cqs serve` HTTP server.
///
/// Binds to `bind_addr` (default `127.0.0.1:8080`), serves the embedded
/// HTML shell at `/`, and answers JSON queries against `store` for
/// `/api/graph`, `/api/chunk/:id`, `/api/search`, `/api/stats`.
///
/// Returns when the listener fails or the process is interrupted.
/// `quiet` suppresses the "listening on" stdout banner so test code
/// can run the server without polluting test output.
///
/// `auth` is the per-launch token (#1096). Pass `Some(token)` to require
/// authentication on every route via [`auth::enforce_auth`]; pass `None`
/// for the back-compat unauthenticated mode (the CLI calls this when
/// `--no-auth` is set, after emitting a loud-warning banner). The
/// caller is responsible for emitting the token in the "listening on"
/// banner, since `quiet=true` callers (tests) construct their own URL.
pub fn run_server(
    store: Store<ReadOnly>,
    bind_addr: SocketAddr,
    quiet: bool,
    auth: Option<AuthToken>,
) -> Result<()> {
    let _span = tracing::info_span!("serve", addr = %bind_addr).entered();

    let state = AppState {
        store: Arc::new(store),
    };
    let allowed_hosts = allowed_host_set(&bind_addr);
    let app = build_router(state, allowed_hosts, auth.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime for cqs serve")?;

    runtime.block_on(async move {
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("Failed to bind {bind_addr}"))?;
        let actual = listener
            .local_addr()
            .with_context(|| format!("Failed to read local_addr after bind {bind_addr}"))?;

        if !quiet {
            // #1096: when auth is enabled, emit the paste-ready URL
            // (token + bind addr) so a fresh launch is one click away
            // from being usable. The token appears here once and is
            // never logged via tracing — auditors can grep for serve
            // banners separately from the structured log stream.
            //
            // P1.13 / SEC: route the token-bearing banner to stderr when
            // stdout is not a TTY. systemd `StandardOutput=journal` and
            // container log drivers persist stdout into a 30-day retention
            // store — stderr is similarly captured but is the conventional
            // place for "informational interactive output" and operators
            // can redirect it (`2>/dev/null`) without losing structured
            // logs. For a stronger guarantee, add `--no-banner` (TODO).
            let stdout_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
            match auth.as_ref() {
                Some(token) => {
                    let line = format!(
                        "cqs serve listening on http://{actual}/?token={}",
                        token.as_str()
                    );
                    if stdout_is_tty {
                        println!("{line}");
                    } else {
                        eprintln!("{line}");
                        eprintln!(
                            "(stdout is not a TTY; token-bearing banner routed to stderr to \
                             avoid persisting into journald/container logs)"
                        );
                    }
                }
                None => {
                    println!("cqs serve listening on http://{actual}");
                    eprintln!(
                        "WARN: --no-auth in use — anyone with network access to {actual} \
                         can read this index"
                    );
                }
            }
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, auth_enabled = auth.is_some(), "cqs serve started");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("axum server failed")?;

        tracing::info!("cqs serve shut down cleanly");
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Build the axum router. Public-in-crate so integration tests can
/// exercise the full handler tree against an in-memory store without
/// binding a TCP port.
///
/// The `allowed_hosts` allowlist is wired through a middleware that
/// rejects DNS-rebinding attacks (see [`enforce_host_allowlist`]).
///
/// `auth` is the per-launch token (#1096). When `Some(_)`, every route
/// requires the token via header / cookie / query param (see
/// [`auth::enforce_auth`]). When `None`, the auth layer is omitted —
/// callers using this mode are responsible for emitting a back-compat
/// warning to the operator.
pub(crate) fn build_router(
    state: AppState,
    allowed_hosts: AllowedHosts,
    auth: Option<AuthToken>,
) -> Router {
    let mut app = Router::new()
        .route("/health", get(handlers::health))
        .route("/api/stats", get(handlers::stats))
        .route("/api/graph", get(handlers::graph))
        .route("/api/chunk/{id}", get(handlers::chunk_detail))
        .route("/api/hierarchy/{id}", get(handlers::hierarchy))
        .route("/api/embed/2d", get(handlers::cluster_2d))
        .route("/api/search", get(handlers::search))
        .route("/", get(assets::index_html))
        .route("/static/{*path}", get(assets::static_asset))
        .with_state(state);

    // #1096 SEC-7: per-launch auth. Sits inside the host-header
    // allowlist (rejected hosts skip auth — saves a constant-time
    // compare on a request we'd reject anyway) and outside the
    // compression/trace layers (so 401 responses are still gzipped
    // and traced).
    if let Some(token) = auth {
        app = app.layer(from_fn_with_state(token, auth::enforce_auth));
    }

    app
        // SEC-1: Host-header allowlist closes the DNS-rebinding class.
        // Must sit inside the compression layer so rejections skip the
        // gzip round-trip.
        .layer(from_fn_with_state(allowed_hosts, enforce_host_allowlist))
        // P1.14 / SEC: cap request bodies. Every route is GET; legitimate
        // clients never send a body. 64 KiB is plenty for query strings
        // and cookies (which travel in headers, not body); axum rejects
        // bodies larger than this with 413 Payload Too Large before
        // allocating. Layer sits *outside* auth/host-allowlist so the
        // limit applies even to rejected requests (preventing OOM-then-401
        // attacks) but *inside* compression so 413 responses are gzipped.
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 * 1024))
        // Gzip every response axum sends. The graph + cluster JSON
        // payloads compress ~5-10× (1-2 MB → 150-300 KB on the cqs
        // corpus); vendor JS bundles compress ~3×. Negligible CPU on
        // the server side, big win on parse/transfer time at the browser.
        .layer(CompressionLayer::new())
        // OB-V1.29-5: TraceLayer emits a span per request plus
        // on-response events with latency + status. Handlers already
        // log entry via `tracing::info!`; this layer closes the loop
        // by logging completion, giving per-endpoint latency in the
        // journal without hand-wrapping every handler body.
        //
        // P1.11 / SEC: customise MakeSpan to record path only, NOT the
        // full URI — the `?token=…` query param lands in span fields
        // otherwise and bleeds the per-launch token into journald /
        // RUST_LOG=debug.
        .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
            tracing::info_span!(
                "http_request",
                method = %req.method(),
                path = %req.uri().path(),
            )
        }))
}

/// Build the allowed-`Host` set for DNS-rebinding protection.
///
/// Accepts:
/// - `localhost`, `127.0.0.1`, `[::1]` (bare, and with the bound port)
/// - The exact `host:port` the server is bound to (e.g. `192.168.1.5:8080`)
/// - The bind IP on its own, so an explicit LAN bind still answers when
///   the client sends the naked IP as `Host:`
///
/// Any other `Host` value is refused by [`enforce_host_allowlist`].
pub(crate) fn allowed_host_set(bind_addr: &SocketAddr) -> AllowedHosts {
    let port = bind_addr.port();
    let mut set = HashSet::new();
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        set.insert(host.to_string());
        set.insert(format!("{host}:{port}"));
    }
    // SocketAddr::to_string wraps IPv6 in brackets automatically.
    set.insert(bind_addr.to_string());
    set.insert(bind_addr.ip().to_string());
    Arc::new(set)
}

/// axum middleware: reject requests whose `Host` header isn't on the
/// allowlist.
///
/// SEC-1 (DNS-rebinding). An attacker page at `evil.example.com` with
/// a TTL-0 DNS record pointing at `127.0.0.1` can make the victim's
/// browser fetch `http://evil.example.com:8080/api/chunk/<id>` and
/// same-origin it to the running cqs serve. The browser *sends* the
/// attacker hostname in the `Host:` header, so rejecting unknown hosts
/// closes the class.
///
/// Reject requests with no `Host:` header. HTTP/1.1 requires one; HTTP/1.0
/// does not, but a no-Host request bypasses DNS-rebinding protection (the
/// allowlist has nothing to compare against) so we treat it as malformed.
/// Tests must build requests with a Host header (see `src/serve/tests.rs`
/// fixtures). (P1.12.)
async fn enforce_host_allowlist(
    State(allowed): State<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    match req.headers().get(header::HOST) {
        None => {
            tracing::warn!("serve: rejected request with missing Host header");
            Err((StatusCode::BAD_REQUEST, "missing Host header"))
        }
        Some(value) => {
            let host = value.to_str().unwrap_or("");
            if allowed.contains(host) {
                Ok(next.run(req).await)
            } else {
                tracing::warn!(host = %host, "serve: rejected request with disallowed Host header");
                Err((StatusCode::BAD_REQUEST, "disallowed Host header"))
            }
        }
    }
}

/// Listen for Ctrl-C or SIGTERM (Unix) to trigger axum's graceful
/// shutdown. Without SIGTERM handling, `systemctl stop` and `launchd`
/// shutdowns escalate to SIGKILL with no graceful drain. (P1.19.)
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "failed to install ctrl-c handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
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

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("ctrl-c received, beginning graceful shutdown"),
        _ = terminate => tracing::info!("SIGTERM received, beginning graceful shutdown"),
    }
}
