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
//! - Per-launch 256-bit auth token gates every request; 3 credential channels
//!   (Bearer / cookie / `?token=` query); `--no-auth` requires a
//!   `NoAuthAcknowledgement` proof token. No WebSocket, no live updates —
//!   single-user local exploration
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
    response::{IntoResponse, Response},
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

pub use auth::{AuthMode, AuthToken, InvalidTokenAlphabet, NoAuthAcknowledgement};
pub use error::ServeError;

/// Shared state passed to every axum handler. Wraps a read-only store
/// behind an `Arc` so the handler tree can read concurrently.
///
/// The `blocking_permits` semaphore caps how many handlers may hold a
/// `spawn_blocking` slot at once. Without it, axum's default
/// runtime allows up to 512 blocking threads — a single hostile (or
/// pathological) client can fan out 512 graph queries and pin ~5 GB
/// of working set across SQLite per-connection scratch buffers.
/// Default 32 permits is plenty for an interactive single-user UI;
/// `CQS_SERVE_BLOCKING_PERMITS` overrides per-launch (clamped 1..1024).
///
/// `last_request_epoch` is an epoch-seconds timestamp touched by every
/// incoming request via [`touch_idle_clock`]. The
/// idle-eviction future in `run_server` polls it; when the gap exceeds
/// `CQS_SERVE_IDLE_MINUTES`, the server shuts down gracefully so the
/// `Store<ReadOnly>` mmap and tokio runtime release. `0` disables.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<Store<ReadOnly>>,
    pub(crate) blocking_permits: Arc<tokio::sync::Semaphore>,
    pub(crate) last_request_epoch: Arc<std::sync::atomic::AtomicU64>,
}

/// Allowed `Host` header values, built at router-build time from the
/// bind address. Shared via `Arc` so the middleware closure is cheap to
/// clone per-request.
///
/// An empty set means "wildcard bind — accept any Host". This
/// short-circuits the DNS-rebinding allowlist when `--bind 0.0.0.0`,
/// because the listening socket has no idea which interface IP a
/// legitimate LAN browser will dial. Without this carve-out, every
/// LAN client gets `400 disallowed Host header` and operators are
/// pushed to `--no-auth`. The per-launch token remains the primary
/// defence in this mode.
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
/// `auth` is the per-launch token wrapped in [`AuthMode`].
/// Pass [`AuthMode::Required`] to enforce the token on every route via
/// [`auth::enforce_auth`]; pass [`AuthMode::Disabled`] (which requires
/// a [`NoAuthAcknowledgement`] proof token) for the unauthenticated mode.
/// The CLI calls this with `Disabled` when
/// `--no-auth` is set, after emitting a loud-warning banner. The
/// caller is responsible for emitting the token in the "listening on"
/// banner, since `quiet=true` callers (tests) construct their own URL.
pub fn run_server(
    store: Store<ReadOnly>,
    bind_addr: SocketAddr,
    quiet: bool,
    auth: AuthMode,
) -> Result<()> {
    let _span = tracing::info_span!("serve", addr = %bind_addr).entered();

    // Bound concurrent `spawn_blocking` jobs across all handlers. See
    // `AppState` doc comment.
    let permits = crate::limits::serve_blocking_permits();
    tracing::info!(permits, "serve: spawn_blocking semaphore initialised");
    // Prime the idle clock to "now" so a startup that immediately backgrounds
    // doesn't fire eviction before the first request arrives.
    let last_request_epoch = Arc::new(std::sync::atomic::AtomicU64::new(now_epoch_secs()));
    let state = AppState {
        store: Arc::new(store),
        blocking_permits: Arc::new(tokio::sync::Semaphore::new(permits)),
        last_request_epoch: Arc::clone(&last_request_epoch),
    };
    let allowed_hosts = allowed_host_set(&bind_addr);
    let idle_minutes = crate::limits::serve_idle_minutes();
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
            // When auth is enabled, emit the paste-ready URL (token + bind
            // addr) so a fresh launch is one click away from being usable —
            // but ONLY when stdout is an interactive terminal. The
            // per-launch token is generated fresh on every start
            // (`AuthToken::random`) and is never persisted; it lives only in
            // this process's memory. Printing it into a non-TTY stdout means
            // it lands in journald / container log drivers (30-day
            // retention), turning a session secret into a logged one.
            //
            // Non-TTY: print the URL WITHOUT the token plus a hint that the
            // token is per-launch and only surfaced on an interactive
            // terminal — so an operator running headless knows to launch in a
            // terminal (or pass `--no-auth` for unattended use).
            let stdout_is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
            match auth.token() {
                Some(token) => {
                    if stdout_is_tty {
                        println!("{}", auth_banner_tty(actual, token.as_str()));
                    } else {
                        for line in auth_banner_non_tty(actual) {
                            println!("{line}");
                        }
                    }
                }
                None => {
                    println!("{}", no_auth_banner_line(actual));
                    eprintln!(
                        "WARN: --no-auth in use — anyone with network access to {actual} \
                         can read this index"
                    );
                }
            }
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, auth_enabled = auth.token().is_some(), "cqs serve started");

        // Race the SIGINT/SIGTERM signal future against an idle-eviction
        // future. With `idle_minutes == 0` the idle future is
        // `pending::<()>()` and the server only exits on signal.
        axum::serve(listener, app)
            .with_graceful_shutdown(idle_or_signal(last_request_epoch, idle_minutes))
            .await
            .context("axum server failed")?;

        tracing::info!("cqs serve shut down cleanly");
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// URL to show in banners and hand to the `--open` browser launch.
///
/// Wildcard binds (`0.0.0.0`, `[::]`) are valid listen addresses but
/// useless connect targets — browsers reject `http://0.0.0.0:8080`.
/// Map them to the matching loopback (`127.0.0.1` / `[::1]`) for the
/// displayed/launched URL only; the bind itself is unchanged. Concrete
/// addresses pass through untouched.
pub fn loopback_open_url(bind_addr: SocketAddr) -> String {
    let mut display = bind_addr;
    if display.ip().is_unspecified() {
        let loopback = match display.ip() {
            std::net::IpAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            std::net::IpAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        };
        display.set_ip(loopback);
    }
    format!("http://{display}")
}

/// The no-auth "listening on" banner line. Uses [`loopback_open_url`] so
/// a wildcard bind still prints a URL a browser will accept.
fn no_auth_banner_line(actual: SocketAddr) -> String {
    format!("cqs serve listening on {}", loopback_open_url(actual))
}

/// Interactive (TTY-stdout) "listening on" banner: the paste-ready URL
/// with the per-launch token embedded. Only used when stdout is a
/// terminal — see the call site for why the token is withheld otherwise.
fn auth_banner_tty(actual: SocketAddr, token: &str) -> String {
    format!("cqs serve listening on http://{actual}/?token={token}")
}

/// Non-interactive (non-TTY stdout) "listening on" banner: the URL
/// WITHOUT the token, plus a hint explaining where the token comes from.
///
/// The token is generated fresh per launch (`AuthToken::random`) and is
/// never persisted — it exists only in process memory and is surfaced
/// solely on an interactive terminal. A headless start therefore can't
/// recover it from logs (by design: that's the leak this avoids), so the
/// hint points at the two recovery paths: relaunch attached to a
/// terminal, or use `--no-auth` for unattended access.
fn auth_banner_non_tty(actual: SocketAddr) -> Vec<String> {
    vec![
        format!("cqs serve listening on http://{actual}/"),
        "(auth token withheld: stdout is not a terminal, so the token is not printed here to \
         avoid persisting it into journald/container logs)"
            .to_string(),
        "(the per-launch token is generated fresh each start and only shown on an interactive \
         terminal; relaunch in a terminal to see it, or pass --no-auth for unattended use)"
            .to_string(),
    ]
}

/// Race a signal-driven shutdown against an idle-driven shutdown. With
/// `idle_minutes == 0` the idle arm is `pending::<()>()` so only signals
/// can resolve the future. Otherwise the server shuts down on whichever
/// fires first.
async fn idle_or_signal(last_request_epoch: Arc<std::sync::atomic::AtomicU64>, idle_minutes: u64) {
    if idle_minutes == 0 {
        tracing::info!("serve: idle eviction disabled (CQS_SERVE_IDLE_MINUTES=0)");
        shutdown_signal().await;
        return;
    }
    tracing::info!(
        idle_minutes,
        "serve: idle eviction armed; server will exit after this many minutes of no requests"
    );
    // 1-minute resolution is plenty for a 30-minute idle window. Polling
    // faster gives no operator-visible benefit and only burns wakeups.
    let poll = std::time::Duration::from_secs(60);
    tokio::select! {
        _ = shutdown_signal() => {}
        _ = wait_for_idle(last_request_epoch, idle_minutes * 60, poll) => {
            tracing::info!(idle_minutes, "serve: idle threshold reached, beginning graceful shutdown");
        }
    }
}

/// Poll `last_request_epoch` at `poll` cadence. Returns when the gap
/// between "now" (epoch seconds) and the most recent touch exceeds
/// `idle_secs`. `poll` is parameterized so tests can drive the loop on
/// millisecond cadence; production passes 60 s.
pub(crate) async fn wait_for_idle(
    last_request_epoch: Arc<std::sync::atomic::AtomicU64>,
    idle_secs: u64,
    poll: std::time::Duration,
) {
    use std::sync::atomic::Ordering;
    let mut tick = tokio::time::interval(poll);
    // Skip the first immediate-fire tick; the first poll happens after
    // one full poll interval.
    tick.tick().await;
    loop {
        tick.tick().await;
        let now = now_epoch_secs();
        let last = last_request_epoch.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= idle_secs {
            return;
        }
    }
}

/// Wall-clock epoch seconds. Saturates at 0 if the system clock is set
/// before the unix epoch (effectively never on a real host, but the
/// `SystemTimeError` path costs nothing to handle).
pub(crate) fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Middleware that updates `AppState.last_request_epoch` on every request.
/// Sits outside auth so the idle clock advances even on unauthenticated
/// pings — the threat model is "user walked away," not "adversary keeps
/// the server alive with 401s." Keepalive cost is negligible: an attacker
/// who can connect already pays the request budget elsewhere (host
/// allowlist, body limit, blocking_permits semaphore).
async fn touch_idle_clock(State(state): State<AppState>, request: Request, next: Next) -> Response {
    state
        .last_request_epoch
        .store(now_epoch_secs(), std::sync::atomic::Ordering::Relaxed);
    next.run(request).await
}

/// Build the axum router. Public-in-crate so integration tests can
/// exercise the full handler tree against an in-memory store without
/// binding a TCP port.
///
/// The `allowed_hosts` allowlist is wired through a middleware that
/// rejects DNS-rebinding attacks (see [`enforce_host_allowlist`]).
///
/// `auth` is the per-launch token wrapped in [`AuthMode`].
/// On [`AuthMode::Required`], every route requires the token via
/// header / cookie / query param (see [`auth::enforce_auth`]). On
/// [`AuthMode::Disabled`], the auth layer is omitted — the
/// [`NoAuthAcknowledgement`] proof token inside that variant
/// guarantees an explicit opt-in.
/// Concurrent in-flight requests are capped pre-everything.
/// `try_acquire` returns immediately — saturation is `503 Service
/// Unavailable`, never a queued allocation. Sits as the outermost
/// middleware so it gates EVERY downstream layer (auth, host allowlist,
/// body limit, compression, trace) — no buffer is allocated, no auth
/// constant-time compare is run, until the request holds a permit.
async fn enforce_concurrency_cap(
    State(sem): State<Arc<tokio::sync::Semaphore>>,
    req: Request,
    next: Next,
) -> Response {
    let permit = match sem.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("serve: concurrent-request cap saturated — returning 503 (SEC-V1.36-9)");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Server at concurrency cap; retry shortly",
            )
                .into_response();
        }
    };
    let response = next.run(req).await;
    drop(permit);
    response
}

pub(crate) fn build_router(state: AppState, allowed_hosts: AllowedHosts, auth: AuthMode) -> Router {
    let touch_state = state.clone();
    let conn_sem = Arc::new(tokio::sync::Semaphore::new(
        crate::limits::serve_max_concurrent_requests(),
    ));
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

    // Touch the idle clock on every request. Sits inside auth (after this
    // layer order is finalized below) so even unauthenticated pings count as
    // activity — the threat model is "user walked away," not "adversary keeps
    // the server alive."
    app = app.layer(from_fn_with_state(touch_state, touch_idle_clock));

    // Per-launch auth. Sits inside the host-header allowlist (rejected hosts
    // skip auth — saves a constant-time compare on a request we'd reject
    // anyway) and outside the compression/trace layers (so 401 responses are
    // still gzipped and traced).
    match auth {
        AuthMode::Required { token, cookie_port } => {
            // `new()` pre-builds the cookie name and lookup needle once so the
            // per-request middleware path doesn't allocate.
            let middleware_state = auth::AuthMiddlewareState::new(token, cookie_port);
            app = app.layer(from_fn_with_state(middleware_state, auth::enforce_auth));
        }
        AuthMode::Disabled(_ack) => {
            // The proof token has been consumed; auth layer omitted by
            // explicit construction. Surface a structured
            // log line at error level so a misconfigured caller is
            // visible regardless of `quiet` (the eprintln banner is
            // gated on `quiet=false`).
            tracing::error!("cqs serve: AuthMode::Disabled — no per-launch token enforced");
        }
    }

    app
        // Host-header allowlist closes the DNS-rebinding class. Must sit
        // inside the compression layer so rejections skip the gzip round-trip.
        .layer(from_fn_with_state(allowed_hosts, enforce_host_allowlist))
        // Cap request bodies. Every route is GET; legitimate
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
        // TraceLayer emits a span per request plus on-response events with
        // latency + status. Handlers already log entry via `tracing::info!`;
        // this layer closes the loop by logging completion, giving
        // per-endpoint latency in the journal without hand-wrapping every
        // handler body.
        //
        // MakeSpan records path only, NOT the full URI — the `?token=…` query
        // param lands in span fields otherwise and bleeds the per-launch token
        // into journald / RUST_LOG=debug.
        //
        // Generate a per-process monotonic `request_id` so concurrent requests
        // for the same path can be correlated in
        // `journalctl --user -u cqs-watch` output. `AtomicU64` counter (no
        // extra dep) — sufficient for one daemon's journal; not globally
        // unique by design.
        .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
            use std::sync::atomic::{AtomicU64, Ordering};
            static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);
            let request_id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            tracing::info_span!(
                "http_request",
                method = %req.method(),
                path = %req.uri().path(),
                request_id,
            )
        }))
        // Outermost middleware. Caps concurrent in-flight requests so an
        // attacker on `--bind 0.0.0.0` can't
        // fan out N connections each holding a 64 KiB pre-auth body
        // buffer (bound only by FD limit otherwise). `try_acquire`
        // makes saturation return 503 immediately — no queueing, no
        // allocation past the permit check. Sits OUTSIDE every other
        // layer (auth, host, body-limit, compression, trace) so the
        // body buffer is never allocated for over-cap requests.
        .layer(from_fn_with_state(conn_sem, enforce_concurrency_cap))
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
///
/// When `bind_addr.ip().is_unspecified()` (i.e. `0.0.0.0` or
/// `[::]`), return an *empty* set. The listening socket can't enumerate
/// which interface IP a legitimate LAN client will use, so any concrete
/// allowlist is wrong. `enforce_host_allowlist` interprets the empty
/// set as "allow any Host" and emits a one-shot startup warning (the
/// per-launch token still gates access). Operators who want the
/// allowlist back can bind to a specific IP.
pub(crate) fn allowed_host_set(bind_addr: &SocketAddr) -> AllowedHosts {
    if bind_addr.ip().is_unspecified() {
        // Empty allowlist = "Host: anything goes". Auth token still
        // checked downstream. See module-level note above.
        tracing::warn!(
            bind = %bind_addr,
            "wildcard bind: DNS-rebinding Host-header allowlist disabled because we can't \
             enumerate LAN interface IPs without an extra dep. Per-launch auth token remains \
             the primary defence — bind to an explicit IP if you need the allowlist back."
        );
        return Arc::new(HashSet::new());
    }
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
/// Closes the DNS-rebinding class. An attacker page at `evil.example.com` with
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
/// fixtures).
async fn enforce_host_allowlist(
    State(allowed): State<AllowedHosts>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    // Empty allowlist = wildcard bind, accept any Host.
    // `allowed_host_set` emits the startup warning; per-launch auth
    // token remains the primary defence in this mode.
    if allowed.is_empty() {
        return Ok(next.run(req).await);
    }
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
/// shutdowns escalate to SIGKILL with no graceful drain.
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
