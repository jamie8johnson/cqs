//! Command dispatch: matches parsed CLI subcommands to handler functions.

use anyhow::Result;

use super::config::{apply_config_defaults, find_project_root};
#[cfg(unix)]
use super::definitions::BatchSupport;
use super::definitions::Cli;
use super::telemetry;

// #1366: Group A and Group B dispatch live in
// `crate::cli::definitions::dispatch_group_{a,b}` (emitted by
// `cqs_macros::CqsCommands` derive on the `Commands` enum). Per-variant
// shims live in `commands::dispatch_shims` and forward to existing
// handlers — see the docstring on that module for the standardized
// dispatch-shim signature. The previous `for_each_command!` central
// registry + `gen_dispatch_group_a` / `gen_dispatch_group_b` emitter
// macros are gone; per-variant `#[cqs_cmd(group, batch)]` attributes
// drive everything now.

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

    // #1453: load per-slot SPLADE α overrides from `slot.toml [splade.alpha]`
    // and install them on the search router. Done once at dispatch entry so
    // every search/eval/batch path benefits without per-call I/O.
    //
    // Resolution mirrors the slot-model lookup above: pick the active slot
    // (`--slot` > CQS_SLOT > active_slot file > "default"), read the alpha
    // table, install. Slot-resolution failure (missing project, malformed
    // slot.toml) → empty table → router falls through to env / preset /
    // default precedence as before.
    // EH-V1.38-5 (#1463): mirror the EH-V1.30.1-3 fix shape applied 30
    // lines above. Pre-fix `.ok().map(...).unwrap_or_default()` silently
    // collapsed slot-resolution errors (typo, missing slot file) into
    // an empty α table — operator who passed `--slot foo` saw default-
    // slot α overrides with the only signal being different search
    // results from what they asked for.
    let slot_alpha_table = match cqs::slot::resolve_slot_name(cli.slot.as_deref(), project_cqs_dir)
    {
        Ok(resolved) => cqs::slot::read_slot_splade_alpha_table(project_cqs_dir, &resolved.name),
        Err(e) => {
            tracing::warn!(
                error = %e,
                slot = ?cli.slot,
                "Slot resolution failed when looking up SPLADE α overrides — \
                 falling back to env / preset / default α precedence"
            );
            Default::default()
        }
    };
    cqs::search::router::install_slot_splade_alpha_overrides(slot_alpha_table);

    // EXT-V1.36-1 (#1460): load synonym overlay from
    // `~/.config/cqs/synonyms.toml` (user-global) and
    // `<project>/.cqs/synonyms.toml` (project-local), with project-local
    // taking precedence on conflict. Empty / missing / malformed files
    // fall through to the compile-time builtins. Done once at dispatch
    // entry so every FTS-expanded search benefits without per-call I/O.
    {
        let mut overlay: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        if let Some(global) = dirs::config_dir().map(|d| d.join("cqs/synonyms.toml")) {
            for (k, v) in cqs::search::synonyms::load_synonym_overlay(&global) {
                overlay.insert(k, v);
            }
        }
        let project_local = project_cqs_dir.join("synonyms.toml");
        for (k, v) in cqs::search::synonyms::load_synonym_overlay(&project_local) {
            // project-local wins on key conflict (overwrites the
            // user-global entry).
            overlay.insert(k, v);
        }
        cqs::search::synonyms::install_synonym_overlay(overlay);
    }

    // EXT-V1.36-8 sub-2 (#1460): load classifier vocab overlay from
    // `~/.config/cqs/classifier.toml` (user-global) and
    // `<project>/.cqs/classifier.toml` (project-local). Same precedence
    // shape as the synonym overlay above — user-global plus project-local
    // appended; AhoCorasick rebuilt once with the merged set.
    {
        let mut neg: Vec<String> = Vec::new();
        let mut multi: Vec<String> = Vec::new();
        if let Some(global) = dirs::config_dir().map(|d| d.join("cqs/classifier.toml")) {
            let (g_neg, g_multi) = cqs::search::router::load_classifier_vocab_overlay(&global);
            neg.extend(g_neg);
            multi.extend(g_multi);
        }
        let project_local = project_cqs_dir.join("classifier.toml");
        let (p_neg, p_multi) = cqs::search::router::load_classifier_vocab_overlay(&project_local);
        neg.extend(p_neg);
        multi.extend(p_multi);
        cqs::search::router::install_classifier_vocab_overlay(neg, multi);
    }

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

    // ── Group A + Group B dispatch ──────────────────────────────────────────
    //
    // #1366: per-variant `#[cqs_cmd(group, batch)]` attributes drive
    // `cqs_macros::CqsCommands` derive on `Commands` (definitions.rs:337),
    // which emits `dispatch_group_a` and `dispatch_group_b` free functions.
    // Each shim (in `commands/dispatch_shims.rs`) destructures the variant
    // and forwards to the existing handler with the same args the
    // for_each_command! body used to bind.
    //
    // Group A returns `ControlFlow::Break(result)` when handled (lifecycle /
    // mutation commands run before the read-only store opens) and
    // `Continue(())` for Group B variants + the bare-query path (handled
    // after the store open).
    //
    // Task #8 — `--json` precedence: every Group B subcommand reads
    // `cli.json || output.json` (OR semantics). Top-level `--json` wins
    // when set; the subcommand's `--json` is the fallback. For the
    // impact/trace pair (`OutputArgs` with `--format`), `cli.json`
    // short-circuits to `OutputFormat::Json` regardless of `--format`. The
    // exact wiring lives per-shim in `commands/dispatch_shims.rs`.
    use crate::cli::definitions::{dispatch_group_a, dispatch_group_b};
    if let std::ops::ControlFlow::Break(result) = dispatch_group_a(&cli, project_cqs_dir) {
        return result;
    }

    let ctx = crate::cli::CommandContext::open_readonly(&cli)?;
    dispatch_group_b(&cli, &ctx, project_cqs_dir)
}

/// Generate shell completion scripts for the specified shell
pub(crate) fn cmd_completions(shell: clap_complete::Shell) {
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
