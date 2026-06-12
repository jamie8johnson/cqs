//! Dispatch shims for `#[derive(CqsCommands)]`.
//!
//! Each `cmd_<variant_snake>_dispatch` is a tiny wrapper that:
//!   1. Pattern-matches the variant out of `&Commands` (proven by the
//!      derive's match arm — `unreachable!()` is genuinely unreachable).
//!   2. Calls the actual handler with destructured args + `&cli` / `ctx`.
//!
//! ## Signature
//!
//! ```text
//! pub fn cmd_xxx_dispatch(
//!     cli: &Cli,
//!     ctx: Option<&CommandContext<'_, ReadOnly>>,
//!     project_cqs_dir: &Path,
//!     cmd: &Commands,
//! ) -> anyhow::Result<()>
//! ```
//!
//! - Group A shims ignore `ctx` (always `None`).
//! - Group B shims `.expect("Group B variant requires ctx")` because the
//!   derive only ever calls them with `Some(&ctx)` after `open_readonly`.

use std::path::Path;

use anyhow::Result;

use crate::cli::commands;
use crate::cli::definitions::{Cli, Commands};
use crate::cli::CommandContext;
use cqs::store::ReadOnly;

/// Shorthand for "this dispatch shim was called with the wrong variant".
/// The derive's match arm proves this never fires; the macro-generated
/// arm pattern is `Some(c @ Commands::Foo { .. }) => cmd_foo_dispatch(...)`,
/// so `cmd` must be `Foo` when the shim runs.
macro_rules! must_be {
    ($cmd:expr, $pat:pat => $body:block) => {
        match $cmd {
            $pat => $body,
            _ => unreachable!(
                "dispatch shim called with unexpected variant: {}",
                $cmd.variant_name()
            ),
        }
    };
}

// ─── Group A — no-store / lifecycle / mutation ──────────────────────────────

pub fn cmd_init_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Init { output } => {
        commands::cmd_init(cli, cli.json || output.json)
    })
}

pub fn cmd_cache_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Cache { subcmd } => {
        commands::cmd_cache(cli, subcmd)
    })
}

pub fn cmd_slot_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Slot { subcmd } => {
        commands::cmd_slot(cli, subcmd)
    })
}

pub fn cmd_doctor_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Doctor { fix, verbose, output } => {
        commands::cmd_doctor(cli.model.as_deref(), *fix, *verbose, cli.json || output.json)
    })
}

pub fn cmd_ping_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Ping { output } => {
        commands::cmd_ping(cli.json || output.json)
    })
}

pub fn cmd_status_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Status { watch_fresh, watch, output, wait, wait_secs } => {
        commands::cmd_status(
            cli.json || output.json,
            *watch_fresh,
            *watch,
            *wait,
            *wait_secs,
            cli.slot.as_deref(),
        )
    })
}

pub fn cmd_hook_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Hook { subcmd } => {
        commands::cmd_hook(subcmd.clone())
    })
}

/// `cqs refresh` is a daemon-only concept. By the time we reach this arm
/// `try_daemon_query` already forwarded the request if a daemon was running,
/// so we're guaranteed there isn't one. Emit a polite no-op.
pub fn cmd_refresh_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Refresh { output } => {
        if cli.json || output.json {
            crate::cli::json_envelope::emit_json(&serde_json::json!({
                "status": "noop",
                "message": "no daemon running, nothing to refresh",
                "refreshed": false,
                "daemon_running": false,
                "caches_invalidated": [],
            }))
        } else {
            tracing::info!(
                refreshed = false,
                daemon_running = false,
                "cqs refresh: no daemon"
            );
            println!("no daemon running, nothing to refresh");
            Ok(())
        }
    })
}

pub fn cmd_index_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Index { args } => {
        commands::cmd_index(cli, args)
    })
}

pub fn cmd_watch_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Watch { debounce, no_ignore, poll, serve } => {
        crate::cli::watch::cmd_watch(cli, *debounce, *no_ignore, *poll, *serve)
    })
}

#[cfg(feature = "serve")]
pub fn cmd_serve_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Serve { port, bind, open, no_auth } => {
        crate::cli::commands::serve::cmd_serve(*port, bind.clone(), *open, *no_auth)
    })
}

pub fn cmd_batch_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Batch => {
        crate::cli::batch::cmd_batch()
    })
}

pub fn cmd_chat_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Chat => {
        crate::cli::chat::cmd_chat()
    })
}

pub fn cmd_completions_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Completions { shell } => {
        crate::cli::dispatch::cmd_completions(*shell);
        Ok(())
    })
}

pub fn cmd_train_data_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(
        cmd,
        Commands::TrainData {
            repos, output, max_commits, min_msg_len,
            max_files, dedup_cap, resume, verbose,
        } => {
            commands::cmd_train_data(cqs::train_data::TrainDataConfig {
                repos: repos.clone(),
                output: output.clone(),
                // CLI surface uses `Option<usize>` (None = unlimited).
                // Library API uses `usize` with `0` as the no-cap sentinel.
                max_commits: max_commits.unwrap_or(0),
                min_msg_len: *min_msg_len,
                max_files: *max_files,
                dedup_cap: *dedup_cap,
                resume: *resume,
                verbose: *verbose,
            })
        }
    )
}

pub fn cmd_export_model_dispatch(
    _cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::ExportModel { repo, output, dim } => {
        commands::cmd_export_model(repo, output, *dim)
    })
}

#[cfg(feature = "convert")]
pub fn cmd_convert_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(
        cmd,
        Commands::Convert {
            path, output_dir, overwrite, dry_run, clean_tags, output,
        } => {
            commands::cmd_convert(
                path,
                output_dir.as_deref(),
                *overwrite,
                *dry_run,
                clean_tags.as_deref(),
                cli.json || output.json,
            )
        }
    )
}

pub fn cmd_telemetry_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Telemetry { reset, reason, all, output } => {
        if *reset {
            commands::cmd_telemetry_reset(project_cqs_dir, reason.as_deref(), cli.json || output.json)
        } else {
            commands::cmd_telemetry(project_cqs_dir, cli.json || output.json, *all)
        }
    })
}

pub fn cmd_project_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Project { subcmd } => {
        commands::cmd_project(cli, subcmd, cli.try_model_config()?)
    })
}

pub fn cmd_model_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Model { subcmd } => {
        commands::cmd_model(cli, subcmd)
    })
}

pub fn cmd_diff_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Diff { args, output } => {
        commands::cmd_diff(
            &args.source,
            args.target.as_deref(),
            args.threshold,
            args.lang.as_deref(),
            cli.json || output.json,
        )
    })
}

pub fn cmd_drift_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Drift { args, output } => {
        commands::cmd_drift(
            &args.reference,
            args.threshold,
            args.min_drift,
            args.lang.as_deref(),
            args.limit,
            cli.json || output.json,
        )
    })
}

pub fn cmd_ref_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Ref { subcmd } => {
        commands::cmd_ref(cli, subcmd)
    })
}

pub fn cmd_gc_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Gc { output } => {
        commands::cmd_gc(cli, cli.json || output.json)
    })
}

pub fn cmd_audit_mode_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::AuditMode { state, expires, output } => {
        commands::cmd_audit_mode(state.as_ref(), expires, cli.json || output.json)
    })
}

/// Notes is a Group A command but conditionally opens a readonly store
/// (mutations work on a fresh project pre-`cqs init && cqs index`, list
/// requires the store). Open optimistically, log debug breadcrumb on failure,
/// hand `Option<&ctx>` to `cmd_notes`.
pub fn cmd_notes_dispatch(
    cli: &Cli,
    _ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    must_be!(cmd, Commands::Notes { subcmd } => {
        let notes_ctx = match crate::cli::CommandContext::open_readonly(cli) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "Notes: readonly store open failed; mutations will use write-only path"
                );
                None
            }
        };
        commands::cmd_notes(cli, notes_ctx.as_ref(), subcmd)
    })
}

// ─── Group B — store-using ──────────────────────────────────────────────────

/// Macro to extract the typestate-aware ctx from `Option<&CommandContext>`.
/// Group B shims always receive `Some(&ctx)` from the derive.
macro_rules! group_b_ctx {
    ($ctx:expr) => {
        $ctx.expect("Group B dispatch shim must be called with Some(&ctx) — derive bug")
    };
}

pub fn cmd_affected_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Affected { base, stdin, output } => {
        commands::cmd_affected(ctx, base.as_deref(), *stdin, cli.json || output.json)
    })
}

pub fn cmd_blame_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Blame { args, output } => {
        commands::cmd_blame(
            ctx,
            &args.name,
            args.commits,
            args.callers,
            cli.json || output.json,
        )
    })
}

pub fn cmd_brief_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Brief { path, output } => {
        commands::cmd_brief(ctx, path, cli.json || output.json)
    })
}

pub fn cmd_stats_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Stats { output } => {
        commands::cmd_stats(ctx, cli.json || output.json)
    })
}

pub fn cmd_deps_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Deps { args, output } => {
        commands::cmd_deps(
            ctx,
            &args.name,
            args.reverse,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        )
    })
}

pub fn cmd_callers_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Callers { args, output } => {
        commands::cmd_callers(
            ctx,
            &args.name,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        )
    })
}

pub fn cmd_callees_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Callees { args, output } => {
        commands::cmd_callees(
            ctx,
            &args.name,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        )
    })
}

pub fn cmd_onboard_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Onboard { args, output } => {
        commands::cmd_onboard(
            ctx,
            &args.query,
            args.depth,
            args.direction,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
        )
    })
}

pub fn cmd_neighbors_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Neighbors { name, limit, output } => {
        commands::cmd_neighbors(ctx, name, *limit, cli.json || output.json)
    })
}

pub fn cmd_explain_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Explain { args, output } => {
        commands::cmd_explain(
            ctx,
            &args.name,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
        )
    })
}

pub fn cmd_similar_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Similar { args, output } => {
        // Scope flags resolve subcommand-tail first (`cqs similar foo --lang
        // rust`), then fall back to the top-level region (`cqs --lang rust
        // similar foo`). The daemon path forwards the top-level values onto the
        // tail, so honoring both spellings keeps CLI-direct and daemon-routed
        // scoping identical.
        let lang = args.lang.as_deref().or(cli.lang.as_deref());
        let path = args.path.as_deref().or(cli.path.as_deref());
        commands::cmd_similar(
            ctx,
            &args.name,
            args.limit_arg.limit,
            args.threshold,
            lang,
            path,
            cli.json || output.json,
        )
    })
}

/// Top-level `--json` (cli.json) overrides whatever the subcommand's
/// `--format` says. `effective_format()` already honours `output.json`; we OR
/// cli.json on top so `cqs --json impact foo` works without `--json` on the
/// subcommand.
pub fn cmd_impact_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Impact { args, output } => {
        let format = if cli.json {
            crate::cli::OutputFormat::Json
        } else {
            output.effective_format()
        };
        commands::cmd_impact(
            ctx,
            &args.name,
            args.depth,
            args.limit_arg.limit,
            &format,
            args.suggest_tests,
            args.type_impact,
            args.cross_project,
        )
    })
}

pub fn cmd_impact_diff_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::ImpactDiff { args, output } => {
        commands::cmd_impact_diff(
            ctx,
            args.base.as_deref(),
            args.stdin,
            cli.json || output.json,
        )
    })
}

pub fn cmd_review_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Review { args, output } => {
        let format = if cli.json {
            crate::cli::OutputFormat::Json
        } else {
            output.effective_format()
        };
        commands::cmd_review(ctx, args.base.as_deref(), args.stdin, &format, args.tokens)
    })
}

pub fn cmd_ci_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Ci { args, output } => {
        let format = if cli.json {
            crate::cli::OutputFormat::Json
        } else {
            output.effective_format()
        };
        commands::cmd_ci(
            ctx,
            args.base.as_deref(),
            args.stdin,
            &format,
            &args.gate,
            args.tokens,
        )
    })
}

pub fn cmd_trace_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Trace { args, output } => {
        let format = if cli.json {
            crate::cli::OutputFormat::Json
        } else {
            output.effective_format()
        };
        commands::cmd_trace(
            ctx,
            &args.source,
            &args.target,
            args.max_depth as usize,
            args.limit_arg.limit,
            &format,
            args.cross_project,
        )
    })
}

pub fn cmd_test_map_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::TestMap { args, output } => {
        commands::cmd_test_map(
            ctx,
            &args.name,
            args.depth as usize,
            args.limit_arg.limit,
            args.cross_project,
            cli.json || output.json,
        )
    })
}

pub fn cmd_context_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Context { args, output } => {
        commands::cmd_context(
            ctx,
            &args.path,
            cli.json || output.json,
            args.summary,
            args.compact,
            args.tokens,
        )
    })
}

pub fn cmd_dead_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Dead { args, output } => {
        commands::cmd_dead(
            ctx,
            cli.json || output.json,
            args.include_pub,
            args.min_confidence,
        )
    })
}

pub fn cmd_gather_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Gather { args, output } => {
        commands::cmd_gather(&commands::GatherContext {
            ctx,
            query: &args.query,
            expand: args.depth,
            direction: args.direction,
            limit: args.limit_arg.limit,
            max_tokens: args.tokens,
            ref_name: args.ref_name.as_deref(),
            json: cli.json || output.json,
        })
    })
}

pub fn cmd_health_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Health { output } => {
        commands::cmd_health(ctx, cli.json || output.json)
    })
}

pub fn cmd_stale_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Stale { args, output } => {
        commands::cmd_stale(ctx, cli.json || output.json, args.count_only)
    })
}

pub fn cmd_suggest_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Suggest { args, output } => {
        commands::cmd_suggest(ctx, cli.json || output.json, args.apply)
    })
}

pub fn cmd_read_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Read { args, output } => {
        commands::cmd_read(
            ctx,
            &args.path,
            args.focus.as_deref(),
            cli.json || output.json,
        )
    })
}

pub fn cmd_reconstruct_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Reconstruct { path, output } => {
        commands::cmd_reconstruct(ctx, path, cli.json || output.json)
    })
}

pub fn cmd_related_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Related { args, output } => {
        commands::cmd_related(ctx, &args.name, args.limit_arg.limit, cli.json || output.json)
    })
}

pub fn cmd_where_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Where { args, output } => {
        commands::cmd_where(ctx, &args.description, args.limit_arg.limit, cli.json || output.json)
    })
}

pub fn cmd_scout_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Scout { args, output } => {
        commands::cmd_scout(
            ctx,
            &args.query,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
            args.search_limit,
            args.search_threshold,
            args.min_gap_ratio,
        )
    })
}

pub fn cmd_plan_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Plan { args, output } => {
        commands::cmd_plan(
            ctx,
            &args.description,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
        )
    })
}

pub fn cmd_task_dispatch(
    cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Task { args, output } => {
        commands::cmd_task(
            ctx,
            &args.description,
            args.limit_arg.limit,
            cli.json || output.json,
            args.tokens,
            args.brief,
        )
    })
}

pub fn cmd_train_pairs_dispatch(
    _cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::TrainPairs { output, limit, language, contrastive } => {
        commands::cmd_train_pairs(
            ctx,
            output.as_path(),
            *limit,
            language.as_deref(),
            *contrastive,
        )
    })
}

pub fn cmd_eval_dispatch(
    _cli: &Cli,
    ctx: Option<&CommandContext<'_, ReadOnly>>,
    _project_cqs_dir: &Path,
    cmd: &Commands,
) -> Result<()> {
    let ctx = group_b_ctx!(ctx);
    must_be!(cmd, Commands::Eval { args } => {
        commands::cmd_eval(ctx, args)
    })
}
