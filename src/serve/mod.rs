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

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{routing::get, Router};
use tokio::net::TcpListener;

use crate::store::{ReadOnly, Store};

mod assets;
mod data;
mod error;
mod handlers;

#[cfg(test)]
mod tests;

pub use error::ServeError;

/// Shared state passed to every axum handler. Wraps a read-only store
/// behind an `Arc` so the handler tree can read concurrently.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<Store<ReadOnly>>,
}

/// Run the `cqs serve` HTTP server.
///
/// Binds to `bind_addr` (default `127.0.0.1:8080`), serves the embedded
/// HTML shell at `/`, and answers JSON queries against `store` for
/// `/api/graph`, `/api/chunk/:id`, `/api/search`, `/api/stats`.
///
/// Returns when the listener fails or the process is interrupted.
/// `quiet` suppresses the "listening on" stdout banner so test code
/// can run the server without polluting test output.
pub fn run_server(store: Store<ReadOnly>, bind_addr: SocketAddr, quiet: bool) -> Result<()> {
    let _span = tracing::info_span!("serve", addr = %bind_addr).entered();

    let state = AppState {
        store: Arc::new(store),
    };
    let app = build_router(state);

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
            println!("cqs serve listening on http://{actual}");
            println!("press Ctrl-C to stop");
        }
        tracing::info!(addr = %actual, "cqs serve started");

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
pub(crate) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .route("/api/stats", get(handlers::stats))
        .route("/api/graph", get(handlers::graph))
        .route("/api/chunk/{id}", get(handlers::chunk_detail))
        .route("/api/search", get(handlers::search))
        .route("/", get(assets::index_html))
        .route("/static/{*path}", get(assets::static_asset))
        .with_state(state)
}

/// Listen for Ctrl-C to trigger axum's graceful shutdown.
async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %e, "failed to install ctrl-c handler; server will only stop on listener failure");
        std::future::pending::<()>().await;
    }
    tracing::info!("ctrl-c received, beginning graceful shutdown");
}
