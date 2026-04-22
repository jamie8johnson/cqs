//! `cqs serve` — interactive web UI for exploring the cqs index.
//!
//! Thin CLI wrapper around `cqs::serve::run_server`. Resolves the
//! project's read-only store and binds the requested address.

use std::net::SocketAddr;

use anyhow::{Context, Result};

use crate::cli::find_project_root;

/// Entry point for `cqs serve`. Bumped from the dispatch in
/// `src/cli/dispatch.rs`.
///
/// # Arguments
/// * `port` — TCP port (default 8080)
/// * `bind` — bind address (default `127.0.0.1`); anything else exposes
///   the un-authenticated server beyond localhost
/// * `open` — open the system browser on start
pub(crate) fn cmd_serve(port: u16, bind: String, open: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_serve", port, bind = %bind, open).entered();

    if bind != "127.0.0.1" && bind != "localhost" && bind != "::1" {
        tracing::warn!(
            bind = %bind,
            "binding cqs serve to non-localhost — there is no auth, anyone with network \
             access to this address can read the index"
        );
        eprintln!("WARN: --bind {bind} exposes cqs serve beyond localhost; there is no auth");
    }

    let bind_addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("Failed to parse {bind}:{port} as a SocketAddr"))?;

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    if !index_path.exists() {
        anyhow::bail!(
            "No cqs index found at {}. Run `cqs init` and `cqs index` first.",
            index_path.display()
        );
    }

    let store = cqs::Store::open_readonly(&index_path)
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    if open {
        let url = format!("http://{bind_addr}");
        if let Err(e) = open_browser(&url) {
            tracing::warn!(url = %url, error = %e, "failed to open browser");
            eprintln!("WARN: --open requested but failed to launch browser: {e}");
            eprintln!("       open {url} manually");
        }
    }

    cqs::serve::run_server(store, bind_addr, false)
}

/// Best-effort browser launch. Falls through cleanly on failure —
/// the server still starts and the user can open the URL manually.
fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer.exe";

    std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {cmd} {url}"))?;
    Ok(())
}
