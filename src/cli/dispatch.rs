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

    // Parent-index write guard. A WRITE command whose resolved
    // project root crossed a git-worktree / Cargo-workspace boundary
    // *upward* would silently mutate an index outside the current
    // worktree. Refuse it unless explicitly acknowledged. Runs before any
    // dispatch (and before daemon forwarding, which never carries write
    // commands anyway — they're all `BatchSupport::Cli`), so the
    // mutation is blocked at the boundary, not by call-site discipline.
    // Reads never reach this gate.
    if let Some(cmd) = cli.command.as_ref() {
        if cmd.mutates_index() {
            guard_parent_index_write(cli.parent_index)?;
        }
    }

    // Load config and apply defaults (CLI flags override config)
    let config = cqs::config::Config::load(&find_project_root());
    apply_config_defaults(&mut cli, &config);

    // Wire the [scoring] config section to the RRF K override so a user
    // writing `[scoring] rrf_k = 40` in `.cqs.toml` is honored.
    if let Some(ref scoring) = config.scoring {
        cqs::search::scoring::set_rrf_k_from_config(scoring);
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

/// Environment-variable acknowledgment for a parent-index write.
/// Mirrors the `--parent-index` flag — either suffices. Set to `1` to
/// permit a WRITE command to mutate the parent index from inside a
/// worktree.
const PARENT_INDEX_OK_ENV: &str = "CQS_PARENT_INDEX_OK";

/// Refuse a WRITE command whose resolved project root crossed a
/// git-worktree / Cargo-workspace boundary upward, unless the caller
/// acknowledged it via `--parent-index` or `CQS_PARENT_INDEX_OK=1`.
///
/// The guard lives on the CLI resolution path: every write command is
/// `BatchSupport::Cli` and runs inline (the daemon forwards reads only),
/// so blocking here covers both surfaces — a daemon-forwarded write
/// cannot exist. Reads never call this.
///
/// `acknowledged` is the parsed `--parent-index` flag; the env var is an
/// equivalent acknowledgment for scripted / agent contexts that prefer a
/// process-env opt-in over a per-invocation flag.
fn guard_parent_index_write(acknowledged: bool) -> Result<()> {
    let _span = tracing::info_span!("guard_parent_index_write").entered();

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            // Can't resolve CWD → can't detect a boundary crossing.
            // Fail-open: never block a write on a CWD-resolution quirk.
            tracing::warn!(error = %e, "parent-index guard: current_dir() failed; skipping check");
            return Ok(());
        }
    };
    let resolved_root = find_project_root();

    let Some(worktree_root) = cqs::worktree::parent_index_boundary_crossed(&cwd, &resolved_root)
    else {
        // No upward boundary crossing — regular repo or non-worktree.
        return Ok(());
    };

    let env_ok = std::env::var(PARENT_INDEX_OK_ENV)
        .map(|v| v.trim() == "1")
        .unwrap_or(false);

    if acknowledged || env_ok {
        tracing::warn!(
            worktree = %worktree_root.display(),
            parent_index = %resolved_root.display(),
            via = if acknowledged { "--parent-index" } else { PARENT_INDEX_OK_ENV },
            "Writing to the PARENT index from inside a worktree (acknowledged). \
             This mutates an index outside the current worktree."
        );
        return Ok(());
    }

    tracing::warn!(
        worktree = %worktree_root.display(),
        parent_index = %resolved_root.display(),
        "Refusing to write to the PARENT index from inside a worktree"
    );
    Err(anyhow::anyhow!(
        "refusing to write to the parent index from inside a git worktree\n  \
         worktree root: {worktree}\n  \
         resolved index: {parent}\n\n\
         This WRITE command's project-root discovery walked up past the worktree's \
         own .git to the parent index (Cargo-workspace / worktree boundary). \
         Mutating it would defeat worktree isolation.\n\n\
         If this is intentional, re-run with `--parent-index` or set \
         `{env}=1`. To write a worktree-local index instead, run `cqs init` \
         in the worktree first (creates its own .cqs/).",
        worktree = worktree_root.display(),
        parent = resolved_root.display(),
        env = PARENT_INDEX_OK_ENV,
    ))
}

/// Generate shell completion scripts for the specified shell
pub(crate) fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}

/// `true` when this invocation requests JSON output and so is forwardable to
/// the daemon, which serves the structured JSON payload.
///
/// Resolution:
/// - Top-level `--json` (`cli.json`) forces JSON for every command.
/// - Subcommands resolve their own `--json` / `--format json` via
///   [`Commands::effective_output_format`].
/// - The bare-query path (`cqs "query"`, no subcommand) has no per-command
///   output group; it is JSON only when top-level `--json` is set.
///
/// Text mode (everything else) returns `false` so the caller keeps the
/// command on the CLI path: text-mode invocations render the payload through
/// the command's own renderer, so output is surface-independent rather than
/// the raw daemon JSON payload.
#[cfg(unix)]
fn daemon_invocation_is_json(cli: &Cli) -> bool {
    use crate::cli::definitions::OutputFormat;
    if cli.json {
        return true;
    }
    match cli.command.as_ref() {
        Some(cmd) => matches!(cmd.effective_output_format(), Some(OutputFormat::Json)),
        // Bare-query search: text by default, JSON only under top-level
        // `--json` (handled above).
        None => false,
    }
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
const PROCESS_LOCAL_ARG_IDS: &[&str] =
    &["json", "quiet", "model", "slot", "verbose", "parent_index"];

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
    "no_rank_signals",
    "overlay",
];

/// Top-level `Cli` arg IDs whose value is a *search scope* (`lang`, `path`)
/// the subcommand handlers honor when the same flag is present on their own
/// batch surface. On daemon forwarding these are re-spliced onto the tail of
/// any subcommand whose batch `*Args` accepts them — see
/// [`cqs::daemon_translate::CliArgSpec::scope_targets`]. A subset of
/// [`SEARCH_KNOB_ARG_IDS`]; the membership is pinned by
/// `scope_arg_ids_are_search_knobs` below so a scope ID can't drift out of the
/// search-knob set silently.
#[cfg(unix)]
const SCOPE_ARG_IDS: &[&str] = &["lang", "path"];

/// Build the [`cqs::daemon_translate::CliArgSpec`] from the live clap
/// definition. Derived at runtime (like `telemetry::describe_command`) so a
/// new top-level flag is classified automatically — hand-mirrored flag lists
/// are how `-v <cmd>` / `--rrf <cmd>` came to hard-error daemon-up while
/// working daemon-down. The batch command is passed so the scope-forward
/// target set is derived from each subcommand's live `*Args` surface rather
/// than a hardcoded subcommand match.
#[cfg(unix)]
fn cli_arg_spec() -> cqs::daemon_translate::CliArgSpec {
    use clap::CommandFactory;
    let daemon_capable = crate::cli::definitions::Commands::daemon_capable_variant_names();
    cqs::daemon_translate::CliArgSpec::from_clap(
        &Cli::command(),
        PROCESS_LOCAL_ARG_IDS,
        &crate::cli::batch::BatchInput::command(),
        SCOPE_ARG_IDS,
        &daemon_capable,
    )
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

    // Text-mode invocations stay on the CLI path. The daemon wire shape is
    // structured JSON (the agent-facing contract); the CLI renders prose for
    // text mode through each command's own renderer, which often needs the
    // open store (e.g. `impact` re-resolves the relative file label) and so
    // can't be reproduced from the JSON payload alone. Forwarding a text-mode
    // query would print that JSON payload instead of the rendered text —
    // output would depend on whether a daemon happened to serve it. Bypassing
    // here keeps text output surface-independent at the cost of the daemon
    // fast path for text mode only; `--json` keeps the fast path.
    if !daemon_invocation_is_json(cli) {
        return Ok(None);
    }

    // `--stdin` invocations (review / ci / impact-diff with a piped diff) stay
    // on the CLI path even in JSON mode. The daemon reads its diff in the
    // *server* process and never sees the client's stdin, so forwarding would
    // silently analyze the wrong diff. This is the same surface-independence
    // guarantee the text-mode bypass above provides, applied to a stdin-bearing
    // invocation rather than a text-mode one.
    if let Some(cmd) = cli.command.as_ref() {
        if cmd.reads_diff_from_stdin() {
            tracing::debug!(
                cmd = cmd_label,
                "--stdin invocation kept on CLI path: daemon has no client stdin on the wire"
            );
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
                    // PARITY: the daemon's search handler runs the per-origin
                    // staleness check (`attach_stale_origins_meta` in
                    // `batch::handlers::search`) and reports stale files via
                    // `_meta.stale_origins`. Print the same stderr warning the
                    // CLI-direct path emits (`warn_stale_results` →
                    // `print_stale_warning`), gated on `--quiet` exactly like
                    // `render_query_output`. `--no-stale-check` needs no gate
                    // here: it forwards to the daemon, which skips the check,
                    // so the meta is absent.
                    if !cli.quiet {
                        crate::cli::staleness::print_stale_warning_from_meta(meta);
                    }
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
            search.get_arguments().flat_map(spellings).collect();

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

    /// Every `SCOPE_ARG_IDS` entry must be a top-level search knob — a scope
    /// flag is a filter the search path honors, never a process-local flag.
    /// If a scope ID drifts out of `SEARCH_KNOB_ARG_IDS` (e.g. a rename) this
    /// fails instead of silently forwarding a flag the batch parser rejects.
    #[test]
    fn scope_arg_ids_are_search_knobs() {
        for id in SCOPE_ARG_IDS {
            assert!(
                SEARCH_KNOB_ARG_IDS.contains(id),
                "SCOPE_ARG_IDS entry `{id}` must also be in SEARCH_KNOB_ARG_IDS — \
                 a scope flag is a search knob, not a process-local flag"
            );
        }
    }

    /// The scope-forward target set is *derived* from the live batch clap
    /// definition, not a hand-maintained list. This pins the derivation: the
    /// set of subcommands the daemon translator forwards top-level scope flags
    /// to must equal exactly the set of batch subcommands whose flattened
    /// `*Args` surface accepts a scope-flag spelling.
    ///
    /// Concretely, this FAILS if someone adds `lang`/`path` to another wire
    /// `*Args` struct without the forwarding picking it up — the independent
    /// recomputation here grows, `spec.scope_targets` must grow with it, and a
    /// mismatch means the derivation broke (someone re-introduced a hardcoded
    /// list). It equally fails if `scope_targets` lists a subcommand whose
    /// batch surface does *not* take the flag (a spurious forward).
    #[test]
    fn scope_targets_track_the_batch_wire_surface() {
        let spec = cli_arg_spec();
        assert!(
            !spec.scope_flags.is_empty(),
            "scope_flags empty — derivation regression (SCOPE_ARG_IDS not picked up from clap)"
        );

        // Independently recompute the expected targets straight from the batch
        // clap surface: a *daemon-capable* subcommand is a target iff one of
        // its non-positional args spells a scope flag. This mirrors the
        // production derivation but is written separately here so a bug in the
        // production walk is caught by divergence rather than copied.
        let daemon_capable: std::collections::BTreeSet<&str> =
            crate::cli::definitions::Commands::daemon_capable_variant_names()
                .into_iter()
                .collect();
        let batch_app = crate::cli::batch::BatchInput::command();
        let expected: std::collections::BTreeSet<String> = batch_app
            .get_subcommands()
            .filter(|sub| daemon_capable.contains(sub.get_name()))
            .filter(|sub| {
                sub.get_arguments()
                    .filter(|a| !a.is_positional())
                    .flat_map(spellings)
                    .any(|s| spec.scope_flags.contains(&s))
            })
            .map(|sub| sub.get_name().to_string())
            .collect();

        assert_eq!(
            spec.scope_targets, expected,
            "scope-forward targets diverged from the batch wire surface — \
             the derivation must equal exactly the subcommands whose `*Args` \
             accept a scope flag, never a hardcoded list"
        );

        // Anchor current behavior: `similar` carries lang/path on its
        // `SimilarArgs`, so it must be a target; `callers` does not, so it must
        // not. These guard against the derivation collapsing to empty/all.
        assert!(
            spec.scope_targets.contains("similar"),
            "`similar` accepts --lang/--path on its batch surface and must be a scope-forward target"
        );
        assert!(
            !spec.scope_targets.contains("callers"),
            "`callers` has no --lang/--path on its batch surface and must not be a scope-forward target"
        );
    }

    /// Behavioral pin on the *translator*, driven by the production-derived
    /// spec: for every daemon-capable subcommand whose batch `*Args` accepts
    /// `--lang`, a top-level `cqs --lang rust <sub> <pos>` must forward the
    /// scope flag onto that subcommand's tail; for every daemon-capable
    /// subcommand that does NOT accept it, the flag must be dropped.
    ///
    /// This is the real guard the issue asks for: it FAILS if someone adds
    /// `lang`/`path` to another daemon-routed wire `*Args` struct but the
    /// forwarding doesn't pick it up — e.g. if the derivation regressed to a
    /// hardcoded `["similar"]` list, the newly-eligible subcommand would land
    /// in the "accepts but not forwarded" branch here and trip the assert.
    /// It probes the live batch clap surface for eligibility, so the
    /// expectation tracks the wire structs, never a copy of the target list.
    #[test]
    fn translator_forwards_scope_flags_for_every_eligible_daemon_command() {
        let spec = cli_arg_spec();
        let batch_app = crate::cli::batch::BatchInput::command();

        let mut checked_forward = 0u32;
        let mut checked_dropped = 0u32;
        for name in crate::cli::definitions::Commands::daemon_capable_variant_names() {
            let Some(sub) = batch_app.find_subcommand(name) else {
                // Pinned separately by `every_daemon_capable_command_has_a_batch_subcommand`.
                continue;
            };
            // Does this subcommand's batch surface accept `--lang`? (a single
            // representative scope spelling — `scope_flags` membership is the
            // same set the production derivation consults).
            let accepts_lang = sub
                .get_arguments()
                .filter(|a| !a.is_positional())
                .flat_map(spellings)
                .any(|s| s == "--lang");

            // Build a top-level-scoped argv. A bare positional (`x`) satisfies
            // every daemon-capable subcommand's required arg or is ignored by
            // arg-less ones; the translator never parses it, only splices.
            let argv: Vec<String> = ["--lang", "rust", name, "x"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let (cmd, args) =
                cqs::daemon_translate::translate_cli_args_to_batch(&argv, true, &spec);
            assert_eq!(
                cmd, name,
                "subcommand name must round-trip through the translator"
            );

            if accepts_lang {
                assert!(
                    args.windows(2).any(|w| w[0] == "--lang" && w[1] == "rust"),
                    "daemon-capable `{name}` accepts --lang on its batch surface but the \
                     translator did not forward the top-level `--lang rust` onto its tail \
                     (got {args:?}) — the scope-forward derivation missed it"
                );
                checked_forward += 1;
            } else {
                assert!(
                    !args.iter().any(|a| a == "--lang"),
                    "daemon-capable `{name}` does NOT accept --lang on its batch surface, \
                     so the top-level scope flag must be dropped, not forwarded (got {args:?})"
                );
                checked_dropped += 1;
            }
        }
        // Guard the loop actually exercised both branches — a derivation that
        // silently excluded every command would make the asserts vacuous.
        assert!(
            checked_forward >= 1,
            "no daemon-capable subcommand was found to accept --lang — derivation regression \
             (expected at least `similar`)"
        );
        assert!(
            checked_dropped >= 1,
            "no daemon-capable subcommand was found to reject --lang — fixture regression"
        );
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

    /// Parse `argv` (after the implicit `cqs`) into a `Cli`, returning the
    /// JSON-mode decision the daemon-forward gate makes for it.
    fn gate_is_json(argv: &[&str]) -> bool {
        use clap::Parser as _;
        let mut full = vec!["cqs"];
        full.extend_from_slice(argv);
        let cli = Cli::try_parse_from(full).expect("argv must parse");
        daemon_invocation_is_json(&cli)
    }

    /// Every daemon-dispatchable command must report a concrete output format
    /// (`Some(..)`) so the text-mode gate can classify it. A new daemon
    /// command added without an arm in `Commands::effective_output_format`
    /// returns `None` → treated as text → silently bypasses the daemon. That
    /// is the safe failure direction (correct output, slower), but it also
    /// means the new command never gets the daemon fast path even with
    /// `--json`. This test makes that omission loud at test time.
    ///
    /// `daemon_capable_variant_names` is emitted by the `CqsCommands` derive;
    /// it lists exactly the top-level variants with `batch = "daemon"`.
    #[test]
    fn daemon_dispatchable_variants_report_an_output_format() {
        use crate::cli::definitions::Commands;
        use clap::Parser as _;
        // Representative argv per daemon-capable command. Kept minimal: only
        // the required positionals so the parse succeeds. The gate predicate
        // only inspects the output flags, not the args.
        let argv_for: &[(&str, &[&str])] = &[
            ("stats", &["stats"]),
            ("blame", &["blame", "foo"]),
            ("deps", &["deps", "foo"]),
            ("callers", &["callers", "foo"]),
            ("callees", &["callees", "foo"]),
            ("onboard", &["onboard", "foo"]),
            ("explain", &["explain", "foo"]),
            ("similar", &["similar", "foo"]),
            ("impact", &["impact", "foo"]),
            ("impact-diff", &["impact-diff"]),
            ("review", &["review"]),
            ("ci", &["ci"]),
            ("trace", &["trace", "foo", "bar"]),
            ("test-map", &["test-map", "foo"]),
            ("context", &["context", "foo"]),
            ("dead", &["dead"]),
            ("gather", &["gather", "foo"]),
            ("health", &["health"]),
            ("stale", &["stale"]),
            ("read", &["read", "foo"]),
            ("related", &["related", "foo"]),
            ("where", &["where", "foo"]),
            ("scout", &["scout", "foo"]),
            ("plan", &["plan", "foo"]),
            ("task", &["task", "foo"]),
            ("refresh", &["refresh"]),
            // `notes` is daemon-capable for the `list` subcommand (runtime
            // classification); its output format lives on the subcommand.
            ("notes", &["notes", "list"]),
            // `suggest` is daemon-capable when no `--apply` mutation runs
            // (runtime classification); it carries `TextJsonArgs`.
            ("suggest", &["suggest"]),
        ];
        let covered: std::collections::BTreeSet<&str> =
            argv_for.iter().map(|(name, _)| *name).collect();

        // The derive lists exactly the `batch = "daemon"` top-level variants.
        for name in Commands::daemon_capable_variant_names() {
            assert!(
                covered.contains(name),
                "daemon-capable command `{name}` has no coverage in this test — \
                 add an argv entry and an arm in Commands::effective_output_format"
            );
        }

        for (name, argv) in argv_for {
            let cli = {
                let mut full = vec!["cqs"];
                full.extend_from_slice(argv);
                Cli::try_parse_from(full)
                    .unwrap_or_else(|e| panic!("argv for `{name}` must parse: {e}"))
            };
            let cmd = cli.command.as_ref().unwrap_or_else(|| {
                panic!("argv for `{name}` must produce a subcommand");
            });
            assert!(
                cmd.effective_output_format().is_some(),
                "daemon-capable command `{name}` must report an output format \
                 (Some) so the text-mode gate can classify it"
            );
        }
    }

    /// Text-mode invocations are not daemon-forwardable; JSON-mode ones are.
    /// Covers the three input shapes: `--json` boolean, `--format json`
    /// (OutputArgs), and the bare-query path.
    #[test]
    fn daemon_gate_text_mode_is_not_forwardable() {
        // Subcommand text mode → CLI path.
        assert!(!gate_is_json(&["read", "src/lib.rs"]));
        assert!(!gate_is_json(&["impact", "foo"]));
        // Subcommand JSON mode → daemon path.
        assert!(gate_is_json(&["read", "src/lib.rs", "--json"]));
        // OutputArgs `--format json` is recognized as JSON.
        assert!(gate_is_json(&["impact", "foo", "--format", "json"]));
        assert!(gate_is_json(&["impact", "foo", "--json"]));
        // Top-level `--json` forces JSON for any command.
        assert!(gate_is_json(&["--json", "read", "src/lib.rs"]));
        // Bare query: text by default, JSON under top-level `--json`.
        assert!(!gate_is_json(&["find the thing"]));
        assert!(gate_is_json(&["--json", "find the thing"]));
        // `notes list` resolves its `--json` on the subcommand: text bypasses,
        // subcommand `--json` forwards (the existing notes round-trip relies
        // on this).
        assert!(!gate_is_json(&["notes", "list"]));
        assert!(gate_is_json(&["notes", "list", "--json"]));
    }
}
