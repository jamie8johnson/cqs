//! Command dispatch: matches parsed CLI subcommands to handler functions.

use anyhow::Result;

use super::config::{apply_config_defaults, find_project_root};
use super::definitions::{Cli, Commands};
use super::telemetry;
use super::{batch, chat, watch};

#[cfg(feature = "convert")]
use super::commands::cmd_convert;
use super::commands::{
    cmd_affected, cmd_audit_mode, cmd_blame, cmd_brief, cmd_cache, cmd_callees, cmd_callers,
    cmd_ci, cmd_context, cmd_dead, cmd_deps, cmd_diff, cmd_doctor, cmd_drift, cmd_explain,
    cmd_export_model, cmd_gather, cmd_gc, cmd_health, cmd_impact, cmd_impact_diff, cmd_index,
    cmd_init, cmd_neighbors, cmd_notes, cmd_notes_mutate, cmd_onboard, cmd_plan, cmd_project,
    cmd_query, cmd_read, cmd_reconstruct, cmd_ref, cmd_related, cmd_review, cmd_scout, cmd_similar,
    cmd_stale, cmd_stats, cmd_suggest, cmd_task, cmd_telemetry, cmd_telemetry_reset, cmd_test_map,
    cmd_trace, cmd_train_data, cmd_train_pairs, cmd_where, NotesCommand,
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
    if std::env::var("CQS_NO_DAEMON").as_deref() != Ok("1") {
        if let Some(output) = try_daemon_query(&cqs_dir, &cli) {
            print!("{}", output);
            return Ok(());
        }
    }

    // ── Group A: no-store commands (early return before CommandContext) ──────
    match cli.command {
        Some(Commands::Init) => return cmd_init(&cli),
        Some(Commands::Cache { ref subcmd }) => return cmd_cache(subcmd),
        Some(Commands::Doctor { fix }) => return cmd_doctor(cli.model.as_deref(), fix),
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
                max_commits,
                min_msg_len,
                max_files,
                dedup_cap,
                resume,
                verbose,
            })
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
                cmd_telemetry(&cqs_dir, output.json, all)
            }
        }
        Some(Commands::Project { ref subcmd }) => {
            return cmd_project(subcmd, cli.try_model_config()?)
        }
        // Special: open stores on arbitrary paths, not via CommandContext
        Some(Commands::Diff {
            ref source,
            ref target,
            threshold,
            ref lang,
            ref output,
        }) => {
            return cmd_diff(
                source,
                target.as_deref(),
                threshold,
                lang.as_deref(),
                output.json,
            )
        }
        Some(Commands::Drift {
            ref reference,
            threshold,
            min_drift,
            ref lang,
            limit,
            ref output,
        }) => {
            return cmd_drift(
                reference,
                threshold,
                min_drift,
                lang.as_deref(),
                limit,
                output.json,
            )
        }
        Some(Commands::Ref { ref subcmd }) => return cmd_ref(&cli, subcmd),
        // Special: uses read-write CommandContext::open_readwrite()
        Some(Commands::Gc { ref output }) => return cmd_gc(&cli, output.json),
        // Notes mutations open one read-write store for reindex (RM-8: avoid
        // double connection from readonly CommandContext + separate write store)
        Some(Commands::Notes { ref subcmd }) if !matches!(subcmd, NotesCommand::List { .. }) => {
            return cmd_notes_mutate(&cli, subcmd);
        }
        // AuditMode doesn't use a store — uses find_project_root + resolve_index_dir
        Some(Commands::AuditMode {
            ref state,
            ref expires,
            ref output,
        }) => return cmd_audit_mode(state.as_ref(), expires, output.json),
        _ => {} // Fall through to Group B
    }

    // ── Group B: store-using commands ───────────────────────────────────────
    let ctx = crate::cli::CommandContext::open_readonly(&cli)?;

    match cli.command {
        Some(Commands::Affected {
            ref base,
            ref output,
        }) => cmd_affected(&ctx, base.as_deref(), output.json),
        Some(Commands::Blame {
            ref args,
            ref output,
        }) => cmd_blame(&ctx, &args.name, args.depth, args.callers, output.json),
        Some(Commands::Brief {
            ref path,
            ref output,
        }) => cmd_brief(&ctx, path, output.json),
        Some(Commands::Stats { ref output }) => cmd_stats(&ctx, output.json),
        Some(Commands::Deps {
            ref name,
            reverse,
            cross_project,
            ref output,
        }) => cmd_deps(&ctx, name, reverse, cross_project, output.json),
        Some(Commands::Callers {
            ref name,
            cross_project,
            ref output,
        }) => cmd_callers(&ctx, name, cross_project, output.json),
        Some(Commands::Callees {
            ref name,
            cross_project,
            ref output,
        }) => cmd_callees(&ctx, name, cross_project, output.json),
        Some(Commands::Onboard {
            ref query,
            depth,
            ref output,
            tokens,
        }) => cmd_onboard(&ctx, query, depth, output.json, tokens),
        Some(Commands::Neighbors {
            ref name,
            limit,
            ref output,
        }) => cmd_neighbors(&ctx, name, limit, output.json),
        Some(Commands::Notes { ref subcmd }) => cmd_notes(&ctx, subcmd),
        Some(Commands::Explain {
            ref name,
            ref output,
            tokens,
        }) => cmd_explain(&ctx, name, output.json, tokens),
        Some(Commands::Similar {
            ref args,
            ref output,
        }) => cmd_similar(&ctx, &args.name, args.limit, args.threshold, output.json),
        Some(Commands::Impact {
            ref args,
            ref output,
        }) => {
            let format = output.effective_format();
            cmd_impact(
                &ctx,
                &args.name,
                args.depth,
                &format,
                args.suggest_tests,
                args.type_impact,
                args.cross_project,
            )
        }
        Some(Commands::ImpactDiff {
            ref base,
            stdin,
            ref output,
        }) => cmd_impact_diff(&ctx, base.as_deref(), stdin, output.json),
        Some(Commands::Review {
            ref base,
            stdin,
            ref output,
            tokens,
        }) => {
            let format = output.effective_format();
            cmd_review(&ctx, base.as_deref(), stdin, &format, tokens)
        }
        Some(Commands::Ci {
            ref base,
            stdin,
            ref output,
            ref gate,
            tokens,
        }) => {
            let format = output.effective_format();
            cmd_ci(&ctx, base.as_deref(), stdin, &format, gate, tokens)
        }
        Some(Commands::Trace {
            ref args,
            ref output,
        }) => {
            let format = output.effective_format();
            cmd_trace(
                &ctx,
                &args.source,
                &args.target,
                args.max_depth as usize,
                &format,
                args.cross_project,
            )
        }
        Some(Commands::TestMap {
            ref name,
            depth,
            cross_project,
            ref output,
        }) => cmd_test_map(&ctx, name, depth, cross_project, output.json),
        Some(Commands::Context {
            ref args,
            ref output,
        }) => cmd_context(
            &ctx,
            &args.path,
            output.json,
            args.summary,
            args.compact,
            args.tokens,
        ),
        Some(Commands::Dead {
            ref args,
            ref output,
        }) => cmd_dead(&ctx, output.json, args.include_pub, args.min_confidence),
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
            json: output.json,
        }),
        Some(Commands::Health { ref output }) => cmd_health(&ctx, output.json),
        Some(Commands::Stale {
            ref output,
            count_only,
        }) => cmd_stale(&ctx, output.json, count_only),
        Some(Commands::Suggest { ref output, apply }) => cmd_suggest(&ctx, output.json, apply),
        Some(Commands::Read {
            ref path,
            ref focus,
            ref output,
        }) => cmd_read(&ctx, path, focus.as_deref(), output.json),
        Some(Commands::Reconstruct {
            ref path,
            ref output,
        }) => cmd_reconstruct(&ctx, path, output.json),
        Some(Commands::Related {
            ref name,
            limit,
            ref output,
        }) => cmd_related(&ctx, name, limit, output.json),
        Some(Commands::Where {
            ref description,
            limit,
            ref output,
        }) => cmd_where(&ctx, description, limit, output.json),
        Some(Commands::Scout {
            ref args,
            ref output,
        }) => cmd_scout(&ctx, &args.query, args.limit, output.json, args.tokens),
        Some(Commands::Plan {
            ref description,
            limit,
            ref output,
            tokens,
        }) => cmd_plan(&ctx, description, limit, output.json, tokens),
        Some(Commands::Task {
            ref description,
            limit,
            ref output,
            tokens,
            brief,
        }) => cmd_task(&ctx, description, limit, output.json, tokens, brief),
        Some(Commands::TrainPairs {
            ref output,
            limit,
            ref language,
            contrastive,
        }) => cmd_train_pairs(&ctx, output, limit, language.as_deref(), contrastive),
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

/// Try to forward the current command to a running daemon.
/// Returns `Some(output)` if the daemon handled it, `None` if no daemon or
/// the command is not daemon-dispatchable (index, watch, gc, init, etc.).
fn try_daemon_query(cqs_dir: &std::path::Path, cli: &Cli) -> Option<String> {
    // Only forward commands that the batch handler can dispatch.
    // None = default search (most common invocation: `cqs "query"`)
    match &cli.command {
        Some(Commands::Init)
        | Some(Commands::Index { .. })
        | Some(Commands::Watch { .. })
        | Some(Commands::Batch)
        | Some(Commands::Chat)
        | Some(Commands::Completions { .. })
        | Some(Commands::TrainData { .. })
        | Some(Commands::TrainPairs { .. })
        | Some(Commands::Cache { .. })
        | Some(Commands::Doctor { .. }) => return None,
        None | Some(_) => {}
    }

    let sock_path = super::daemon_socket_path(cqs_dir);
    if !sock_path.exists() {
        return None;
    }

    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let stream = match UnixStream::connect(&sock_path) {
        Ok(s) => s,
        Err(_) => {
            tracing::debug!(path = %sock_path.display(), "Daemon socket exists but connect failed");
            return None;
        }
    };

    stream
        .set_read_timeout(Some(Duration::from_secs(
            std::env::var("CQS_DAEMON_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|ms| ms / 1000)
                .unwrap_or(30),
        )))
        .ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    // Build batch-format request from CLI args.
    // Strip global flags (--json, -q, --quiet, --model, etc.) — they live
    // on the top-level Cli struct, not on the subcommand. The batch handler
    // always outputs JSON, so --json is implicit.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let global_flags: &[&str] = &["--json", "-q", "--quiet"];
    let global_with_value: &[&str] = &["--model", "-n", "--limit"];
    let mut args: Vec<String> = Vec::new();
    let mut skip_next = false;
    for arg in &raw_args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if global_flags.contains(&arg.as_str()) {
            continue;
        }
        if global_with_value.contains(&arg.as_str()) {
            // Remap -n/--limit: the batch parser uses --limit on the subcommand
            if arg == "-n" || arg == "--limit" {
                args.push("--limit".to_string());
                // Next arg is the value — pass it through
                continue;
            }
            skip_next = true;
            continue;
        }
        args.push(arg.clone());
    }
    // Default search (no subcommand): `cqs "query"` → args after stripping
    // are just the query + flags. Prepend "search" so the batch parser sees it.
    let (command, cmd_args): (&str, &[String]) = if cli.command.is_none() {
        ("search", &args)
    } else if let Some((first, rest)) = args.split_first() {
        (first.as_str(), rest)
    } else {
        ("", &[])
    };
    let request = serde_json::json!({
        "command": command,
        "args": cmd_args,
    });

    let mut stream = stream;
    if writeln!(stream, "{}", request).is_err() {
        return None;
    }
    if stream.flush().is_err() {
        return None;
    }

    let mut reader = std::io::BufReader::new(&stream);
    let mut response_line = String::new();
    if reader.read_line(&mut response_line).is_err() {
        return None;
    }

    let resp: serde_json::Value = serde_json::from_str(response_line.trim()).ok()?;
    if resp.get("status")?.as_str()? == "ok" {
        Some(resp.get("output")?.as_str()?.to_string())
    } else {
        let msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("daemon error");
        tracing::warn!(error = msg, "Daemon returned error, falling back to CLI");
        None
    }
}
