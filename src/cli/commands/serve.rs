//! `cqs serve` — interactive web UI for exploring the cqs index.
//!
//! Thin CLI wrapper around `cqs::serve::run_server`. Resolves the
//! project's read-only store and binds the requested address.

use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, Result};

use crate::cli::find_project_root;

/// Decide whether `cqs serve --no-auth --bind <bind>` should emit the
/// "non-loopback exposure" warning.
///
/// PB-V1.30.1-1: returns `true` when `bind` resolves to anything that
/// is NOT a loopback address. Subsumes `0.0.0.0` and `::`
/// (IPv4/IPv6 UNSPECIFIED — the most exposed bind targets), concrete
/// LAN IPs, and hostnames that don't loop back. Parse-failure
/// (e.g. "localhost") falls through to the explicit name check so the
/// previous behavior on the literal hostname is preserved.
pub(crate) fn serve_warn_no_auth_exposure(bind: &str) -> bool {
    let is_loopback = match bind.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => matches!(bind, "localhost"),
    };
    !is_loopback
}

/// Entry point for `cqs serve`. Dispatched from `src/cli/dispatch.rs`.
///
/// # Arguments
/// * `port` — TCP port (default 8080)
/// * `bind` — bind address (default `127.0.0.1`)
/// * `open` — open the system browser on start (token-aware URL)
/// * `no_auth` — disable per-launch auth (#1096); back-compat opt-out
///   for scripted automation, with loud-warning banner on boot
pub(crate) fn cmd_serve(port: u16, bind: String, open: bool, no_auth: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_serve", port, bind = %bind, open, no_auth).entered();

    // #1096 + PB-V1.30.1-1: warn loudly only when --no-auth is paired
    // with a non-loopback bind. With auth on, non-loopback binds are
    // fine — every request is gated by the token. The legacy
    // unconditional warning misled operators into thinking any LAN
    // bind was insecure. The substring check it replaced silently
    // accepted `0.0.0.0` and `::` (UNSPECIFIED — the most exposed
    // bind targets of all); `serve_warn_no_auth_exposure` parses the
    // bind once with `IpAddr::is_loopback()` so wildcard binds now
    // trigger the warning explicitly.
    if no_auth && serve_warn_no_auth_exposure(&bind) {
        tracing::warn!(
            bind = %bind,
            "binding cqs serve to non-localhost without auth — anyone with network \
             access to this address can read the index"
        );
        eprintln!(
            "WARN: --bind {bind} with --no-auth exposes cqs serve beyond localhost \
             with no authentication"
        );
    }

    let bind_addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("Failed to parse {bind}:{port} as a SocketAddr"))?;

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs::resolve_index_db(&cqs_dir);

    if !index_path.exists() {
        anyhow::bail!(
            "No cqs index found at {}. Run `cqs init` and `cqs index` first.",
            index_path.display()
        );
    }

    let store = cqs::Store::open_readonly(&index_path)
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    // #1096: generate a per-launch token unless explicitly opted out.
    // The token is shared with `run_server` (via `AuthMode::Required`)
    // and with the browser-open URL below — both branches need to
    // agree on the same value, so we build the AuthMode once and
    // borrow the token out for the URL.
    //
    // #1135: cookie_port = bind_addr.port() so two `cqs serve`
    // instances on the same host don't collide in the browser
    // cookie jar.
    //
    // #1136: AuthMode::Disabled requires `NoAuthAcknowledgement`,
    // which is constructed via `from_cli_no_auth_flag()` — the
    // function name is the audit trail for "user explicitly opted
    // into a no-auth server."
    let auth = if no_auth {
        cqs::serve::AuthMode::disabled(cqs::serve::NoAuthAcknowledgement::from_cli_no_auth_flag())
    } else {
        cqs::serve::AuthMode::required(cqs::serve::AuthToken::random(), bind_addr.port())
    };

    if open {
        // The launched URL embeds the token as a query parameter; the
        // post-auth redirect strips it from the address bar and hands
        // it off to a `cqs_token_<port>` cookie, so reload + bookmark
        // stay working without leaving the token visible. With
        // --no-auth the URL is the bare bind addr.
        let url = match auth.token() {
            Some(token) => format!("http://{bind_addr}/?token={}", token.as_str()),
            None => format!("http://{bind_addr}"),
        };
        if let Err(e) = open_browser(&url) {
            tracing::warn!(error = %e, "failed to open browser");
            eprintln!("WARN: --open requested but failed to launch browser: {e}");
            eprintln!("       open the URL printed in the listening banner manually");
        }
    }

    cqs::serve::run_server(store, bind_addr, false, auth)
}

/// Best-effort browser launch. Falls through cleanly on failure —
/// the server still starts and the user can open the URL manually.
fn open_browser(url: &str) -> Result<()> {
    // P2.55: on Windows, `explorer.exe <url>` doesn't reliably navigate and
    // can strip query strings (the `?token=…` we depend on for auth, since
    // the serve banner mints a per-launch token). `cmd /C start "" "<url>"`
    // hands the URL to the user's default browser through the documented
    // Win32 protocol-handler path. The empty `""` is required because
    // `start`'s first quoted arg is interpreted as the window title.
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn cmd /C start \"\" {url}"))?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        std::process::Command::new(cmd)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn {cmd} {url}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // PB-V1.30.1-1: loopback IPv4 — no warning.
    #[test]
    fn no_warn_on_ipv4_loopback() {
        assert!(!serve_warn_no_auth_exposure("127.0.0.1"));
    }

    // PB-V1.30.1-1: arbitrary loopback IPv4 (127.0.0.0/8) — no warning.
    #[test]
    fn no_warn_on_ipv4_loopback_range() {
        assert!(!serve_warn_no_auth_exposure("127.1.2.3"));
    }

    // PB-V1.30.1-1: loopback IPv6 — no warning.
    #[test]
    fn no_warn_on_ipv6_loopback() {
        assert!(!serve_warn_no_auth_exposure("::1"));
    }

    // PB-V1.30.1-1: literal hostname "localhost" — no warning.
    // Resolves to 127.0.0.1 in practice, parse falls through to the
    // explicit name check so we preserve the legacy behavior.
    #[test]
    fn no_warn_on_localhost_name() {
        assert!(!serve_warn_no_auth_exposure("localhost"));
    }

    // PB-V1.30.1-1 (regression): IPv4 wildcard (0.0.0.0) — warning fires.
    // The legacy substring check silently accepted this string.
    #[test]
    fn warn_on_ipv4_wildcard() {
        assert!(serve_warn_no_auth_exposure("0.0.0.0"));
    }

    // PB-V1.30.1-1 (regression): IPv6 unspecified (::) — warning fires.
    #[test]
    fn warn_on_ipv6_unspecified_short() {
        assert!(serve_warn_no_auth_exposure("::"));
    }

    // PB-V1.30.1-1 (regression): IPv6 unspecified (::0) — warning fires.
    #[test]
    fn warn_on_ipv6_unspecified_explicit() {
        assert!(serve_warn_no_auth_exposure("::0"));
    }

    // PB-V1.30.1-1: concrete LAN IP — warning fires (existing behavior).
    #[test]
    fn warn_on_lan_ip() {
        assert!(serve_warn_no_auth_exposure("192.168.1.5"));
    }

    // PB-V1.30.1-1: arbitrary hostname that's not "localhost" —
    // warning fires (we can't resolve at warn-time, conservative is fail-closed).
    #[test]
    fn warn_on_arbitrary_hostname() {
        assert!(serve_warn_no_auth_exposure("server.lan"));
    }

    // PB-V1.30.1-1: empty bind — falls through, warns. Defensive
    // behavior: an empty string can't be a loopback, so don't suppress
    // the warning.
    #[test]
    fn warn_on_empty_bind() {
        assert!(serve_warn_no_auth_exposure(""));
    }
}
