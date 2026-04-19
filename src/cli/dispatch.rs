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
    cmd_explain, cmd_export_model, cmd_gather, cmd_gc, cmd_health, cmd_impact, cmd_impact_diff,
    cmd_index, cmd_init, cmd_model, cmd_neighbors, cmd_notes, cmd_onboard, cmd_ping, cmd_plan,
    cmd_project, cmd_query, cmd_read, cmd_reconstruct, cmd_ref, cmd_related, cmd_review, cmd_scout,
    cmd_similar, cmd_stale, cmd_stats, cmd_suggest, cmd_task, cmd_telemetry, cmd_telemetry_reset,
    cmd_test_map, cmd_trace, cmd_train_data, cmd_train_pairs, cmd_where,
};

/// Run CLI with pre-parsed arguments (used when main.rs needs to inspect args first)
pub fn run_with(mut cli: Cli) -> Result<()> {
    // Log command for telemetry (opt-in via CQS_TELEMETRY=1)
    let cqs_dir = cqs::resolve_index_dir(&find_project_root());
    let telem_args: Vec<String> = std::env::args().collect();
    let (telem_cmd, telem_query) = telemetry::describe_command(&telem_args);
    telemetry::log_command(&cqs_dir, &telem_cmd, telem_query.as_deref(), None);

    // v1.22.0 audit OB-14: root span so all per-command logs have a parent.
    let _root = tracing::info_span!("cqs", cmd = %telem_cmd).entered();

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

    // Resolve embedding model config once (CLI > env > config > default),
    // then apply env var overrides (CQS_MAX_SEQ_LENGTH, CQS_EMBEDDING_DIM)
    cli.resolved_model = Some(
        cqs::embedder::ModelConfig::resolve(cli.model.as_deref(), config.embedding.as_ref())
            .apply_env_overrides(),
    );

    // Clamp limit to prevent usize::MAX wrapping to -1 in SQLite queries
    cli.limit = cli.limit.clamp(1, 100);

    // ── Daemon client: forward to running daemon if available ──────────────
    #[cfg(unix)]
    if std::env::var("CQS_NO_DAEMON").as_deref() != Ok("1") {
        if let Some(output) = try_daemon_query(&cqs_dir, &cli) {
            print!("{}", output);
            return Ok(());
        }
    }

    // ── Group A: no-store commands (early return before CommandContext) ──────
    match cli.command {
        Some(Commands::Init) => return cmd_init(&cli),
        Some(Commands::Cache { ref subcmd }) => return cmd_cache(&cli, subcmd),
        Some(Commands::Doctor { fix, verbose, json }) => {
            // Task #8: top-level `--json` cascades into doctor's `--json` so
            // `cqs --json doctor --verbose` emits JSON.
            return cmd_doctor(cli.model.as_deref(), fix, verbose, cli.json || json);
        }
        // Task B2: ping does direct socket I/O via cqs::daemon_translate::
        // daemon_ping. Must NOT open a Store (works on fresh projects pre-
        // `cqs init`). Exits 1 if no daemon is running so health-monitor
        // scripts can act on the result.
        Some(Commands::Ping { json }) => return cmd_ping(cli.json || json),
        Some(Commands::Index { ref args }) => return cmd_index(&cli, args),
        Some(Commands::Watch {
            debounce,
            no_ignore,
            poll,
            serve,
        }) => return watch::cmd_watch(&cli, debounce, no_ignore, poll, serve),
        Some(Commands::Batch) => return batch::cmd_batch(),
        Some(Commands::Chat) => return chat::cmd_chat(),
        Some(Commands::Completions { shell }) => {
            cmd_completions(shell);
            return Ok(());
        }
        Some(Commands::TrainData {
            repos,
            output,
            max_commits,
            min_msg_len,
            max_files,
            dedup_cap,
            resume,
            verbose,
        }) => {
            return cmd_train_data(cqs::train_data::TrainDataConfig {
                repos,
                output,
                // API-V1.22-13: CLI surface uses `Option<usize>` (None =
                // unlimited). The library's TrainDataConfig still uses `usize`
                // with `0` as the "no cap" sentinel — translate at the
                // dispatch boundary to keep the lib API stable.
                max_commits: max_commits.unwrap_or(0),
                min_msg_len,
                max_files,
                dedup_cap,
                resume,
                verbose,
            });
        }
        Some(Commands::ExportModel {
            ref repo,
            ref output,
            dim,
        }) => return cmd_export_model(repo, output, dim),
        #[cfg(feature = "convert")]
        Some(Commands::Convert {
            ref path,
            ref output,
            overwrite,
            dry_run,
            ref clean_tags,
        }) => {
            return cmd_convert(
                path,
                output.as_deref(),
                overwrite,
                dry_run,
                clean_tags.as_deref(),
            )
        }
        Some(Commands::Telemetry {
            reset,
            ref reason,
            all,
            ref output,
        }) => {
            return if reset {
                cmd_telemetry_reset(&cqs_dir, reason.as_deref())
            } else {
                cmd_telemetry(&cqs_dir, cli.json || output.json, all)
            }
        }
        Some(Commands::Project { ref subcmd }) => {
            return cmd_project(&cli, subcmd, cli.try_model_config()?)
        }
        // Model: each subcommand opens its own Store at known paths
        // (`cqs model show/list` open readonly; `swap` orchestrates a backup
        // + reindex). None fit through CommandContext because `swap` deletes
        // the open store under it.
        Some(Commands::Model { ref subcmd }) => return cmd_model(&cli, subcmd),
        // Special: open stores on arbitrary paths, not via CommandContext
        Some(Commands::Diff {
            ref args,
            ref output,
        }) => {
            return cmd_diff(
                &args.source,
                args.target.as_deref(),
                args.threshold,
                args.lang.as_deref(),
                cli.json || output.json,
            )
        }
        Some(Commands::Drift {
            ref args,
            ref output,
        }) => {
            return cmd_drift(
                &args.reference,
                args.threshold,
                args.min_drift,
                args.lang.as_deref(),
                args.limit,
                cli.json || output.json,
            )
        }
        Some(Commands::Ref { ref subcmd }) => return cmd_ref(&cli, subcmd),
        // Special: uses read-write CommandContext::open_readwrite()
        Some(Commands::Gc { ref output }) => return cmd_gc(&cli, cli.json || output.json),
        // AuditMode doesn't use a store — uses find_project_root + resolve_index_dir
        Some(Commands::AuditMode {
            ref state,
            ref expires,
            ref output,
        }) => return cmd_audit_mode(state.as_ref(), expires, cli.json || output.json),
        // Notes: opening the readonly store is optional — mutations
        // (`add`/`update`/`remove`) must work on a fresh project before any
        // `cqs init && cqs index` has run (so a user can capture notes from
        // the first minute). `cmd_notes` only requires the store for `list`,
        // which it enforces internally. This replaces the old split between
        // `cmd_notes` and `cmd_notes_mutate` (issue #959): one handler, with
        // the store lifecycle decided here instead of via a routing gate.
        Some(Commands::Notes { ref subcmd }) => {
            // SEC-D.9: don't silently collapse distinct store-open failures
            // (schema corruption, dim mismatch, permission denied, missing
            // index) into a single clueless "Index not found" downstream.
            // Mutations (`add`/`update`/`remove`) work without an open store
            // so we still fall through with `None`, but we leave a debug
            // breadcrumb so an operator running with `RUST_LOG=cqs=debug`
            // sees the underlying cause.
            let ctx = match crate::cli::CommandContext::open_readonly(&cli) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "Notes: readonly store open failed; mutations will use write-only path"
                    );
                    None
                }
            };
            return cmd_notes(&cli, ctx.as_ref(), subcmd);
        }
        _ => {} // Fall through to Group B
    }

    // ── Group B: store-using commands ───────────────────────────────────────
    //
    // Task #8 — `--json` precedence: every subcommand reads
    // `cli.json || output.json` (OR semantics). Top-level `--json` wins when
    // set; the subcommand's `--json` is the fallback. For the impact/trace
    // pair (`OutputArgs` with `--format`), `cli.json` short-circuits to
    // `OutputFormat::Json` regardless of `--format`. This makes
    // `cqs --json <subcmd> ...` always emit JSON without forcing the user
    // to remember whether the subcommand has its own `--json`.
    let ctx = crate::cli::CommandContext::open_readonly(&cli)?;

    match cli.command {
        Some(Commands::Affected {
            ref base,
            stdin,
            ref output,
        }) => cmd_affected(&ctx, base.as_deref(), stdin, cli.json || output.json),
        Some(Commands::Blame {
            ref args,
            ref output,
        }) => cmd_blame(
            &ctx,
            &args.name,
            args.commits,
            args.callers,
            cli.json || output.json,
        ),
        Some(Commands::Brief {
            ref path,
            ref output,
        }) => cmd_brief(&ctx, path, cli.json || output.json),
        Some(Commands::Stats { ref output }) => cmd_stats(&ctx, cli.json || output.json),
        Some(Commands::Deps {
            ref args,
            ref output,
        }) => cmd_deps(
            &ctx,
            &args.name,
            args.reverse,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        ),
        Some(Commands::Callers {
            ref args,
            ref output,
        }) => cmd_callers(
            &ctx,
            &args.name,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        ),
        Some(Commands::Callees {
            ref args,
            ref output,
        }) => cmd_callees(
            &ctx,
            &args.name,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        ),
        Some(Commands::Onboard {
            ref args,
            ref output,
        }) => cmd_onboard(
            &ctx,
            &args.query,
            args.depth,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
        ),
        Some(Commands::Neighbors {
            ref name,
            limit,
            ref output,
        }) => cmd_neighbors(&ctx, name, limit, cli.json || output.json),
        Some(Commands::Explain {
            ref args,
            ref output,
        }) => cmd_explain(
            &ctx,
            &args.name,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
        ),
        Some(Commands::Similar {
            ref args,
            ref output,
        }) => cmd_similar(
            &ctx,
            &args.name,
            args.limit,
            args.threshold,
            cli.json || output.json,
        ),
        Some(Commands::Impact {
            ref args,
            ref output,
        }) => {
            // Task #8: top-level `--json` (cli.json) overrides whatever the
            // subcommand's `--format` says. `effective_format()` already
            // honours `output.json`; we OR cli.json on top so
            // `cqs --json impact foo` works without `--json` on the subcommand.
            let format = if cli.json {
                crate::cli::OutputFormat::Json
            } else {
                output.effective_format()
            };
            cmd_impact(
                &ctx,
                &args.name,
                args.depth,
                args.limit_arg.limit,
                &format,
                args.suggest_tests,
                args.type_impact,
                args.cross_project,
            )
        }
        Some(Commands::ImpactDiff {
            ref args,
            ref output,
        }) => cmd_impact_diff(
            &ctx,
            args.base.as_deref(),
            args.stdin,
            cli.json || output.json,
        ),
        Some(Commands::Review {
            ref args,
            ref output,
        }) => {
            let format = if cli.json {
                crate::cli::OutputFormat::Json
            } else {
                output.effective_format()
            };
            cmd_review(&ctx, args.base.as_deref(), args.stdin, &format, args.tokens)
        }
        Some(Commands::Ci {
            ref args,
            ref output,
        }) => {
            let format = if cli.json {
                crate::cli::OutputFormat::Json
            } else {
                output.effective_format()
            };
            cmd_ci(
                &ctx,
                args.base.as_deref(),
                args.stdin,
                &format,
                &args.gate,
                args.tokens,
            )
        }
        Some(Commands::Trace {
            ref args,
            ref output,
        }) => {
            // Task #8: cli.json wins over the subcommand format.
            let format = if cli.json {
                crate::cli::OutputFormat::Json
            } else {
                output.effective_format()
            };
            cmd_trace(
                &ctx,
                &args.source,
                &args.target,
                args.max_depth as usize,
                args.limit_arg.limit,
                &format,
                args.cross_project,
            )
        }
        Some(Commands::TestMap {
            ref args,
            ref output,
        }) => cmd_test_map(
            &ctx,
            &args.name,
            args.depth,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        ),
        Some(Commands::Context {
            ref args,
            ref output,
        }) => cmd_context(
            &ctx,
            &args.path,
            cli.json || output.json,
            args.summary,
            args.compact,
            args.tokens,
        ),
        Some(Commands::Dead {
            ref args,
            ref output,
        }) => cmd_dead(
            &ctx,
            cli.json || output.json,
            args.include_pub,
            args.min_confidence,
        ),
        Some(Commands::Gather {
            ref args,
            ref output,
        }) => cmd_gather(&super::commands::GatherContext {
            ctx: &ctx,
            query: &args.query,
            expand: args.expand,
            direction: args.direction,
            limit: args.limit,
            max_tokens: args.tokens,
            ref_name: args.ref_name.as_deref(),
            json: cli.json || output.json,
        }),
        Some(Commands::Health { ref output }) => cmd_health(&ctx, cli.json || output.json),
        Some(Commands::Stale {
            ref args,
            ref output,
        }) => cmd_stale(&ctx, cli.json || output.json, args.count_only),
        Some(Commands::Suggest {
            ref args,
            ref output,
        }) => cmd_suggest(&ctx, cli.json || output.json, args.apply),
        Some(Commands::Read {
            ref args,
            ref output,
        }) => cmd_read(
            &ctx,
            &args.path,
            args.focus.as_deref(),
            cli.json || output.json,
        ),
        Some(Commands::Reconstruct {
            ref path,
            ref output,
        }) => cmd_reconstruct(&ctx, path, cli.json || output.json),
        Some(Commands::Related {
            ref args,
            ref output,
        }) => cmd_related(&ctx, &args.name, args.limit, cli.json || output.json),
        Some(Commands::Where {
            ref args,
            ref output,
        }) => cmd_where(&ctx, &args.description, args.limit, cli.json || output.json),
        Some(Commands::Scout {
            ref args,
            ref output,
        }) => cmd_scout(
            &ctx,
            &args.query,
            args.limit,
            cli.json || output.json,
            args.tokens,
        ),
        Some(Commands::Plan {
            ref args,
            ref output,
        }) => cmd_plan(
            &ctx,
            &args.description,
            args.limit,
            cli.json || output.json,
            args.tokens,
        ),
        Some(Commands::Task {
            ref args,
            ref output,
        }) => cmd_task(
            &ctx,
            &args.description,
            args.limit,
            cli.json || output.json,
            args.tokens,
            args.brief,
        ),
        Some(Commands::TrainPairs {
            ref output,
            limit,
            ref language,
            contrastive,
        }) => cmd_train_pairs(
            &ctx,
            output.as_path(),
            limit,
            language.as_deref(),
            contrastive,
        ),
        Some(Commands::Eval { ref args }) => cmd_eval(&ctx, args),
        None => match &cli.query {
            Some(q) => cmd_query(&ctx, q),
            None => {
                println!("Usage: cqs <query> or cqs <command>");
                println!("Run 'cqs --help' for more information.");
                Ok(())
            }
        },
        // All Group A commands were handled above with early returns
        _ => unreachable!("All Group A commands return early before CommandContext"),
    }
}

/// Generate shell completion scripts for the specified shell
fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}

/// Return a static string identifying the variant of a `Commands`.
/// Used only for tracing spans; `Commands` does not derive `Debug` to keep
/// help output clean.
#[cfg(unix)]
fn command_variant_name(cmd: &Commands) -> &'static str {
    match cmd {
        Commands::Init => "init",
        Commands::Brief { .. } => "brief",
        Commands::Doctor { .. } => "doctor",
        Commands::Index { .. } => "index",
        Commands::Stats { .. } => "stats",
        Commands::Watch { .. } => "watch",
        Commands::Affected { .. } => "affected",
        Commands::Batch => "batch",
        Commands::Blame { .. } => "blame",
        Commands::Chat => "chat",
        Commands::Completions { .. } => "completions",
        Commands::Deps { .. } => "deps",
        Commands::Callers { .. } => "callers",
        Commands::Callees { .. } => "callees",
        Commands::Onboard { .. } => "onboard",
        Commands::Neighbors { .. } => "neighbors",
        Commands::Notes { .. } => "notes",
        Commands::Ref { .. } => "ref",
        Commands::Diff { .. } => "diff",
        Commands::Drift { .. } => "drift",
        Commands::Explain { .. } => "explain",
        Commands::Similar { .. } => "similar",
        Commands::Impact { .. } => "impact",
        Commands::ImpactDiff { .. } => "impact-diff",
        Commands::Review { .. } => "review",
        Commands::Ci { .. } => "ci",
        Commands::Trace { .. } => "trace",
        Commands::TestMap { .. } => "test-map",
        Commands::Context { .. } => "context",
        Commands::Dead { .. } => "dead",
        Commands::Gather { .. } => "gather",
        Commands::Project { .. } => "project",
        Commands::Gc { .. } => "gc",
        Commands::Health { .. } => "health",
        Commands::AuditMode { .. } => "audit-mode",
        Commands::Telemetry { .. } => "telemetry",
        Commands::Stale { .. } => "stale",
        Commands::Suggest { .. } => "suggest",
        Commands::Read { .. } => "read",
        Commands::Reconstruct { .. } => "reconstruct",
        Commands::Related { .. } => "related",
        Commands::Where { .. } => "where",
        Commands::Scout { .. } => "scout",
        Commands::Plan { .. } => "plan",
        Commands::Task { .. } => "task",
        #[cfg(feature = "convert")]
        Commands::Convert { .. } => "convert",
        Commands::ExportModel { .. } => "export-model",
        Commands::TrainData { .. } => "train-data",
        Commands::TrainPairs { .. } => "train-pairs",
        Commands::Cache { .. } => "cache",
        Commands::Ping { .. } => "ping",
        Commands::Eval { .. } => "eval",
        Commands::Model { .. } => "model",
    }
}

/// Try to forward the current command to a running daemon.
/// Returns `Some(output)` if the daemon handled it, `None` if no daemon or
/// the command is not daemon-dispatchable (index, watch, gc, init, etc.).
#[cfg(unix)]
fn try_daemon_query(cqs_dir: &std::path::Path, cli: &Cli) -> Option<String> {
    // OB-NEW-5: root span so every failed-transport fallback is traceable.
    // Commands doesn't derive Debug so we log the discriminant name instead.
    let cmd_label = cli
        .command
        .as_ref()
        .map(|c| command_variant_name(c))
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
            return None;
        }
    }

    let sock_path = super::daemon_socket_path(cqs_dir);
    if !sock_path.exists() {
        return None;
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
            return None;
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
        return None;
    }
    if let Err(e) = stream.flush() {
        tracing::debug!(error = %e, stage = "flush", "Daemon transport failed, falling back to CLI");
        return None;
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
            return None;
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
        return None;
    }

    let resp: serde_json::Value = match serde_json::from_str(response_line.trim()) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                error = %e,
                stage = "parse",
                "Daemon transport failed, falling back to CLI"
            );
            return None;
        }
    };
    let status = match resp.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            tracing::debug!(
                stage = "parse",
                "Daemon response missing 'status' field, falling back to CLI"
            );
            return None;
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
        let output = resp.get("output")?;
        let text = match output {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string(other).ok()?,
        };
        return Some(text);
    }

    // EH-13: daemon understood the request but surfaced an error. Transport-level
    // failures (connect/read/write) already returned `None` above, so reaching
    // here means this is a daemon protocol error the user needs to see.
    // Falling back to CLI now would mask daemon bugs — tell the user and
    // suggest the CLI override if they want to retry outside the daemon.
    let msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("daemon error");
    tracing::warn!(error = msg, "Daemon returned protocol-level error");
    eprintln!("cqs: daemon error: {msg}");
    eprintln!(
        "hint: set CQS_NO_DAEMON=1 to run the command directly in the CLI (bypasses the daemon)."
    );
    // Still return None so we fall through to CLI path, but the user has been
    // told why — no silent fallback.
    None
}
