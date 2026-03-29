//! Command dispatch: matches parsed CLI subcommands to handler functions.

use anyhow::Result;

use super::config::{apply_config_defaults, find_project_root};
use super::definitions::{Cli, Commands};
use super::telemetry;
use super::{batch, chat, watch};

#[cfg(feature = "convert")]
use super::commands::cmd_convert;
use super::commands::{
    cmd_audit_mode, cmd_blame, cmd_callees, cmd_callers, cmd_ci, cmd_context, cmd_dead, cmd_deps,
    cmd_diff, cmd_doctor, cmd_drift, cmd_explain, cmd_export_model, cmd_gather, cmd_gc, cmd_health,
    cmd_impact, cmd_impact_diff, cmd_index, cmd_init, cmd_notes, cmd_onboard, cmd_plan,
    cmd_project, cmd_query, cmd_read, cmd_ref, cmd_related, cmd_review, cmd_scout, cmd_similar,
    cmd_stale, cmd_stats, cmd_suggest, cmd_task, cmd_test_map, cmd_trace, cmd_train_data,
    cmd_where,
};

/// Run CLI with pre-parsed arguments (used when main.rs needs to inspect args first)
pub fn run_with(mut cli: Cli) -> Result<()> {
    // Log command for telemetry (opt-in via CQS_TELEMETRY=1)
    let cqs_dir = cqs::resolve_index_dir(&find_project_root());
    let telem_args: Vec<String> = std::env::args().collect();
    let (telem_cmd, telem_query) = telemetry::describe_command(&telem_args);
    telemetry::log_command(&cqs_dir, &telem_cmd, telem_query.as_deref(), None);

    // Load config and apply defaults (CLI flags override config)
    let config = cqs::config::Config::load(&find_project_root());
    apply_config_defaults(&mut cli, &config);

    // Resolve embedding model config once (CLI > env > config > default)
    cli.resolved_model = Some(cqs::embedder::ModelConfig::resolve(
        cli.model.as_deref(),
        config.embedding.as_ref(),
    ));

    // Clamp limit to prevent usize::MAX wrapping to -1 in SQLite queries
    cli.limit = cli.limit.clamp(1, 100);

    match cli.command {
        Some(Commands::Batch) => batch::cmd_batch(),
        Some(Commands::Blame {
            ref name,
            depth,
            callers,
            json,
        }) => cmd_blame(name, json, depth, callers),
        Some(Commands::Chat) => chat::cmd_chat(),
        Some(Commands::Init) => cmd_init(&cli),
        Some(Commands::Doctor) => cmd_doctor(cli.model.as_deref()),
        Some(Commands::Index { ref args }) => cmd_index(&cli, args),
        Some(Commands::Stats { json }) => cmd_stats(&cli, json),
        Some(Commands::Watch {
            debounce,
            no_ignore,
            poll,
        }) => watch::cmd_watch(&cli, debounce, no_ignore, poll),
        Some(Commands::Completions { shell }) => {
            cmd_completions(shell);
            Ok(())
        }
        Some(Commands::Deps {
            ref name,
            reverse,
            json,
        }) => cmd_deps(name, reverse, json),
        Some(Commands::Callers { ref name, json }) => cmd_callers(name, json),
        Some(Commands::Callees { ref name, json }) => cmd_callees(name, json),
        Some(Commands::Onboard {
            ref query,
            depth,
            json,
            tokens,
        }) => cmd_onboard(&cli, query, depth, json, tokens),
        Some(Commands::Notes { ref subcmd }) => cmd_notes(&cli, subcmd),
        Some(Commands::Ref { ref subcmd }) => cmd_ref(&cli, subcmd),
        Some(Commands::Diff {
            ref source,
            ref target,
            threshold,
            ref lang,
            json,
        }) => cmd_diff(source, target.as_deref(), threshold, lang.as_deref(), json),
        Some(Commands::Drift {
            ref reference,
            threshold,
            min_drift,
            ref lang,
            limit,
            json,
        }) => cmd_drift(
            reference,
            threshold,
            min_drift,
            lang.as_deref(),
            limit,
            json,
        ),
        Some(Commands::Explain {
            ref name,
            json,
            tokens,
        }) => cmd_explain(&cli, name, json, tokens),
        Some(Commands::Similar {
            ref target,
            limit,
            threshold,
            json,
        }) => cmd_similar(&cli, target, limit, threshold, json),
        Some(Commands::Impact {
            ref args,
            ref output,
        }) => {
            let format = output.effective_format();
            cmd_impact(
                &args.name,
                args.depth,
                &format,
                args.suggest_tests,
                args.include_types,
            )
        }
        Some(Commands::ImpactDiff {
            ref base,
            stdin,
            json,
        }) => cmd_impact_diff(&cli, base.as_deref(), stdin, json),
        Some(Commands::Review {
            ref base,
            stdin,
            ref output,
            tokens,
        }) => {
            let format = output.effective_format();
            cmd_review(base.as_deref(), stdin, &format, tokens)
        }
        Some(Commands::Ci {
            ref base,
            stdin,
            ref output,
            ref gate,
            tokens,
        }) => {
            let format = output.effective_format();
            cmd_ci(base.as_deref(), stdin, &format, gate, tokens)
        }
        Some(Commands::Trace {
            ref source,
            ref target,
            max_depth,
            ref output,
        }) => {
            let format = output.effective_format();
            cmd_trace(source, target, max_depth as usize, &format)
        }
        Some(Commands::TestMap {
            ref name,
            depth,
            json,
        }) => cmd_test_map(name, depth, json),
        Some(Commands::Context { ref args, json }) => cmd_context(
            &cli,
            &args.path,
            json,
            args.summary,
            args.compact,
            args.tokens,
        ),
        Some(Commands::Dead { ref args, json }) => {
            cmd_dead(&cli, json, args.include_pub, args.min_confidence)
        }
        Some(Commands::Gather { ref args, json }) => cmd_gather(
            &cli,
            &args.query,
            args.expand,
            args.direction,
            args.limit,
            args.tokens,
            args.ref_name.as_deref(),
            json,
        ),
        Some(Commands::Project { ref subcmd }) => cmd_project(subcmd, cli.model_config()),
        Some(Commands::Gc { json }) => cmd_gc(json),
        Some(Commands::Health { json }) => cmd_health(json),
        Some(Commands::AuditMode {
            ref state,
            ref expires,
            json,
        }) => cmd_audit_mode(state.as_ref(), expires, json),
        Some(Commands::Stale { json, count_only }) => cmd_stale(&cli, json, count_only),
        Some(Commands::Suggest { json, apply }) => cmd_suggest(json, apply),
        Some(Commands::Read {
            ref path,
            ref focus,
            json,
        }) => cmd_read(path, focus.as_deref(), json),
        Some(Commands::Related {
            ref name,
            limit,
            json,
        }) => cmd_related(&cli, name, limit, json),
        Some(Commands::Where {
            ref description,
            limit,
            json,
        }) => cmd_where(description, limit, json, cli.model_config()),
        Some(Commands::Scout { ref args, json }) => {
            cmd_scout(&cli, &args.query, args.limit, json, args.tokens)
        }
        Some(Commands::Plan {
            ref description,
            limit,
            json,
            tokens,
        }) => cmd_plan(&cli, description, limit, json, tokens),
        Some(Commands::Task {
            ref description,
            limit,
            json,
            tokens,
        }) => cmd_task(&cli, description, limit, json, tokens),
        #[cfg(feature = "convert")]
        Some(Commands::Convert {
            ref path,
            ref output,
            overwrite,
            dry_run,
            ref clean_tags,
        }) => cmd_convert(
            path,
            output.as_deref(),
            overwrite,
            dry_run,
            clean_tags.as_deref(),
        ),
        Some(Commands::TrainData {
            repos,
            output,
            max_commits,
            min_msg_len,
            max_files,
            dedup_cap,
            resume,
            verbose,
        }) => cmd_train_data(cqs::train_data::TrainDataConfig {
            repos,
            output,
            max_commits,
            min_msg_len,
            max_files,
            dedup_cap,
            resume,
            verbose,
        }),
        Some(Commands::ExportModel {
            ref repo,
            ref output,
        }) => cmd_export_model(repo, output),
        None => match &cli.query {
            Some(q) => cmd_query(&cli, q),
            None => {
                println!("Usage: cqs <query> or cqs <command>");
                println!("Run 'cqs --help' for more information.");
                Ok(())
            }
        },
    }
}

/// Generate shell completion scripts for the specified shell
fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}
