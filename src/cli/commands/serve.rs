//! `cqs serve` — interactive web UI for exploring the cqs index.
//!
//! Thin CLI wrapper around `cqs::serve::run_server`. Resolves the
//! project's read-only store and binds the requested address.

use std::net::SocketAddr;

use anyhow::{Context, Result};

use crate::cli::find_project_root;

/// Entry point for `cqs serve`. Dispatched from `src/cli/dispatch.rs`.
///
/// # Arguments
/// * `port` — TCP port (default 8080)
/// * `bind` — bind address (default `127.0.0.1`)
/// * `open` — open the system browser on start (token-aware URL)
/// * `no_auth` — disable per-launch auth (#1096); back-compat opt-out
///   for scripted automation, with loud-warning banner on boot
///
/// CQ-V1.30.1-6: the CLI-side "non-loopback + --no-auth" warning was
/// removed; `serve/mod.rs::run_server` already emits an unconditional
/// `WARN: --no-auth in use` on the listening banner. Carrying both
/// surfaces was redundant and the CLI-side variant masked the most
/// common footgun (localhost + --no-auth) by staying silent.
pub(crate) fn cmd_serve(port: u16, bind: String, open: bool, no_auth: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_serve", port, bind = %bind, open, no_auth).entered();

    // CQ-V1.30.1-6: CLI-side `--no-auth` warning dropped — `serve/mod.rs::run_server`
    // emits an unconditional `WARN: --no-auth in use` on the listening banner, plus
    // a structured `tracing::error!` regardless of `quiet`, plus loud-warn for the
    // non-loopback case (#1118 / SEC-7 + PB-V1.30.1-1 in #1206). Carrying both
    // surfaces was redundant.

    // PB-V1.30.1-2: resolve "localhost" to 127.0.0.1 before parse, since
    // `SocketAddr::parse` only accepts numeric IPs. CLI docs treat "localhost"
    // as a valid bind value; without this resolution the literal hostname
    // would always fail.
    let bind_str: &str = if bind == "localhost" {
        "127.0.0.1"
    } else {
        bind.as_str()
    };
    let bind_addr: SocketAddr = format!("{bind_str}:{port}")
        .parse()
        .with_context(|| format!("Failed to parse {bind_str}:{port} as a SocketAddr"))?;

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
        // SEC-V1.33-1 (#1337): with auth on, the token-bearing URL would
        // land in the spawned browser-launcher's argv (`xdg-open`,
        // `cmd /C start`, `open`), readable by any local user via
        // `/proc/<pid>/cmdline` / `wmic process get CommandLine` and
        // typically captured by audit subsystems (auditd, ETW). Banner
        // routing already pulled the token off journald/stdout for the
        // same threat model; spawning a subprocess re-leaks it through
        // a parallel surface. Min-viable mitigation: skip the launch
        // when the URL would carry a token, and tell the user to paste
        // the banner URL manually. With `--no-auth` the URL is bare,
        // nothing to leak — proceed normally.
        match auth.token() {
            Some(_) => {
                eprintln!(
                    "NOTE: --open suppressed under auth (token in argv would be readable\n       \
                     to other local users via /proc/<pid>/cmdline). Paste the URL\n       \
                     printed in the listening banner into your browser instead."
                );
            }
            None => {
                let url = format!("http://{bind_addr}");
                if let Err(e) = open_browser(&url) {
                    tracing::warn!(error = %e, "failed to open browser");
                    eprintln!("WARN: --open requested but failed to launch browser: {e}");
                    eprintln!("       open the URL printed in the listening banner manually");
                }
            }
        }
    }

    cqs::serve::run_server(store, bind_addr, false, auth)
}

/// Reject URLs containing shell metacharacters before handing them to
/// cmd.exe. SEC-V1.36-3 / P3: cmd.exe re-parses `&|>%^()<` even inside
/// double quotes for pipe / redirect operators after expansion, so a
/// hostile bind addr (`bind = "127.0.0.1:8080/&calc&"`) could spawn an
/// extra command. The token alphabet is alnum-only, but `bind` accepts
/// arbitrary user input. Reject up front rather than rely on cmd.exe's
/// quoting heuristics.
fn url_safe_for_cmd(url: &str) -> bool {
    !url.chars()
        .any(|c| matches!(c, '&' | '|' | '^' | '<' | '>' | '%' | '(' | ')'))
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
        if !url_safe_for_cmd(url) {
            anyhow::bail!(
                "Refusing to open URL via cmd.exe — contains shell metacharacters. \
                 Open manually or fix the bind address."
            );
        }
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn cmd /C start \"\" {url}"))?;
        return Ok(());
    }

    // PB-V1.30.1-4 / #1224: under WSL, the Linux side has no default
    // browser — `xdg-open` either fails outright or pops a "no
    // application registered" dialog. The user expects the URL to
    // open in their Windows-side default browser the way every
    // other WSL-aware tool does it. Hand off to `cmd.exe /C start
    // "" "<url>"` via the WSL interop so the browser launch goes
    // through the same Win32 protocol-handler path as the native
    // Windows branch above.
    #[cfg(target_os = "linux")]
    {
        if cqs::config::is_wsl() {
            if !url_safe_for_cmd(url) {
                anyhow::bail!(
                    "Refusing to open URL via cmd.exe (WSL interop) — \
                     contains shell metacharacters. Open manually or fix the bind address."
                );
            }
            std::process::Command::new("cmd.exe")
                .args(["/C", "start", "", url])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .with_context(|| {
                    format!("Failed to spawn cmd.exe /C start \"\" {url} (WSL interop)")
                })?;
            return Ok(());
        }
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
