//! Model swap commands — `cqs model { show, list, swap }`.
//!
//! Closes the manual backup-and-swap dance the user hit during the v9-200k
//! experiment. Pre-`cqs model swap`, switching the embedder meant:
//!
//!   1. `mv .cqs/ .cqs.bge-large.bak/`
//!   2. `CQS_EMBEDDING_MODEL=v9-200k cqs index --force`
//!   3. `systemctl --user restart cqs-watch`
//!   4. Hope nothing crashed mid-rebuild — if it did, manual recovery from
//!      `.cqs.bge-large.bak/` was the only way out.
//!
//! `cqs model swap <preset>` automates the whole sequence with restore-on-
//! failure semantics:
//!
//!   1. Validate the preset name.
//!   2. Stop the cqs-watch daemon (Linux best-effort via systemctl).
//!   3. Rename `.cqs/` → `.cqs.<old-shortname>.bak/`.
//!   4. Re-run `cmd_index` with `--force` and the new model.
//!   5. On failure: nuke any partial `.cqs/`, rename the backup back, restart
//!      the daemon, and surface the error.
//!   6. On success: leave the backup in place (user can `rm -rf` after they
//!      verify the new index is healthy) and restart the daemon.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use cqs::embedder::ModelConfig;
use cqs::Store;

use crate::cli::args::IndexArgs;
use crate::cli::commands::index::cmd_index;
use crate::cli::config::find_project_root;
use crate::cli::definitions::Cli;

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// `cqs model` subcommand surface.
#[derive(clap::Subcommand)]
pub(crate) enum ModelCommand {
    /// Show the model recorded in the current index, plus on-disk size.
    Show {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List built-in embedding model presets, marking the current one with `*`.
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Swap the indexed embedder: backup `.cqs/`, reindex with the new
    /// preset, restore on failure.
    Swap {
        /// Preset short name: `bge-large`, `v9-200k`, `e5-base`, ...
        preset: String,
        /// Skip the `.cqs/` backup before reindexing. Faster but unrecoverable
        /// if the reindex fails — only use when you have a separate backup.
        #[arg(long)]
        no_backup: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

/// `cqs model show --json` payload.
#[derive(Debug, Serialize)]
struct ModelShowOutput {
    model: String,
    dim: usize,
    total_chunks: u64,
    index_db_size_bytes: u64,
    hnsw_size_bytes: u64,
    cagra_size_bytes: u64,
}

/// One row of `cqs model list --json`.
#[derive(Debug, Serialize)]
struct ModelListEntry {
    name: String,
    repo: String,
    dim: usize,
    current: bool,
}

/// `cqs model swap --json` payload (success case).
///
/// P2 #73: `chunks_indexed` is `i64` rather than `u64` so we can emit `-1`
/// as a sentinel meaning "swap succeeded but we couldn't read back the
/// post-swap chunk count" (store open failed, or chunk_count failed).
/// `count_error` carries the underlying error string in that case so agents
/// can distinguish "really zero" (`chunks_indexed: 0, count_error: null`)
/// from "couldn't count" (`chunks_indexed: -1, count_error: Some(...)`).
#[derive(Debug, Serialize)]
struct ModelSwapOutput {
    from: String,
    to: String,
    chunks_indexed: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    count_error: Option<String>,
    elapsed_secs: f64,
    backup_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Dispatch entry — fans out to the per-subcommand handler.
pub(crate) fn cmd_model(cli: &Cli, subcmd: &ModelCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_model").entered();
    match subcmd {
        ModelCommand::Show { json } => cmd_model_show(cli.json || *json),
        ModelCommand::List { json } => cmd_model_list(cli.json || *json),
        ModelCommand::Swap {
            preset,
            no_backup,
            json,
        } => cmd_model_swap(cli, preset, *no_backup, cli.json || *json),
    }
}

// ---------------------------------------------------------------------------
// `cqs model show`
// ---------------------------------------------------------------------------

/// Print the model recorded in the index plus on-disk file sizes.
fn cmd_model_show(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_model_show").entered();

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs::resolve_index_db(&cqs_dir);

    if !index_path.exists() {
        // P2.11: route bail through the JSON envelope when --json is set so
        // `cqs --json model show` keeps the canonical {data, error, version}
        // shape instead of dumping anyhow text to stderr.
        let msg = format!(
            "No index at {}. Run `cqs init && cqs index` first.",
            index_path.display()
        );
        if json {
            crate::cli::json_envelope::emit_json_error("no_index", &msg)?;
            std::process::exit(1);
        }
        bail!("{msg}");
    }

    let store = Store::open_readonly(&index_path)
        .with_context(|| format!("Failed to open index at {}", index_path.display()))?;

    let model = store
        .stored_model_name()
        .unwrap_or_else(|| "<unrecorded>".to_string());
    let dim = store.dim();
    let total_chunks = match store.chunk_count() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to read chunk count");
            0
        }
    };

    let index_db_size = file_size_or_zero(&index_path);
    // Sum the four enriched HNSW files (graph/data/ids/checksum). The split
    // mirrors `HnswIndex::save("index")`.
    let hnsw_size = ["graph", "data", "ids", "checksum"]
        .iter()
        .map(|suffix| file_size_or_zero(&cqs_dir.join(format!("index.hnsw.{suffix}"))))
        .sum::<u64>();
    let cagra_size = file_size_or_zero(&cqs_dir.join("index.cagra"))
        + file_size_or_zero(&cqs_dir.join("index.cagra.meta"));

    if json {
        let out = ModelShowOutput {
            model,
            dim,
            total_chunks,
            index_db_size_bytes: index_db_size,
            hnsw_size_bytes: hnsw_size,
            cagra_size_bytes: cagra_size,
        };
        crate::cli::json_envelope::emit_json(&out)?;
    } else {
        println!("current model: {} ({}-dim)", model, dim);
        println!("chunks:        {}", total_chunks);
        println!("index.db:      {}", human_bytes(index_db_size));
        println!("HNSW files:    {}", human_bytes(hnsw_size));
        if cagra_size > 0 {
            println!("CAGRA files:   {}", human_bytes(cagra_size));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `cqs model list`
// ---------------------------------------------------------------------------

/// List built-in presets, marking the index's current model with `*`.
fn cmd_model_list(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_model_list").entered();

    // Best-effort current-model lookup. A missing index is a soft warning —
    // `list` should still work on a fresh project so the user can see which
    // presets are available before running `cqs init`.
    let current = read_current_model_name();

    let entries: Vec<ModelListEntry> = ModelConfig::PRESET_NAMES
        .iter()
        .filter_map(|name| {
            let cfg = ModelConfig::from_preset(name)?;
            let is_current = current
                .as_deref()
                .map(|c| c == cfg.name || c == cfg.repo)
                .unwrap_or(false);
            Some(ModelListEntry {
                name: cfg.name,
                repo: cfg.repo,
                dim: cfg.dim,
                current: is_current,
            })
        })
        .collect();

    if json {
        // P2.15: wrap in `{models: [...], current: <name>}` so list-shape
        // envelopes match ref/project/slot. The per-row `current` bool stays
        // for backward compatibility but the top-level field is canonical.
        let current_name = entries.iter().find(|e| e.current).map(|e| e.name.clone());
        crate::cli::json_envelope::emit_json(&serde_json::json!({
            "models": entries,
            "current": current_name,
        }))?;
    } else {
        println!("{:<12} {:<6} {:<3} REPO", "NAME", "DIM", "CUR");
        println!("{}", "-".repeat(60));
        for e in &entries {
            let mark = if e.current { "*" } else { " " };
            println!("{:<12} {:<6} {:<3} {}", e.name, e.dim, mark, e.repo);
        }
        if current.is_none() {
            println!();
            println!("(no index found — run `cqs init && cqs index` to record a model)");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `cqs model swap <preset>`
// ---------------------------------------------------------------------------

/// Backup `.cqs/`, reindex with the new preset, restore on failure.
fn cmd_model_swap(cli: &Cli, preset: &str, no_backup: bool, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_model_swap", preset, no_backup).entered();

    // 1. Validate preset.
    // P2.11: emit JSON envelope error on bad preset / missing index so
    // `cqs --json model swap …` returns structured output, not text.
    let new_cfg = match ModelConfig::from_preset(preset) {
        Some(c) => c,
        None => {
            let valid = ModelConfig::PRESET_NAMES.join(", ");
            let msg = format!(
                "Unknown preset '{preset}'. Valid presets: {valid}. Run `cqs model list` for repos."
            );
            if json {
                crate::cli::json_envelope::emit_json_error("unknown_preset", &msg)?;
                std::process::exit(1);
            }
            bail!("{msg}");
        }
    };

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs::resolve_index_db(&cqs_dir);

    if !index_path.exists() {
        let msg = format!(
            "No index at {}. Run `cqs init && cqs index --model {preset}` first.",
            index_path.display()
        );
        if json {
            crate::cli::json_envelope::emit_json_error("no_index", &msg)?;
            std::process::exit(1);
        }
        bail!("{msg}");
    }

    // 2. Read current model. Used both for the no-op short-circuit and the
    //    backup-directory name.
    let current_model = {
        let store = Store::open_readonly(&index_path)
            .with_context(|| format!("Failed to open index at {}", index_path.display()))?;
        store.stored_model_name().unwrap_or_default()
    };

    let already_on_target = !current_model.is_empty()
        && (current_model == new_cfg.name || current_model == new_cfg.repo);
    if already_on_target {
        if json {
            let out = serde_json::json!({
                "from": current_model,
                "to": new_cfg.name,
                "noop": true,
                "message": format!("already on {}, no-op", new_cfg.name),
            });
            crate::cli::json_envelope::emit_json(&out)?;
        } else {
            println!("already on {}, no-op", new_cfg.name);
        }
        return Ok(());
    }

    let from_label = if current_model.is_empty() {
        "<unrecorded>".to_string()
    } else {
        current_model.clone()
    };

    // 3. Stop daemon (best-effort).
    let daemon_was_running = stop_daemon_best_effort(&cqs_dir);
    if !cli.quiet && daemon_was_running {
        eprintln!("stopped cqs-watch daemon");
    }

    // 4. Backup `.cqs/` → `.cqs.<old-shortname>.bak/`. We rename rather than
    //    copy because rename is atomic on the same filesystem and avoids
    //    duplicating the multi-GB index.
    let backup_path = if no_backup {
        None
    } else {
        let bp = backup_path_for(&root, &from_label);
        if let Err(e) = remove_existing_backup(&bp) {
            // Stale leftover backup blocks the rename — surface but still
            // fail-fast: refusing to proceed with no recovery path is the
            // right move.
            restart_daemon_if_needed(daemon_was_running, cli.quiet);
            return Err(e)
                .with_context(|| format!("Pre-existing backup at {} blocks swap", bp.display()));
        }
        if let Err(e) = std::fs::rename(&cqs_dir, &bp) {
            restart_daemon_if_needed(daemon_was_running, cli.quiet);
            return Err(anyhow::anyhow!(
                "Failed to back up {} to {}: {e}. Original index untouched.",
                cqs_dir.display(),
                bp.display()
            ));
        }
        if !cli.quiet {
            eprintln!("backed up {} -> {}", cqs_dir.display(), bp.display());
        }
        Some(bp)
    };

    // 5. Reindex with the new model.
    let start = std::time::Instant::now();
    let reindex_result = reindex_with_new_model(cli, new_cfg.clone());
    let elapsed_secs = start.elapsed().as_secs_f64();

    match reindex_result {
        Ok(()) => {
            // P2 #73: count chunks for the success report. Bind both error
            // paths and emit a `-1` sentinel + `count_error` when we can't
            // read back the post-swap chunk count, so agents can distinguish
            // "really empty project" (chunks_indexed=0, count_error=null)
            // from "swap succeeded but the new index is unreadable"
            // (chunks_indexed=-1, count_error=Some(...)).
            let (chunks_indexed, count_error): (i64, Option<String>) =
                match Store::open_readonly(&index_path) {
                    Ok(s) => match s.chunk_count() {
                        Ok(n) => (n as i64, None),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Swap reported success but post-swap chunk_count failed"
                            );
                            (-1, Some(format!("chunk_count failed: {e}")))
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Swap reported success but post-swap store open failed"
                        );
                        (-1, Some(format!("store open failed: {e}")))
                    }
                };

            restart_daemon_if_needed(daemon_was_running, cli.quiet);

            if json {
                let out = ModelSwapOutput {
                    from: from_label.clone(),
                    to: new_cfg.name.clone(),
                    chunks_indexed,
                    count_error,
                    elapsed_secs,
                    backup_path: backup_path.as_ref().map(|p| p.display().to_string()),
                };
                crate::cli::json_envelope::emit_json(&out)?;
            } else {
                if let Some(ref err) = count_error {
                    eprintln!(
                        "warning: swap succeeded but could not read back chunk count: {}",
                        err
                    );
                }
                let chunks_label = if chunks_indexed < 0 {
                    "?".to_string()
                } else {
                    chunks_indexed.to_string()
                };
                println!(
                    "swapped: {} -> {} ({} chunks, {:.1}s)",
                    from_label, new_cfg.name, chunks_label, elapsed_secs
                );
                if let Some(bp) = &backup_path {
                    println!(
                        "backup kept at {}. `rm -rf` once you've verified the new index.",
                        bp.display()
                    );
                }
            }
            Ok(())
        }
        Err(reindex_err) => {
            // 6. Restore from backup.
            let restore_outcome = if let Some(ref bp) = backup_path {
                restore_from_backup(&cqs_dir, bp)
            } else {
                Err(anyhow::anyhow!(
                    "no backup was taken (--no-backup), cannot restore"
                ))
            };

            restart_daemon_if_needed(daemon_was_running, cli.quiet);

            match restore_outcome {
                Ok(()) => Err(anyhow::anyhow!(
                    "Reindex failed: {reindex_err}. Original index restored from {}.",
                    backup_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<no backup>".to_string())
                )),
                Err(restore_err) => Err(anyhow::anyhow!(
                    "REINDEX FAILED *AND* RESTORE FAILED. \
                     Reindex error: {reindex_err}. \
                     Restore error: {restore_err}. \
                     Manual recovery required from {} — \
                     `rm -rf {}` then `mv {} {}` and run `cqs index --force`.",
                    backup_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<no backup>".to_string()),
                    cqs_dir.display(),
                    backup_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<no backup>".to_string()),
                    cqs_dir.display(),
                )),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Re-run `cmd_index --force` against the project, but with a fresh `Cli`
/// whose `resolved_model` points at the new preset.
///
/// We construct a fresh `Cli` rather than mutating `&Cli` because dispatch
/// hands us a borrowed reference. Only the fields cmd_index actually reads
/// (`quiet`, `try_model_config`) need to be set; everything else stays at
/// clap defaults.
fn reindex_with_new_model(cli: &Cli, new_cfg: ModelConfig) -> Result<()> {
    let _span = tracing::info_span!("reindex_with_new_model", model = %new_cfg.name).entered();

    let mut new_cli = clone_cli_for_reindex(cli);
    new_cli.resolved_model = Some(new_cfg);

    let args = IndexArgs {
        force: true,
        dry_run: false,
        no_ignore: false,
        #[cfg(feature = "llm-summaries")]
        llm_summaries: false,
        #[cfg(feature = "llm-summaries")]
        improve_docs: false,
        #[cfg(feature = "llm-summaries")]
        apply: false,
        #[cfg(feature = "llm-summaries")]
        improve_all: false,
        #[cfg(feature = "llm-summaries")]
        max_docs: None,
        #[cfg(feature = "llm-summaries")]
        hyde_queries: false,
        #[cfg(feature = "llm-summaries")]
        max_hyde: None,
        umap: false,
        // P2.12: model swap drives a programmatic reindex; we never want
        // the swap path to spit a JSON envelope to stdout (the caller is
        // already mid-text-rendering). Keep the inner index run on the
        // text path regardless of the outer `--json`.
        json: false,
    };

    cmd_index(&new_cli, &args)
}

/// Build a fresh `Cli` populated with just the fields `cmd_index` reads.
///
/// The `Cli` struct has dozens of fields (search flags, output options, etc.)
/// that only matter to other subcommands. Forging a default-valued copy and
/// overlaying `quiet` is enough to drive `cmd_index` correctly.
fn clone_cli_for_reindex(cli: &Cli) -> Cli {
    use clap::Parser as _;
    // `try_parse_from(["cqs"])` runs clap with no subcommand — defaults all
    // fields, leaves `command` as `None`. That's the cheapest way to get a
    // zero-config Cli without listing every field by hand.
    let mut fresh = Cli::try_parse_from(["cqs"]).expect("Cli with no args must parse");
    fresh.quiet = cli.quiet;
    fresh.verbose = cli.verbose;
    fresh
}

/// Best-effort daemon stop. Returns true if the daemon was likely running
/// before the call (used to decide whether to restart on the way out).
///
/// Linux: `systemctl --user stop cqs-watch`.
///
/// P2 #71: macOS — `systemctl` doesn't exist, but `cqs watch --serve` is
/// equally Unix domain socket-based. Discover the daemon PID via
/// `LOCAL_PEERPID` on a fresh socket connection, then `SIGTERM` it and poll
/// for socket disappearance. Without this fix `cqs model swap` on macOS would
/// always proceed against a live daemon holding the OLD model in memory and
/// the old store pool open against the database file the swap is rewriting.
fn stop_daemon_best_effort(cqs_dir: &Path) -> bool {
    let _span = tracing::info_span!("stop_daemon_best_effort").entered();
    let was_running = daemon_socket_alive(cqs_dir);
    if !was_running {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = cqs_dir; // not needed on the Linux systemctl path
        let status = std::process::Command::new("systemctl")
            .args(["--user", "stop", "cqs-watch"])
            .status();
        match status {
            Ok(s) if s.success() => {
                tracing::info!("Stopped cqs-watch via systemctl");
                true
            }
            Ok(s) => {
                tracing::warn!(code = ?s.code(), "systemctl --user stop cqs-watch returned non-zero");
                // Still report `true` — the daemon may have been stopped by
                // someone else, or systemctl was unavailable. Caller will
                // attempt restart anyway.
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to invoke systemctl");
                true
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        match stop_daemon_via_sigterm_macos(cqs_dir) {
            Ok(()) => {
                tracing::info!("Stopped cqs-watch via SIGTERM");
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to stop daemon via SIGTERM on macOS");
                // Still report `true` so the caller restarts what it stopped
                // (or attempted to stop) — the alternative is silently
                // proceeding against a live daemon, which is the bug this
                // branch exists to fix.
                true
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = cqs_dir; // silence unused warning on other platforms
        tracing::debug!(
            "Daemon control unsupported on this platform; skipping (systemctl=Linux, SIGTERM=macOS)"
        );
        false
    }
}

/// macOS-only: discover the daemon PID via `getsockopt(LOCAL_PEERPID)` on
/// a fresh socket connection, send SIGTERM, then poll for the socket file
/// to disappear.
///
/// P2 #71: factored out so `stop_daemon_best_effort` stays readable while the
/// macOS-specific FFI lives in one place. Failure modes covered:
/// - socket vanishes between `daemon_socket_alive` and our connect: returns
///   Ok(()) — the daemon went away on its own and the caller's `was_running`
///   bookkeeping still tells it to attempt a restart.
/// - getsockopt failure: surfaces as Err. Caller logs and proceeds; the
///   restart path will spawn a new daemon either way.
/// - kill(SIGTERM) failure: surfaces as Err.
/// - daemon ignores SIGTERM and the socket lingers: we wait up to ~3s, then
///   give up. The reindex will still race the live daemon — that's a real
///   loss of safety, but at least the operator sees the warning in the log.
#[cfg(target_os = "macos")]
fn stop_daemon_via_sigterm_macos(cqs_dir: &Path) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    use std::time::Duration;

    let sock_path = cqs::daemon_translate::daemon_socket_path(cqs_dir);
    if !sock_path.exists() {
        return Ok(());
    }
    let stream = std::os::unix::net::UnixStream::connect(&sock_path)
        .with_context(|| format!("Failed to connect to daemon socket {}", sock_path.display()))?;

    // SOL_LOCAL / LOCAL_PEERPID returns the PID of the daemon listening on
    // the connected socket. Standard macOS API; available since 10.8.
    // SAFETY: getsockopt is FFI but we pass valid pointer/length pairs and
    // check the return value before dereferencing the result.
    let mut pid: libc::pid_t = 0;
    let mut len: libc::socklen_t = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast::<libc::c_void>(),
            &mut len,
        )
    };
    drop(stream); // close before signaling so the daemon's accept loop sees the disconnect.
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow::anyhow!("getsockopt(LOCAL_PEERPID) failed: {err}"));
    }
    if pid <= 0 {
        return Err(anyhow::anyhow!(
            "LOCAL_PEERPID returned non-positive pid {pid}; cannot signal"
        ));
    }
    tracing::info!(
        pid,
        "Discovered daemon PID via LOCAL_PEERPID; sending SIGTERM"
    );

    // SAFETY: kill(2) FFI; pid validated > 0 above. SIGTERM is the documented
    // graceful-shutdown signal for cqs watch --serve (the SocketCleanupGuard
    // in watch.rs unlinks the socket on drop).
    let kill_rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    if kill_rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow::anyhow!("kill({pid}, SIGTERM) failed: {err}"));
    }

    // Poll for the socket to disappear (the daemon's SocketCleanupGuard
    // unlinks it on graceful shutdown). 3s budget is generous: the daemon's
    // periodic GC tick is ~30s but the accept loop wakes every few hundred
    // milliseconds, and SIGTERM is handled in the main shutdown path.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if !sock_path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::warn!(
        pid,
        socket = %sock_path.display(),
        "Daemon did not unlink socket within 3s of SIGTERM; proceeding anyway (model swap may race)"
    );
    Ok(())
}

/// Best-effort daemon restart. No-op when the daemon wasn't previously running.
///
/// Linux: `systemctl --user start cqs-watch`.
/// P2 #71: macOS — spawn `cqs watch --serve` directly (no systemctl); detached
/// stdio so we don't block on the long-lived child.
fn restart_daemon_if_needed(was_running: bool, quiet: bool) {
    if !was_running {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new("systemctl")
            .args(["--user", "start", "cqs-watch"])
            .status();
        match status {
            Ok(s) if s.success() => {
                if !quiet {
                    eprintln!("restarted cqs-watch daemon");
                }
            }
            Ok(s) => {
                tracing::warn!(code = ?s.code(), "systemctl --user start cqs-watch returned non-zero");
                if !quiet {
                    eprintln!(
                        "warning: failed to restart cqs-watch daemon (systemctl exited {:?}). \
                         Run `systemctl --user start cqs-watch` manually.",
                        s.code()
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to invoke systemctl on restart");
                if !quiet {
                    eprintln!(
                        "warning: failed to restart cqs-watch daemon ({e}). \
                         Run `systemctl --user start cqs-watch` manually."
                    );
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        // No systemctl on macOS — re-launch `cqs watch --serve` directly.
        // SEC-V1.25-7: resolve our own binary path so a malicious `cqs`
        // earlier in PATH can't hijack the restart.
        match std::env::current_exe() {
            Ok(cqs_path) => {
                let spawn_result = std::process::Command::new(&cqs_path)
                    .args(["watch", "--serve"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                match spawn_result {
                    Ok(child) => {
                        tracing::info!(pid = child.id(), "Restarted cqs watch --serve on macOS");
                        if !quiet {
                            eprintln!("restarted cqs-watch daemon (pid {})", child.id());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to spawn cqs watch --serve");
                        if !quiet {
                            eprintln!(
                                "warning: failed to restart cqs-watch daemon ({e}). \
                                 Run `cqs watch --serve &` manually."
                            );
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "current_exe() failed; cannot restart daemon");
                if !quiet {
                    eprintln!(
                        "warning: failed to resolve cqs binary path ({e}). \
                         Run `cqs watch --serve &` manually."
                    );
                }
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = quiet;
    }
}

/// Cheap probe for "is the daemon socket present". The actual daemon may
/// have died and left a stale socket; that's fine — we just want a hint
/// for whether to attempt a restart.
fn daemon_socket_alive(cqs_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let sock = cqs::daemon_translate::daemon_socket_path(cqs_dir);
        sock.exists()
    }
    #[cfg(not(unix))]
    {
        let _ = cqs_dir;
        false
    }
}

/// Compute `<root>/.cqs.<shortname>.bak` for the backup destination.
///
/// Sanitizes the shortname so a stored model id like `BAAI/bge-large-en-v1.5`
/// does not produce a path with embedded slashes.
fn backup_path_for(root: &Path, model_label: &str) -> PathBuf {
    let safe: String = model_label
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    root.join(format!(".cqs.{safe}.bak"))
}

/// Remove a pre-existing backup directory if present. Distinct from
/// `restore_from_backup` because the rename source/dest semantics differ.
fn remove_existing_backup(backup: &Path) -> Result<()> {
    if !backup.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(backup)
        .with_context(|| format!("Failed to remove stale backup at {}", backup.display()))
}

/// Restore `.cqs/` from a backup directory, nuking any partial in-place
/// `.cqs/` first.
fn restore_from_backup(cqs_dir: &Path, backup: &Path) -> Result<()> {
    let _span = tracing::info_span!("restore_from_backup").entered();
    if cqs_dir.exists() {
        std::fs::remove_dir_all(cqs_dir).with_context(|| {
            format!(
                "Failed to remove partial {} before restore",
                cqs_dir.display()
            )
        })?;
    }
    std::fs::rename(backup, cqs_dir).with_context(|| {
        format!(
            "Failed to rename {} back to {}",
            backup.display(),
            cqs_dir.display()
        )
    })?;
    Ok(())
}

/// Best-effort current-model lookup. Returns `None` if no index exists or
/// metadata can't be read.
fn read_current_model_name() -> Option<String> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs::resolve_index_db(&cqs_dir);
    if !index_path.exists() {
        return None;
    }
    Store::open_readonly(&index_path)
        .ok()
        .and_then(|s| s.stored_model_name())
}

/// Read file size, returning 0 for missing files (rather than propagating).
/// Used for the show/swap reports where a missing optional file is fine.
fn file_size_or_zero(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Pretty-print a byte count as KiB / MiB / GiB. Used for `cqs model show`
/// human output.
fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.2} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.2} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.2} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.00 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 5), "5.00 MiB");
        assert_eq!(human_bytes(1024u64.pow(3) * 2), "2.00 GiB");
    }

    #[test]
    fn backup_path_sanitizes_slashes() {
        let root = Path::new("/tmp/x");
        // Repo-id-style label keeps slashes out of the directory name.
        let p = backup_path_for(root, "BAAI/bge-large-en-v1.5");
        assert_eq!(p, PathBuf::from("/tmp/x/.cqs.BAAI-bge-large-en-v1.5.bak"));
    }

    #[test]
    fn backup_path_short_name_unchanged() {
        let root = Path::new("/tmp/x");
        let p = backup_path_for(root, "v9-200k");
        assert_eq!(p, PathBuf::from("/tmp/x/.cqs.v9-200k.bak"));
    }

    #[test]
    fn backup_path_unrecorded_label_safe() {
        // Empty / unrecorded model name must still produce a usable path.
        let root = Path::new("/tmp/x");
        let p = backup_path_for(root, "<unrecorded>");
        // Path component contains only ASCII chars after sanitization.
        assert!(p.to_string_lossy().ends_with(".bak"));
    }

    #[test]
    fn file_size_or_zero_missing_path() {
        assert_eq!(file_size_or_zero(Path::new("/nonexistent/file/path/x")), 0);
    }

    /// P2 #73: zero chunks (empty project) emits `0` with no `count_error`
    /// field — agents see an unambiguous "really empty" signal.
    #[test]
    fn model_swap_output_zero_chunks_no_error() {
        let out = ModelSwapOutput {
            from: "bge-large".to_string(),
            to: "v9-200k".to_string(),
            chunks_indexed: 0,
            count_error: None,
            elapsed_secs: 1.5,
            backup_path: None,
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["chunks_indexed"], 0);
        assert!(
            json.get("count_error").is_none(),
            "skip_serializing_if must drop None count_error"
        );
        assert_eq!(json["from"], "bge-large");
        assert_eq!(json["to"], "v9-200k");
    }

    /// P2 #73: when the post-swap chunk_count fails, agents see
    /// `chunks_indexed: -1` and a populated `count_error` string.
    #[test]
    fn model_swap_output_sentinel_with_count_error() {
        let out = ModelSwapOutput {
            from: "<unrecorded>".to_string(),
            to: "v9-200k".to_string(),
            chunks_indexed: -1,
            count_error: Some("chunk_count failed: I/O error".to_string()),
            elapsed_secs: 0.1,
            backup_path: Some("/tmp/x/.cqs.bak".to_string()),
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["chunks_indexed"], -1);
        assert_eq!(json["count_error"], "chunk_count failed: I/O error");
        assert_eq!(json["backup_path"], "/tmp/x/.cqs.bak");
    }
}
