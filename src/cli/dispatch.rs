//! Command dispatch: matches parsed CLI subcommands to handler functions.

use anyhow::Result;

use super::config::{apply_config_defaults, find_project_root};
use super::definitions::{Cli, Commands};
use super::telemetry;
use super::{batch, chat, watch};

#[cfg(feature = "convert")]
use super::commands::cmd_convert;
use super::commands::{
    cmd_affected, cmd_audit_mode, cmd_blame, cmd_brief, cmd_callees, cmd_callers, cmd_ci,
    cmd_context, cmd_dead, cmd_deps, cmd_diff, cmd_doctor, cmd_drift, cmd_explain,
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

    // Load config and apply defaults (CLI flags override config)
    let config = cqs::config::Config::load(&find_project_root());
    apply_config_defaults(&mut cli, &config);

    // Resolve embedding model config once (CLI > env > config > default),
    // then apply env var overrides (CQS_MAX_SEQ_LENGTH, CQS_EMBEDDING_DIM)
    cli.resolved_model = Some(
        cqs::embedder::ModelConfig::resolve(cli.model.as_deref(), config.embedding.as_ref())
            .apply_env_overrides(),
    );

    // Clamp limit to prevent usize::MAX wrapping to -1 in SQLite queries
    cli.limit = cli.limit.clamp(1, 100);

    // ── Group A: no-store commands (early return before CommandContext) ──────
    match cli.command {
        Some(Commands::Init) => return cmd_init(&cli),
        Some(Commands::Doctor { fix }) => return cmd_doctor(cli.model.as_deref(), fix),
        Some(Commands::Index { ref args }) => return cmd_index(&cli, args),
        Some(Commands::Watch {
            debounce,
            no_ignore,
            poll,
        }) => return watch::cmd_watch(&cli, debounce, no_ignore, poll),
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
        Some(Commands::Project { ref subcmd }) => return cmd_project(subcmd, cli.model_config()),
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
            ref output,
        }) => cmd_deps(&ctx, name, reverse, output.json),
        Some(Commands::Callers {
            ref name,
            ref output,
        }) => cmd_callers(&ctx, name, output.json),
        Some(Commands::Callees {
            ref name,
            ref output,
        }) => cmd_callees(&ctx, name, output.json),
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
                args.include_types,
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
            )
        }
        Some(Commands::TestMap {
            ref name,
            depth,
            ref output,
        }) => cmd_test_map(&ctx, name, depth, output.json),
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
