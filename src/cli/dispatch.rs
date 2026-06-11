//! Command dispatch: matches parsed CLI subcommands to handler functions.

use anyhow::Result;

use super::config::{apply_config_defaults, find_project_root};
#[cfg(unix)]
use super::definitions::BatchSupport;
use super::definitions::Cli;
use super::telemetry;

// Group A and Group B dispatch live in
// `crate::cli::definitions::dispatch_group_{a,b}` (emitted by
// `cqs_macros::CqsCommands` derive on the `Commands` enum). Per-variant
// shims live in `commands::dispatch_shims` and forward to existing
// handlers — see the docstring on that module for the standardized
// dispatch-shim signature. Per-variant `#[cqs_cmd(group, batch)]`
// attributes drive the routing.

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
    // Root span so all per-command logs have a parent.
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

    // Wire the [scoring] config section to the RRF K override so a user
    // writing `[scoring] rrf_k = 40` in `.cqs.toml` is honored.
    if let Some(ref scoring) = config.scoring {
        cqs::store::set_rrf_k_from_config(scoring);
    }

    // Resolve embedding model config once. Priority:
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
        // Surface slot-resolution failures via tracing instead of `.ok()`
        // swallowing them. A bad slot pointer or read error here means the
        // persisted model intent gets silently ignored — the operator sees the
        // wrong model resolve and has zero observability on why.
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

    // Load per-slot SPLADE α overrides from `slot.toml [splade.alpha]` and
    // install them on the search router. Done once at dispatch entry so every
    // search/eval/batch path benefits without per-call I/O.
    //
    // Resolution mirrors the slot-model lookup above: pick the active slot
    // (`--slot` > CQS_SLOT > active_slot file > "default"), read the alpha
    // table, install. Slot-resolution failure (missing project, malformed
    // slot.toml) → empty table → router falls through to env / preset /
    // default precedence. A slot-resolution error is surfaced via tracing
    // rather than silently collapsed into an empty α table — otherwise an
    // operator who passed `--slot foo` would see default-slot α overrides with
    // the only signal being different search results from what they asked for.
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

    // Load synonym overlay from `~/.config/cqs/synonyms.toml` (user-global)
    // and `<project>/.cqs/synonyms.toml` (project-local), with project-local
    // taking precedence on conflict. Empty / missing / malformed files fall
    // through to the compile-time builtins. Done once at dispatch entry so
    // every FTS-expanded search benefits without per-call I/O.
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

    // Load classifier vocab overlay from `~/.config/cqs/classifier.toml`
    // (user-global) and `<project>/.cqs/classifier.toml` (project-local). Same
    // precedence shape as the synonym overlay above — user-global plus
    // project-local appended; AhoCorasick rebuilt once with the merged set.
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

    // Install reranker pool-sizing overrides from `[reranker]` TOML so
    // cli/limits.rs::rerank_pool_max + rerank_over_retrieval_multiplier honor
    // the durable config. Env vars still win — see those helpers for the
    // precedence chain.
    {
        let (pool_max, over_retrieval) = config
            .reranker
            .as_ref()
            .map(|s| (s.pool_max, s.over_retrieval))
            .unwrap_or((None, None));
        crate::cli::limits::install_reranker_pool_overrides(pool_max, over_retrieval);
    }

    // Clamp limit to prevent usize::MAX wrapping to -1 in SQLite queries.
    // Same cap as the daemon batch handler — see `limits::SEARCH_LIMIT_CAP`.
    cli.limit = cli.limit.clamp(1, crate::cli::limits::SEARCH_LIMIT_CAP);

    // ── Daemon client: forward to running daemon if available ──────────────
    // The daemon binds to whichever slot was active at *its* startup (per
    // spec). If the user requested a slot — `--slot <name>` flag OR the
    // documented-equivalent `CQS_SLOT` env var (honored by
    // `slot::resolve_slot_name`) — bypass the daemon so the requested slot
    // wins instead of silently getting the daemon's startup slot. Note the
    // flag is propagated into `CQS_SLOT` above, so the env check covers both.
    #[cfg(unix)]
    if cli.slot.is_none()
        && !cqs_slot_env_pins_slot()
        && std::env::var("CQS_NO_DAEMON").as_deref() != Ok("1")
    {
        // Daemon protocol errors surface as `Err`. Transport-level failures
        // return `Ok(None)` so CLI fallback works for those.
        if let Some(output) = try_daemon_query(project_cqs_dir, &cli)? {
            print!("{}", output);
            return Ok(());
        }
    }

    // ── Group A + Group B dispatch ──────────────────────────────────────────
    //
    // Per-variant `#[cqs_cmd(group, batch)]` attributes drive
    // `cqs_macros::CqsCommands` derive on `Commands` (definitions.rs:337),
    // which emits `dispatch_group_a` and `dispatch_group_b` free functions.
    // Each shim (in `commands/dispatch_shims.rs`) destructures the variant
    // and forwards to the existing handler.
    //
    // Group A returns `ControlFlow::Break(result)` when handled (lifecycle /
    // mutation commands run before the read-only store opens) and
    // `Continue(())` for Group B variants + the bare-query path (handled
    // after the store open).
    //
    // `--json` precedence: every Group B subcommand reads
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

/// `true` when the `CQS_SLOT` env var pins a slot — i.e. it is set to a
/// non-empty (post-trim) value. Mirrors the semantics of
/// `slot::resolve_slot_name`, which trims and treats empty/whitespace (and
/// non-UTF-8) as UNSET: `CQS_SLOT= cqs …` — a script clearing the var — must
/// keep the daemon fast path, not silently bypass it.
#[cfg(unix)]
fn cqs_slot_env_pins_slot() -> bool {
    std::env::var("CQS_SLOT")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Top-level `Cli` arg IDs (clap IDs = struct field names) that configure the
/// CLI *process* — output shaping, logging, model/slot selection — rather
/// than the search. On daemon forwarding these are stripped from bare-query
/// argv; every other top-level flag is a search knob mirrored by
/// `args::SearchArgs` and forwards verbatim.
///
/// Kept in lock-step with [`SEARCH_KNOB_ARG_IDS`] by
/// `every_top_level_cli_arg_is_classified_for_daemon_translation` — adding a
/// top-level flag without classifying it here or there fails that test
/// instead of failing daemon-up at runtime.
#[cfg(unix)]
const PROCESS_LOCAL_ARG_IDS: &[&str] = &["json", "quiet", "model", "slot", "verbose"];

/// Top-level `Cli` arg IDs that are search knobs, mirrored spelling-for-
/// spelling by `args::SearchArgs` (plus `LimitArg` for `limit`). Forwarded
/// verbatim to the batch `search` parser on bare-query daemon dispatch.
#[cfg(unix)]
#[allow(
    dead_code,
    reason = "consumed by the classification tests below — the forwarded set is \
              'everything not process-local', so production only needs the strip list"
)]
const SEARCH_KNOB_ARG_IDS: &[&str] = &[
    "limit",
    "threshold",
    "name_boost",
    "lang",
    "include_type",
    "exclude_type",
    "path",
    "pattern",
    "name_only",
    "rrf",
    "include_docs",
    "reranker",
    "splade",
    "splade_alpha",
    "no_content",
    "context",
    "expand_parent",
    "ref_name",
    "include_refs",
    "tokens",
    "no_stale_check",
    "no_demote",
];

/// Build the [`cqs::daemon_translate::CliArgSpec`] from the live clap
/// definition. Derived at runtime (like `telemetry::describe_command`) so a
/// new top-level flag is classified automatically — hand-mirrored flag lists
/// are how `-v <cmd>` / `--rrf <cmd>` came to hard-error daemon-up while
/// working daemon-down.
#[cfg(unix)]
fn cli_arg_spec() -> cqs::daemon_translate::CliArgSpec {
    use clap::CommandFactory;
    cqs::daemon_translate::CliArgSpec::from_clap(&Cli::command(), PROCESS_LOCAL_ARG_IDS)
}

/// Try to forward the current command to a running daemon.
///
/// Returns:
/// - `Ok(Some(output))` — daemon handled the request successfully.
/// - `Ok(None)` — daemon not present, transport failed, or command isn't
///   daemon-dispatchable (index/watch/gc/init/etc.); CLI should run inline.
/// - `Err(_)` — daemon understood the request but returned a protocol-level
///   error. Surfaced as a real error rather than warn-and-retry, since CLI
///   fallback can produce different results from the daemon path.
#[cfg(unix)]
fn try_daemon_query(cqs_dir: &std::path::Path, cli: &Cli) -> Result<Option<String>, anyhow::Error> {
    // Root span so every failed-transport fallback is traceable.
    // Commands doesn't derive Debug so we log the discriminant name instead.
    let cmd_label = cli
        .command
        .as_ref()
        .map(|c| c.variant_name())
        .unwrap_or("search");
    let _span = tracing::debug_span!("try_daemon_query", cmd = cmd_label).entered();

    // Every `Commands` variant classifies itself via `batch_support()`; the
    // match there is exhaustive, so adding a new CLI command forces an explicit
    // daemon-forwarding decision at compile time. No surface change can
    // silently flip a command's daemon behavior.
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

    // From here on the socket file exists, so transport failures are
    // anomalous (wedged or crashed daemon) — they log at warn so the default
    // `cqs=info` filter surfaces why a 3 ms daemon query silently became a
    // multi-second CLI cold start. Only the socket-absent path above stays
    // quiet: that's the normal no-daemon case.
    const FALLBACK_HINT: &str = "daemon unresponsive — falling back to CLI; \
         check `systemctl --user status cqs-watch` (or set CQS_NO_DAEMON=1 to silence)";

    let stream = match UnixStream::connect(&sock_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                path = %sock_path.display(),
                error = %e,
                stage = "connect",
                "{FALLBACK_HINT}"
            );
            return Ok(None);
        }
    };

    // Single timeout knob across client and daemon: both sides resolve through
    // the shared `cqs::daemon_translate::resolve_daemon_timeout_ms` helper.
    let timeout = cqs::daemon_translate::resolve_daemon_timeout_ms();
    // Explicit warn on timeout failures rather than silent `.ok()` — without a
    // timeout the CLI could hang forever on a wedged daemon read.
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

    // Arg-stripping and `-n`→`--limit` remap live in
    // `cqs::daemon_translate::translate_cli_args_to_batch`, a pure helper in
    // the library crate. Integration tests pin its behaviour separately
    // (tests/daemon_forward_test.rs). The caller owns side effects: emitting
    // the `--model ignored` warning and framing the JSON request.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    // `--model` is stripped because the daemon runs a single loaded model.
    // Surface the mismatch to the user rather than silently ignoring the flag.
    if let Some(m) = cqs::daemon_translate::stripped_model_value(&raw_args) {
        tracing::warn!(
            requested_model = %m,
            "Daemon ignores --model; query will run against daemon's loaded model. \
             Set CQS_NO_DAEMON=1 to force CLI mode with the requested model."
        );
    }
    let (command, cmd_args) = cqs::daemon_translate::translate_cli_args_to_batch(
        &raw_args,
        cli.command.is_some(),
        &cli_arg_spec(),
    );
    let request = serde_json::json!({
        "command": command,
        "args": cmd_args,
    });

    let mut stream = stream;
    if let Err(e) = writeln!(stream, "{}", request) {
        tracing::warn!(error = %e, stage = "write", "{FALLBACK_HINT}");
        return Ok(None);
    }
    if let Err(e) = stream.flush() {
        tracing::warn!(error = %e, stage = "flush", "{FALLBACK_HINT}");
        return Ok(None);
    }

    // Bound the response so a rogue/buggy daemon can't force us to allocate
    // unbounded memory on `read_line`. 16 MiB matches the practical ceiling for
    // gather/task JSON outputs. Env-overridable via
    // CQS_DAEMON_MAX_RESPONSE_BYTES so large gather/task outputs on big corpora
    // can lift the cap.
    let max_daemon_response = crate::cli::limits::max_daemon_response_bytes();
    use std::io::Read as _;
    let mut reader = std::io::BufReader::new(&stream).take(max_daemon_response);
    let mut response_line = String::new();
    let bytes_read = match reader.read_line(&mut response_line) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, stage = "read", "{FALLBACK_HINT}");
            return Ok(None);
        }
    };
    if bytes_read as u64 == max_daemon_response {
        // Surface this on stderr, not just tracing — agents tuning latency
        // won't see tracing.
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
            tracing::warn!(error = %e, stage = "parse", "{FALLBACK_HINT}");
            return Ok(None);
        }
    };
    let status = match resp.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            tracing::warn!(
                stage = "parse",
                "Daemon response missing 'status' field — {FALLBACK_HINT}"
            );
            return Ok(None);
        }
    };
    if status == "ok" {
        // The daemon embeds the dispatch output as a real JSON value when the
        // bytes parse as JSON (the common case), and uses the string form for
        // plaintext handlers. Accept both shapes:
        //   - `Value::String(s)` — print verbatim (preserves original
        //     whitespace from the dispatch handler).
        //   - any other `Value` — re-serialize for the terminal print. One
        //     re-encode, no escape inflation.
        // `daemon_ping` handles both shapes for the same reason.
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
            // JSON outputs from the daemon arrive in the slim batch envelope
            // (`{"data": …}` / `{"error": …}` from `wrap_value`/`wrap_error`).
            // That envelope is the BATCH/JSONL contract — the CLI surface
            // emits a bare payload (or the full v1 envelope under
            // CQS_OUTPUT_FORMAT=v1), and its shape must not depend on whether
            // a daemon happened to serve the query. Translate before
            // printing; anything that isn't a slim envelope prints verbatim.
            other => match cqs::daemon_translate::classify_slim_envelope(other) {
                Some(cqs::daemon_translate::SlimEnvelope::Data { payload, meta }) => {
                    match crate::cli::json_envelope::daemon_payload_to_cli_text(payload, meta) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                stage = "parse",
                                "Daemon payload presentation failed — falling back to CLI"
                            );
                            return Ok(None);
                        }
                    }
                }
                Some(cqs::daemon_translate::SlimEnvelope::Error { code, message }) => {
                    // Command-level failure from the daemon: surface as a real
                    // error (non-zero exit), matching the in-process path,
                    // instead of printing an error envelope with exit 0.
                    return Err(anyhow::anyhow!(
                        "daemon error: {code}: {message}\nhint: set CQS_NO_DAEMON=1 to bypass the daemon"
                    ));
                }
                None => match serde_json::to_string_pretty(other) {
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
            },
        };
        return Ok(Some(text));
    }

    // Daemon understood the request but surfaced an error. Transport-level
    // failures (connect/read/write) already returned `Ok(None)` above, so
    // reaching here means this is a daemon protocol error the user needs to
    // see. Falling back to CLI here would mask daemon bugs and silently change
    // results — return Err so the caller exits non-zero with the daemon's
    // message.
    let msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("daemon error");
    tracing::warn!(error = msg, "Daemon returned protocol-level error");
    Err(anyhow::anyhow!(
        "daemon error: {msg}\nhint: set CQS_NO_DAEMON=1 to bypass the daemon"
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Collect every spelling (long, long aliases, short, short aliases) of a
    /// clap arg.
    fn spellings(arg: &clap::Arg) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(long) = arg.get_long() {
            out.push(format!("--{long}"));
        }
        for alias in arg.get_all_aliases().unwrap_or_default() {
            out.push(format!("--{alias}"));
        }
        if let Some(short) = arg.get_short() {
            out.push(format!("-{short}"));
        }
        for alias in arg.get_all_short_aliases().unwrap_or_default() {
            out.push(format!("-{alias}"));
        }
        out
    }

    /// Exhaustiveness pin for the daemon arg translation: every top-level
    /// `Cli` flag must be explicitly classified as process-local (stripped on
    /// daemon forwarding) or a search knob (forwarded to the batch `search`
    /// parser on bare queries) — and never both. A new top-level flag fails
    /// here at test time instead of failing daemon-up at runtime, which is
    /// how `-v <cmd>` / `--rrf <cmd>` regressed.
    #[test]
    fn every_top_level_cli_arg_is_classified_for_daemon_translation() {
        let app = Cli::command();
        for arg in app.get_arguments() {
            if arg.is_positional() {
                continue; // `query` — the bare-query payload itself.
            }
            if matches!(
                arg.get_action(),
                clap::ArgAction::Help
                    | clap::ArgAction::HelpShort
                    | clap::ArgAction::HelpLong
                    | clap::ArgAction::Version
            ) {
                continue; // clap-handled before dispatch ever runs.
            }
            let id = arg.get_id().as_str();
            let local = PROCESS_LOCAL_ARG_IDS.contains(&id);
            let knob = SEARCH_KNOB_ARG_IDS.contains(&id);
            assert!(
                local ^ knob,
                "top-level Cli arg `{id}` must be classified in exactly one of \
                 PROCESS_LOCAL_ARG_IDS / SEARCH_KNOB_ARG_IDS (local={local}, knob={knob}); \
                 unclassified flags break daemon/CLI parity"
            );
        }
    }

    /// Every search-knob spelling the bare-query path forwards must be
    /// accepted by the batch `search` parser — otherwise a daemon-up bare
    /// query with that flag returns a parse error while daemon-down works.
    #[test]
    fn forwarded_search_knob_spellings_are_accepted_by_batch_search() {
        let cli_app = Cli::command();
        let batch_app = crate::cli::batch::BatchInput::command();
        let search = batch_app
            .find_subcommand("search")
            .expect("batch parser must have a `search` subcommand");
        let accepted: std::collections::BTreeSet<String> =
            search.get_arguments().flat_map(|a| spellings(a)).collect();

        for arg in cli_app.get_arguments() {
            let id = arg.get_id().as_str();
            if !SEARCH_KNOB_ARG_IDS.contains(&id) {
                continue;
            }
            for spelling in spellings(arg) {
                assert!(
                    accepted.contains(&spelling),
                    "top-level search knob `{id}` spelling `{spelling}` is forwarded to the \
                     daemon but the batch `search` parser does not accept it"
                );
            }
        }
    }

    /// The derived spec marks value-taking flags correctly: `--model` /
    /// `--slot` / `-n` consume a value; `--json` / `-v` / `--rrf` don't.
    #[test]
    fn derived_spec_value_flag_classification() {
        let spec = cli_arg_spec();
        for flag in ["--model", "--slot", "-n", "--limit", "-t", "--tokens"] {
            assert!(
                spec.value_flags.contains(flag),
                "`{flag}` must be classified as a value flag"
            );
        }
        for flag in ["--json", "-q", "-v", "--rrf", "--name-only"] {
            assert!(
                !spec.value_flags.contains(flag),
                "`{flag}` must not be classified as a value flag"
            );
        }
        for flag in [
            "--json",
            "-q",
            "--quiet",
            "-v",
            "--verbose",
            "--model",
            "--slot",
        ] {
            assert!(
                spec.bare_query_strip.contains(flag),
                "`{flag}` is process-local and must be stripped on bare queries"
            );
        }
        assert!(
            !spec.bare_query_strip.contains("--rrf"),
            "`--rrf` is a search knob and must forward on bare queries"
        );
    }
}
