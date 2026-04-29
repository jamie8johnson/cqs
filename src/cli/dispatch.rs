//! Command dispatch: matches parsed CLI subcommands to handler functions.

use anyhow::Result;

use super::config::{apply_config_defaults, find_project_root};
#[cfg(unix)]
use super::definitions::BatchSupport;
use super::definitions::{Cli, Commands};
use super::telemetry;
use super::{batch, chat, watch};

#[cfg(feature = "convert")]
use super::commands::cmd_convert;
use super::commands::{
    cmd_affected, cmd_audit_mode, cmd_blame, cmd_brief, cmd_cache, cmd_callees, cmd_callers,
    cmd_ci, cmd_context, cmd_dead, cmd_deps, cmd_diff, cmd_doctor, cmd_drift, cmd_eval,
    cmd_explain, cmd_export_model, cmd_gather, cmd_gc, cmd_health, cmd_hook, cmd_impact,
    cmd_impact_diff, cmd_index, cmd_init, cmd_model, cmd_neighbors, cmd_notes, cmd_onboard,
    cmd_ping, cmd_plan, cmd_project, cmd_query, cmd_read, cmd_reconstruct, cmd_ref, cmd_related,
    cmd_review, cmd_scout, cmd_similar, cmd_slot, cmd_stale, cmd_stats, cmd_status, cmd_suggest,
    cmd_task, cmd_telemetry, cmd_telemetry_reset, cmd_test_map, cmd_trace, cmd_train_data,
    cmd_train_pairs, cmd_where,
};

// ── Dispatch macros (#1097) ──────────────────────────────────────────────
//
// Both Group A (no-store, early-return) and Group B (store-using) matches
// in `run_with` are generated from the single registration table in
// `crate::cli::registry`. The registry splits rows into `group_a:` and
// `group_b:` brace groups; each emitter consumes both and emits the right
// arm shape per group:
//
//   * **Group A match** (before `let ctx = …`)
//     - `group_a` row → `Some($bind) => return $body`
//     - `group_b` row → `Some($wild) => {}`  (no-op fall-through)
//     - `None` arm    → `=> {}`              (bare query, handled below)
//
//   * **Group B match** (after `let ctx = …`)
//     - `group_a` row → `Some($wild) => unreachable!()`  (already returned)
//     - `group_b` row → `Some($bind) => $body`           (uses `&ctx`, `&cli`)
//     - `None` arm    → bare-query path (`cmd_query`) and usage banner
//
// Both matches are exhaustive over `Commands`. The legacy `_ => {}` and
// `_ => unreachable!()` catch-alls are gone — adding a new variant without
// registering it is a compile error.
//
// Hygiene: macro_rules is unhygienic for free identifiers, so `cli`,
// `ctx`, and the imported `cmd_*` paths in each `$body` resolve in the
// `run_with` scope as expected. Local `let` bindings inside a body are
// hygienic and won't collide with the Group B `let ctx = …` between the
// two macro-emitted matches.
macro_rules! gen_dispatch_group_a {
    (
        cli = $cli:ident,
        ctx = $_ctx:ident,
        project_cqs_dir = $_pcd:ident,
        group_a: {
            $(
                $(#[$a_attr:meta])*
                ( $a_bind:pat , $a_wild:pat , $a_name:literal , $a_bs:expr , $a_body:block )
            ),* $(,)?
        }
        group_b: {
            $(
                $(#[$b_attr:meta])*
                ( $b_bind:pat , $b_wild:pat , $b_name:literal , $b_bs:expr , $b_body:block )
            ),* $(,)?
        }
    ) => {
        match $cli.command {
            $(
                $(#[$a_attr])*
                Some($a_bind) => return $a_body,
            )*
            $(
                $(#[$b_attr])*
                Some($b_wild) => {}, // handled in Group B match below
            )*
            None => {} // bare-query mode handled in Group B below
        }
    };
}

macro_rules! gen_dispatch_group_b {
    (
        cli = $cli:ident,
        ctx = $ctx:ident,
        project_cqs_dir = $_pcd:ident,
        group_a: {
            $(
                $(#[$a_attr:meta])*
                ( $a_bind:pat , $a_wild:pat , $a_name:literal , $a_bs:expr , $a_body:block )
            ),* $(,)?
        }
        group_b: {
            $(
                $(#[$b_attr:meta])*
                ( $b_bind:pat , $b_wild:pat , $b_name:literal , $b_bs:expr , $b_body:block )
            ),* $(,)?
        }
    ) => {
        match $cli.command {
            $(
                $(#[$a_attr])*
                Some($a_wild) => unreachable!(
                    "Group A variant `{}` handled before context open",
                    $a_name
                ),
            )*
            $(
                $(#[$b_attr])*
                Some($b_bind) => $b_body,
            )*
            None => match &$cli.query {
                Some(q) => cmd_query(&$ctx, q),
                None => {
                    println!("Usage: cqs <query> or cqs <command>");
                    println!("Run 'cqs --help' for more information.");
                    Ok(())
                }
            },
        }
    };
}

/// Run CLI with pre-parsed arguments (used when main.rs needs to inspect args first)
pub fn run_with(cli: Cli) -> Result<()> {
    // Log command for telemetry (opt-in via CQS_TELEMETRY=1)
    let project_cqs_dir = cqs::resolve_index_dir(&find_project_root());
    let telem_args: Vec<String> = std::env::args().collect();
    let (telem_cmd, telem_query) = telemetry::describe_command(&telem_args);
    telemetry::log_command(&project_cqs_dir, &telem_cmd, telem_query.as_deref(), None);
    let started = std::time::Instant::now();

    // Inner function carries the multiple early-return paths (daemon
    // forwarding, group-A subcommands' `return $body;`, group-B tail) and
    // funnels them into a single Result we can attach the completion-event
    // telemetry to. Without this, half the invocations would never get a
    // duration/ok event recorded.
    let result = run_with_dispatch(cli, &project_cqs_dir, &telem_cmd);

    telemetry::log_command_complete(
        &project_cqs_dir,
        &telem_cmd,
        started.elapsed().as_millis() as u64,
        result.is_ok(),
        result.as_ref().err().map(|e| e.to_string()).as_deref(),
    );
    result
}

/// Inner dispatch body — separated from [`run_with`] so the outer can
/// uniformly observe the completion outcome via [`telemetry::log_command_complete`].
/// All early returns from the body land back here as the inner function's
/// return value, which the outer wraps with timing + ok/err telemetry.
fn run_with_dispatch(
    mut cli: Cli,
    project_cqs_dir: &std::path::Path,
    telem_cmd: &str,
) -> Result<()> {
    // v1.22.0 audit OB-14: root span so all per-command logs have a parent.
    let _root = tracing::info_span!("cqs", cmd = %telem_cmd).entered();

    // Slot migration: one-shot move of legacy `.cqs/index.db` (+ HNSW + SPLADE)
    // into `.cqs/slots/default/` on first post-upgrade run. Idempotent — every
    // subsequent run observes `.cqs/slots/` and skips. Safe to call on
    // never-indexed projects (returns false, no-op).
    if project_cqs_dir.exists() {
        if let Err(e) = cqs::slot::migrate_legacy_index_to_default_slot(project_cqs_dir) {
            tracing::warn!(error = %e, "slot migration failed; continuing without it");
        }
    }

    // Propagate `--slot <name>` to `CQS_SLOT` env so commands that resolve the
    // active slot via `cqs::slot::resolve_slot_name(None, ...)` (no ctx-passed
    // flag) honor the explicit override. Resolution order is preserved:
    // `--slot` (now in env) > pre-existing `CQS_SLOT` > `.cqs/active_slot` >
    // `"default"`. Only set when the flag was passed on the CLI.
    if let Some(ref slot_name) = cli.slot {
        std::env::set_var("CQS_SLOT", slot_name);
    }

    // Load config and apply defaults (CLI flags override config)
    let config = cqs::config::Config::load(&find_project_root());
    apply_config_defaults(&mut cli, &config);

    // v1.22.0 audit CQ-1: wire the [scoring] config section to the
    // RRF K override. Previously `set_rrf_k_from_config` existed but
    // nothing called it — a user writing `[scoring] rrf_k = 40` in
    // `.cqs.toml` had their value silently ignored.
    if let Some(ref scoring) = config.scoring {
        cqs::store::set_rrf_k_from_config(scoring);
    }

    // Resolve embedding model config once. Priority (#1107):
    //   1. `--model` CLI flag           (explicit override)
    //   2. `.cqs/slots/<name>/slot.toml` (slot intent — set at `slot create --model`)
    //   3. `CQS_EMBEDDING_MODEL` env
    //   4. `.cqs.toml [embedding]`
    //   5. default preset
    //
    // Slot intent is forwarded by passing the persisted preset/repo as
    // `cli_model` to `ModelConfig::resolve`, which makes it land in priority
    // slot 1 inside `resolve` (still beating env/config) without needing a new
    // resolve signature.
    let slot_model_intent = if cli.model.is_none() {
        // EH-V1.30.1-3: surface slot-resolution failures via tracing instead
        // of `.ok()` swallowing them. A bad slot pointer or read error here
        // means the persisted model intent gets silently ignored — the
        // operator sees the wrong model resolve and has zero observability
        // on why.
        let resolved_slot = match cqs::slot::resolve_slot_name(cli.slot.as_deref(), project_cqs_dir)
        {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    slot = ?cli.slot,
                    "slot resolution failed when looking up persisted model intent — \
                     falling back to default model resolution"
                );
                None
            }
        };
        resolved_slot.and_then(|s| cqs::slot::read_slot_model(project_cqs_dir, &s.name))
    } else {
        None
    };
    let effective_cli_model = cli.model.as_deref().or(slot_model_intent.as_deref());
    cli.resolved_model = Some(
        cqs::embedder::ModelConfig::resolve(effective_cli_model, config.embedding.as_ref())
            .apply_env_overrides(),
    );

    // Clamp limit to prevent usize::MAX wrapping to -1 in SQLite queries
    cli.limit = cli.limit.clamp(1, 100);

    // ── Daemon client: forward to running daemon if available ──────────────
    // The daemon binds to whichever slot was active at *its* startup (per
    // spec). If the user passed `--slot <name>`, bypass the daemon so the
    // requested slot wins instead of silently getting the daemon's slot.
    #[cfg(unix)]
    if cli.slot.is_none() && std::env::var("CQS_NO_DAEMON").as_deref() != Ok("1") {
        // P2.17: daemon protocol errors now surface as `Err` instead of being
        // logged-and-fall-through. Transport-level failures still return
        // `Ok(None)` so CLI fallback works for those.
        if let Some(output) = try_daemon_query(project_cqs_dir, &cli)? {
            print!("{}", output);
            return Ok(());
        }
    }

    // ── Group A + Group B dispatch (#1097) ──────────────────────────────────
    //
    // Both matches are generated from the single registration table in
    // `crate::cli::registry`. `__group_a_arm!` and `__group_b_arm!` (defined
    // below) discriminate per row's group ident — Group A rows produce
    // `=> return $body;` in match #1 and `=> {}` in match #2; Group B rows
    // produce `=> {}` in match #1 and `=> $body` (with `&ctx` bound) in
    // match #2. The legacy `_ => {}` and `_ => unreachable!()` catch-alls
    // are gone — both matches are fully exhaustive over `Commands`.
    //
    // Task #8 — `--json` precedence: every Group B subcommand reads
    // `cli.json || output.json` (OR semantics). Top-level `--json` wins
    // when set; the subcommand's `--json` is the fallback. For the impact/
    // trace pair (`OutputArgs` with `--format`), `cli.json` short-circuits
    // to `OutputFormat::Json` regardless of `--format`. The exact wiring
    // lives per-row in the registry.
    crate::cli::registry::for_each_command!(
        gen_dispatch_group_a,
        cli = cli,
        ctx = ctx,
        project_cqs_dir = project_cqs_dir
    );

    let ctx = crate::cli::CommandContext::open_readonly(&cli)?;
    crate::cli::registry::for_each_command!(
        gen_dispatch_group_b,
        cli = cli,
        ctx = ctx,
        project_cqs_dir = project_cqs_dir
    )
}

/// Generate shell completion scripts for the specified shell
fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}

/// Try to forward the current command to a running daemon.
///
/// Returns:
/// - `Ok(Some(output))` — daemon handled the request successfully.
/// - `Ok(None)` — daemon not present, transport failed, or command isn't
///   daemon-dispatchable (index/watch/gc/init/etc.); CLI should run inline.
/// - `Err(_)` — daemon understood the request but returned a protocol-level
///   error. P2.17: surface this as a real error rather than warn-and-retry,
///   since CLI fallback can produce different results from the daemon path.
#[cfg(unix)]
fn try_daemon_query(cqs_dir: &std::path::Path, cli: &Cli) -> Result<Option<String>, anyhow::Error> {
    // OB-NEW-5: root span so every failed-transport fallback is traceable.
    // Commands doesn't derive Debug so we log the discriminant name instead.
    let cmd_label = cli
        .command
        .as_ref()
        .map(|c| c.variant_name())
        .unwrap_or("search");
    let _span = tracing::debug_span!("try_daemon_query", cmd = cmd_label).entered();

    // #947: the hand-maintained allowlist is gone. Every `Commands` variant
    // classifies itself via `batch_support()`; the match there is exhaustive,
    // so adding a new CLI command forces an explicit daemon-forwarding
    // decision at compile time. API-V1.25-1 and the later notes-mutation
    // regression (PR #945) are now structurally impossible — no surface
    // change can silently flip a command's daemon behavior.
    //
    // None (= default search `cqs "query"`) is always daemon-dispatchable.
    if let Some(cmd) = &cli.command {
        if cmd.batch_support() == BatchSupport::Cli {
            return Ok(None);
        }
    }

    let sock_path = super::daemon_socket_path(cqs_dir);
    if !sock_path.exists() {
        return Ok(None);
    }

    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    let stream = match UnixStream::connect(&sock_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(
                path = %sock_path.display(),
                error = %e,
                stage = "connect",
                "Daemon transport failed, falling back to CLI"
            );
            return Ok(None);
        }
    };

    // P2 #41 (post-v1.27.0 audit): single knob across client and daemon.
    // The previous TODO(cross-coordination) noted that `handle_socket_client`
    // hardcoded 5s/30s timeouts; both sides now resolve through the shared
    // `cqs::daemon_translate::resolve_daemon_timeout_ms` helper.
    let timeout = cqs::daemon_translate::resolve_daemon_timeout_ms();
    // EH-14: explicit warn on timeout failures rather than silent `.ok()` —
    // without a timeout the CLI could hang forever on a wedged daemon read.
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::warn!(
            error = %e,
            "Failed to set read timeout on daemon client stream — CLI may hang on wedged daemon"
        );
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::warn!(
            error = %e,
            "Failed to set write timeout on daemon client stream — CLI may hang on wedged daemon"
        );
    }

    // #972: arg-stripping and `-n`→`--limit` remap live in
    // `cqs::daemon_translate::translate_cli_args_to_batch`, a pure helper in
    // the library crate. Integration tests pin its behaviour separately
    // (tests/daemon_forward_test.rs). The caller still owns side effects:
    // emitting the `--model ignored` warning and framing the JSON request.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    // API-V1.25-8: `--model` is stripped because the daemon runs a single
    // loaded model. Surface the mismatch to the user rather than silently
    // ignoring their flag.
    if let Some(m) = cqs::daemon_translate::stripped_model_value(&raw_args) {
        tracing::warn!(
            requested_model = %m,
            "Daemon ignores --model; query will run against daemon's loaded model. \
             Set CQS_NO_DAEMON=1 to force CLI mode with the requested model."
        );
    }
    let (command, cmd_args) =
        cqs::daemon_translate::translate_cli_args_to_batch(&raw_args, cli.command.is_some());
    let request = serde_json::json!({
        "command": command,
        "args": cmd_args,
    });

    let mut stream = stream;
    if let Err(e) = writeln!(stream, "{}", request) {
        tracing::debug!(error = %e, stage = "write", "Daemon transport failed, falling back to CLI");
        return Ok(None);
    }
    if let Err(e) = stream.flush() {
        tracing::debug!(error = %e, stage = "flush", "Daemon transport failed, falling back to CLI");
        return Ok(None);
    }

    // RB-NEW-4: bound the response so a rogue/buggy daemon can't force us to
    // allocate unbounded memory on `read_line`. 16 MiB matches the practical
    // ceiling for gather/task JSON outputs. P3 #109: env-overridable via
    // CQS_DAEMON_MAX_RESPONSE_BYTES so large gather/task outputs on big
    // corpora can lift the cap.
    let max_daemon_response = crate::cli::limits::max_daemon_response_bytes();
    use std::io::Read as _;
    let mut reader = std::io::BufReader::new(&stream).take(max_daemon_response);
    let mut response_line = String::new();
    let bytes_read = match reader.read_line(&mut response_line) {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(
                error = %e,
                stage = "read",
                "Daemon transport failed, falling back to CLI"
            );
            return Ok(None);
        }
    };
    if bytes_read as u64 == max_daemon_response {
        // P3 #109: surface the silent perf regression on stderr, not
        // just tracing — agents tuning latency won't see tracing.
        let cap_mib = max_daemon_response / 1024 / 1024;
        eprintln!(
            "warning: cqs daemon response exceeded {cap_mib} MiB cap — falling back to direct CLI execution. \
             Set CQS_DAEMON_MAX_RESPONSE_BYTES to lift the cap."
        );
        tracing::warn!(
            bytes = bytes_read,
            cap_mib,
            "Daemon response exceeded cap — falling back to CLI"
        );
        return Ok(None);
    }

    let resp: serde_json::Value = match serde_json::from_str(response_line.trim()) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                error = %e,
                stage = "parse",
                "Daemon transport failed, falling back to CLI"
            );
            return Ok(None);
        }
    };
    let status = match resp.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            tracing::debug!(
                stage = "parse",
                "Daemon response missing 'status' field, falling back to CLI"
            );
            return Ok(None);
        }
    };
    if status == "ok" {
        // P2 #62 (post-v1.27.0 audit, partial): the daemon now embeds the
        // dispatch output as a real JSON value when the bytes parse as JSON
        // (the common case), and falls back to the legacy string form for
        // plaintext handlers. Accept both shapes:
        //   - `Value::String(s)` — print verbatim (preserves original
        //     whitespace from the dispatch handler).
        //   - any other `Value` — re-serialize for the terminal print. Cost
        //     is one re-encode but no escape inflation, replacing the prior
        //     parse-of-escaped-string cost the client used to pay.
        // `daemon_ping` already handled both shapes for the same reason.
        let output = match resp.get("output") {
            Some(v) => v,
            None => {
                tracing::warn!(
                    stage = "parse",
                    "Daemon ok response missing/unserializable output — falling back to CLI"
                );
                return Ok(None);
            }
        };
        let text = match output {
            serde_json::Value::String(s) => s.clone(),
            // API-V1.29-8: pretty-print to match CLI `emit_json` so agents
            // diffing `cqs --json …` output between CLI and daemon modes
            // don't hit spurious whitespace drift. One re-encode cost; worth
            // the parity with the in-process path.
            other => match serde_json::to_string_pretty(other) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        stage = "parse",
                        "Daemon ok response missing/unserializable output — falling back to CLI"
                    );
                    return Ok(None);
                }
            },
        };
        return Ok(Some(text));
    }

    // P2.17 / EH-13: daemon understood the request but surfaced an error.
    // Transport-level failures (connect/read/write) already returned
    // `Ok(None)` above, so reaching here means this is a daemon protocol
    // error the user needs to see. Falling back to CLI here would mask
    // daemon bugs and silently change results — return Err so the caller
    // exits non-zero with the daemon's message.
    let msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("daemon error");
    tracing::warn!(error = msg, "Daemon returned protocol-level error");
    Err(anyhow::anyhow!(
        "daemon error: {msg}\nhint: set CQS_NO_DAEMON=1 to bypass the daemon"
    ))
}
