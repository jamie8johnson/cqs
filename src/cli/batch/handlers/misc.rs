//! Misc dispatch handlers: notes, gc, plan, task, scout, where, gather, diff, drift, refresh, help.
//!
//! Handlers take a single `&XArgs` argument so the macro-driven
//! `BatchCmd::dispatch` calls every row uniformly.

use anyhow::Result;

use super::super::commands::BatchInput;
use super::super::BatchView;
use crate::cli::args::{
    DiffArgs, DriftArgs, GatherArgs, NotesListArgs, PlanArgs, ReconcileArgs, ScoutArgs, TaskArgs,
    WaitFreshArgs, WhereArgs,
};
use crate::cli::validate_finite_f32;

/// Performs a semantic search gather operation with optional cross-index querying and token budget constraints.
///
/// Takes `&GatherArgs` directly (the shared CLI/batch struct). Both the CLI
/// and batch paths deserialize into the same struct, so there is no per-field
/// drift to reason about.
pub(in crate::cli::batch) fn dispatch_gather(
    ctx: &BatchView,
    args: &GatherArgs,
) -> Result<serde_json::Value> {
    let query = args.query.as_str();
    let ref_name = args.ref_name.as_deref();
    let _span = tracing::info_span!("batch_gather", query, ?ref_name).entered();

    let embedder = ctx.embedder()?;

    // Thin adapter over the shared `gather_core`. The daemon always serializes,
    // so it charges the per-result JSON overhead in token packing. Reference
    // resolution differs by surface (cached LRU here), so the adapter resolves
    // the reference index + project vector index and hands them to the core.
    let core_args = crate::cli::commands::gather::GatherArgs {
        query: query.to_string(),
        depth: args.depth,
        direction: args.direction,
        limit: args.limit_arg.limit,
        tokens: args.tokens,
        json_overhead: crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
    };

    let (result, token_info) = if let Some(rn) = ref_name {
        ctx.get_ref(rn)?;
        let ref_idx = ctx
            .borrow_ref(rn)
            .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", rn))?;
        let index = ctx.vector_index()?;
        crate::cli::commands::gather::gather_core(
            &ctx.store(),
            embedder,
            &ctx.root,
            &core_args,
            Some(ref_idx.as_ref()),
            index.as_deref(),
        )?
    } else {
        crate::cli::commands::gather::gather_core(
            &ctx.store(),
            embedder,
            &ctx.root,
            &core_args,
            None,
            None,
        )?
    };

    let output = crate::cli::commands::build_gather_output(&result, query, token_info);
    Ok(serde_json::to_value(&output)?)
}

/// Dispatches filtered notes from the batch context as a JSON response.
///
/// Thin adapter over the shared [`crate::cli::commands::notes::notes_list_core`]
/// (same union schema the CLI emits). Filters the always-fresh
/// `docs/notes.toml` parse by `warnings` / `patterns` / `kind`, computes the
/// `--check` staleness map, and hands both to the core. Each note carries the
/// union field set (`id`, `type`, `sentiment`, `sentiment_label`, `text`,
/// `mentions`, optional `kind` / `stale_mentions`).
///
/// `check: bool` routes staleness checks through the daemon path so agents
/// calling `cqs notes list --check --json` via the socket receive
/// `stale_mentions` per note (present when `--check` is set, absent otherwise).
///
/// # Arguments
/// * `ctx` - The batch context containing the notes to dispatch
/// * `args` - Filter knobs (`warnings` / `patterns` / `kind` / `check`)
///
/// # Returns
/// A JSON object `{notes: [...], count: N}` over the filtered notes.
///
/// # Errors
/// Returns an error if the staleness check fails.
pub(in crate::cli::batch) fn dispatch_notes(
    ctx: &BatchView,
    args: &NotesListArgs,
) -> Result<serde_json::Value> {
    let warnings = args.warnings;
    let patterns = args.patterns;
    let kind = args.kind.as_deref();
    let check = args.check;
    let _span = tracing::info_span!("batch_notes", warnings, patterns, kind, check).entered();

    let notes = ctx.notes();

    // Populate `stale_mentions` keyed by note text only when `--check` is set
    // (single query through the cached read-only store; no extra writes).
    let staleness: std::collections::HashMap<String, Vec<String>> = if check {
        cqs::suggest::check_note_staleness(&ctx.store(), &ctx.root)?
            .into_iter()
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    let kind_norm = kind.and_then(|k| {
        let trimmed = k.trim().to_lowercase();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    let filtered: Vec<&cqs::note::Note> = notes
        .iter()
        .filter(|n| {
            let sentiment_ok = if warnings {
                n.is_warning()
            } else if patterns {
                n.is_pattern()
            } else {
                true
            };
            let kind_ok = match &kind_norm {
                Some(k) => n.kind.as_deref() == Some(k.as_str()),
                None => true,
            };
            sentiment_ok && kind_ok
        })
        .collect();

    // Thin adapter over the shared `notes_list_core` — identical union object
    // (`{notes, count}` with per-note `id`/`type`/`sentiment_label`) across the
    // CLI and daemon surfaces. Both read the same fresh `docs/notes.toml` parse.
    Ok(crate::cli::commands::notes::notes_list_core(
        &filtered, &staleness, check,
    ))
}

/// Dispatches a task execution within a batch context, optionally with token budgeting.
/// This function executes a task based on a natural language description, retrieving relevant code chunks and generating a JSON representation of the results. When a token budget is specified, it applies waterfall budgeting similar to the CLI; otherwise, it returns the standard task JSON representation.
/// # Arguments
/// * `ctx` - The batch execution context containing store, embedder, and root path
/// * `description` - Natural language description of the task to execute
/// * `limit` - Maximum number of results to return (clamped to 1-10)
/// * `tokens` - Optional token budget for waterfall budgeting of results
/// # Returns
/// A `Result` containing a JSON value representing the task execution results, with optional token-based budgeting applied.
/// # Errors
/// Returns an error if the embedder, call graph, test chunks cannot be retrieved from the context, or if task execution fails.
pub(in crate::cli::batch) fn dispatch_task(
    ctx: &BatchView,
    args: &TaskArgs,
) -> Result<serde_json::Value> {
    let description = args.description.as_str();
    let tokens = args.tokens;
    let _span = tracing::info_span!("batch_task", description).entered();
    let embedder = ctx.embedder()?;
    let limit = args
        .limit_arg
        .limit
        .clamp(1, crate::cli::PLACEMENT_LIMIT_CAP);
    let graph = ctx.call_graph()?;
    let test_chunks = ctx.test_chunks()?;
    let result = cqs::task_with_resources(
        &ctx.store(),
        embedder,
        description,
        &ctx.root,
        limit,
        &graph,
        &test_chunks,
    )?;

    // Shared projection: waterfall budgeting when `--tokens` is set, else full
    // serialization. Identical to the CLI's non-brief JSON path.
    crate::cli::commands::task::task_json_core(&result, embedder, tokens)
}

/// Performs a scout search query with optional token budget packing.
/// Executes a scout search on the store using the provided query and returns results as JSON. If a token budget is specified, attempts to batch-fetch chunk content and pack results based on relevance scoring within the token limit.
/// # Arguments
/// * `ctx` - Batch context containing the embedder and data store
/// * `query` - Search query string
/// * `limit` - Maximum number of results to return (clamped to 1-50)
/// * `tokens` - Optional token budget for content packing; if None, returns results without content
/// # Returns
/// A JSON value containing scout search results with optional packed content based on token budget.
/// # Errors
/// Returns an error if embedder initialization fails or if the core scout search operation fails.
pub(in crate::cli::batch) fn dispatch_scout(
    ctx: &BatchView,
    args: &ScoutArgs,
) -> Result<serde_json::Value> {
    let query = args.query.as_str();
    let _span = tracing::info_span!("batch_scout", query).entered();
    let embedder = ctx.embedder()?;
    // Thin adapter over the shared `scout_core` — identical JSON shape across
    // the CLI and daemon surfaces.
    let core_args = crate::cli::commands::scout::ScoutArgs {
        query: query.to_string(),
        limit: args.limit_arg.limit,
        tokens: args.tokens,
    };
    let (output, _token_info) =
        crate::cli::commands::scout::scout_core(&ctx.store(), embedder, &ctx.root, &core_args)?;
    Ok(output)
}

/// Suggests optimal file placements for code based on a natural language description.
/// Uses an embedder to analyze the provided description and searches the codebase to find the most suitable locations for placing new code. Returns placement suggestions ranked by relevance score, along with contextual information about each candidate location.
/// # Arguments
/// * `ctx` - The batch processing context containing the code store and embedder.
/// * `description` - A natural language description of the code to be placed.
/// * `limit` - The maximum number of suggestions to return (clamped to 1-10).
/// # Returns
/// A JSON value containing the input description and an array of placement suggestions, each with file path, relevance score, insertion line, nearby function name, reasoning, and detected code patterns (imports, error handling, naming conventions, visibility, inline tests).
/// # Errors
/// Returns an error if the embedder cannot be initialized or if the placement suggestion operation fails.
pub(in crate::cli::batch) fn dispatch_where(
    ctx: &BatchView,
    args: &WhereArgs,
) -> Result<serde_json::Value> {
    let description = args.description.as_str();
    let _span = tracing::info_span!("batch_where", description).entered();
    let embedder = ctx.embedder()?;
    let limit = args
        .limit_arg
        .limit
        .clamp(1, crate::cli::PLACEMENT_LIMIT_CAP);
    let result = cqs::suggest_placement(&ctx.store(), embedder, description, limit)?;

    let output = crate::cli::commands::build_where_output(&result, description, &ctx.root);
    Ok(serde_json::to_value(&output)?)
}

/// Detects content drift between a reference dataset and the current dataset by comparing similarity scores.
/// # Arguments
/// * `ctx` - The batch processing context containing reference and current data stores
/// * `reference` - The name of the reference dataset to compare against
/// * `threshold` - The similarity threshold (0.0-1.0) below which content is considered drifted
/// * `min_drift` - The minimum drift value to report
/// * `lang` - Optional language specification for drift detection
/// * `limit` - Optional maximum number of drifted items to return in results
/// # Returns
/// A JSON object containing:
/// - `reference`: The reference dataset name
/// - `threshold`: The similarity threshold used
/// - `min_drift`: The minimum drift value used
/// - `drifted`: Array of drifted items with name, file, chunk_type, similarity, and drift values
/// - `total_compared`: Total number of items compared
/// - `unchanged`: Number of unchanged items
/// # Errors
/// Returns an error if:
/// - The threshold or min_drift values are not finite numbers
/// - The reference dataset cannot be loaded or accessed
/// - Drift detection fails during comparison
pub(in crate::cli::batch) fn dispatch_drift(
    ctx: &BatchView,
    args: &DriftArgs,
) -> Result<serde_json::Value> {
    let reference = args.reference.as_str();
    let _span = tracing::info_span!("batch_drift", reference).entered();
    let threshold = validate_finite_f32(args.threshold, "threshold")?;
    let min_drift = validate_finite_f32(args.min_drift, "min_drift")?;

    // Use cached reference store (PERF-27/RM-17)
    ctx.get_ref(reference)?;
    let ref_idx = ctx
        .borrow_ref(reference)
        .ok_or_else(|| anyhow::anyhow!("Reference '{}' not loaded", reference))?;

    // Thin adapter: build the surface-agnostic args, then drive the shared
    // `drift_core` so the wire shape matches the CLI's typed `DriftOutput`
    // (normalized forward-slash paths, `chunk_type` via `ChunkType::to_string`).
    let core_args = crate::cli::commands::drift::DriftArgs {
        reference: reference.to_string(),
        threshold,
        min_drift,
        lang: args.lang.clone(),
        limit: args.limit,
    };
    let output = crate::cli::commands::drift::drift_core(&ref_idx.store, &ctx.store(), &core_args)?;
    Ok(serde_json::to_value(&output)?)
}

/// Runs semantic diff between a reference and the project (or another
/// reference). Thin adapter: resolve the source/target stores (needs the
/// reference LRU + config), build a surface-agnostic
/// [`crate::cli::commands::diff::DiffArgs`], then drive the shared
/// `diff_core` so the wire shape matches the CLI's typed `DiffOutput`.
pub(in crate::cli::batch) fn dispatch_diff(
    ctx: &BatchView,
    args: &DiffArgs,
) -> Result<serde_json::Value> {
    let source = args.source.as_str();
    let target = args.target.as_deref();
    let _span = tracing::info_span!("batch_diff", source).entered();
    let threshold = validate_finite_f32(args.threshold, "threshold")?;

    let source_store = crate::cli::commands::resolve::resolve_reference_store(&ctx.root, source)?;

    let target_label = target.unwrap_or("project");
    let core_args = crate::cli::commands::diff::DiffArgs {
        source: source.to_string(),
        target: target_label.to_string(),
        threshold,
        lang: args.lang.clone(),
    };

    // `project` diffs against the open store; any other target resolves a
    // reference store first.
    let output = if target_label == "project" {
        crate::cli::commands::diff::diff_core(&source_store, &ctx.store(), &core_args)?
    } else {
        let target_ref_store =
            crate::cli::commands::resolve::resolve_reference_store(&ctx.root, target_label)?;
        crate::cli::commands::diff::diff_core(&source_store, &target_ref_store, &core_args)?
    };

    Ok(serde_json::to_value(&output)?)
}

/// Runs task planning with template classification and returns results as JSON.
pub(in crate::cli::batch) fn dispatch_plan(
    ctx: &BatchView,
    args: &PlanArgs,
) -> Result<serde_json::Value> {
    let description = args.description.as_str();
    let tokens = args.tokens;
    let _span = tracing::info_span!("batch_plan", description).entered();

    let embedder = ctx.embedder()?;
    // Thin adapter over the shared `plan_core` — identical JSON shape across
    // the CLI and daemon surfaces.
    let core_args = crate::cli::commands::PlanArgs {
        description: description.to_string(),
        limit: args.limit_arg.limit,
        tokens,
    };
    let output = crate::cli::commands::plan_core(&ctx.store(), embedder, &ctx.root, &core_args)?;
    Ok(serde_json::to_value(&output)?)
}

/// Runs garbage collection on the index.
///
/// **Not available via the daemon path.** GC mutates the DB
/// (chunks/calls/type_edges/summaries/sparse_vectors pruning), but the
/// daemon only opens a `Store<ReadOnly>`. The typestate makes this a
/// compile-time invariant: `prune_all` is on
/// `impl Store<ReadWrite>` so the daemon path cannot accidentally
/// call it. Returns an error instructing the user to run `cqs gc`
/// directly; the dispatcher in `cli/dispatch.rs` already classifies
/// `Commands::Gc` as `BatchSupport::Cli` so this branch is unreachable
/// in practice, but the stub exists to keep the batch command surface
/// complete and to document the invariant.
pub(in crate::cli::batch) fn dispatch_gc(_ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_gc").entered();
    anyhow::bail!(
        "gc requires a writable store; run `cqs gc` outside the daemon. \
         (Commands::Gc is BatchSupport::Cli in dispatch.rs; reaching this \
         branch means a daemon classifier regressed — see #946.)"
    )
}

/// Manually invalidates all mutable caches and re-opens the Store.
///
/// The daemon path early-routes `Refresh` to `view.invalidate_via_outer()`
/// inside `dispatch_via_view` (briefly re-locking the BatchContext) and the
/// stdin batch path early-routes to `BatchContext::invalidate` directly. This
/// handler is the fallback used when the dispatch reaches us via
/// `commands::dispatch` (e.g. in tests). It still uses `invalidate_via_outer`
/// so the daemon contract is enforceable from one place.
pub(in crate::cli::batch) fn dispatch_refresh(ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_refresh").entered();
    ctx.invalidate_via_outer()?;
    Ok(serde_json::json!({"status": "ok", "message": "Caches invalidated, Store re-opened"}))
}

/// Generates help documentation for the BatchInput command and returns it as JSON.
/// # Returns
/// A Result containing a JSON object with a "help" key mapped to the formatted help text for the BatchInput command.
/// # Errors
/// Returns an error if writing help text to the buffer fails or if UTF-8 conversion fails.
pub(in crate::cli::batch) fn dispatch_help(_ctx: &BatchView) -> Result<serde_json::Value> {
    // Takes `&BatchView` to match the unit-variant handler shape even though
    // help generation is fully static. Keeps the macro-driven dispatch table
    // uniform.
    use clap::CommandFactory;
    let mut buf = Vec::new();
    BatchInput::command().write_help(&mut buf)?;
    let help_text = String::from_utf8_lossy(&buf).to_string();
    Ok(serde_json::json!({"help": help_text}))
}

/// Daemon healthcheck — returns the JSON-serialized [`PingResponse`] snapshot.
///
/// Thin wrapper over [`BatchContext::ping_snapshot`]. The handler
/// touches no I/O beyond a single `metadata()` call inside `ping_snapshot`,
/// so it stays cheap even on a very busy daemon — important because the
/// CLI's `cqs ping` may be polled by orchestration scripts.
///
/// [`PingResponse`]: cqs::daemon_translate::PingResponse
pub(in crate::cli::batch) fn dispatch_ping(ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_ping").entered();
    let snapshot = ctx.ping_snapshot();
    serde_json::to_value(&snapshot)
        .map_err(|e| anyhow::anyhow!("Failed to serialize PingResponse: {e}"))
}

/// Watch-mode freshness snapshot — returns the latest
/// [`cqs::watch_status::WatchSnapshot`] the watch loop published. Outside
/// `cqs watch --serve` (one-shot `cqs batch`) this returns the default
/// `unknown` snapshot. Pure read — clones the small struct out from under
/// a `RwLock` read guard and serializes it.
pub(in crate::cli::batch) fn dispatch_status(ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_status").entered();
    let snapshot = ctx.watch_snapshot();
    serde_json::to_value(&snapshot)
        .map_err(|e| anyhow::anyhow!("Failed to serialize WatchSnapshot: {e}"))
}

/// Block until the watch loop transitions to Fresh, or `wait_secs` elapses.
/// One round-trip, zero busy-poll.
///
/// Behaviour:
/// 1. Read the current snapshot. If already Fresh, return it immediately
///    (no parking) — an already-fresh tree pays no latency.
/// 2. Otherwise park on the shared [`cqs::watch_status::FreshNotifier`]
///    until either the next `false → true` transition fires
///    `notify_all`, or `deadline` expires. The waiter loop in
///    `FreshNotifier::wait_until_fresh` re-checks the predicate after
///    every wake (handles spurious wake-ups + the rare `true → false`
///    flip mid-wait).
/// 3. Return the snapshot in either case — the caller distinguishes
///    "Fresh" vs "Timeout" by inspecting the snapshot's `state` field.
///    On wake the snapshot is read AFTER the wait, so the returned
///    payload reflects the latest publish — not whatever was visible
///    when the request first parked.
///
/// `wait_secs == 0` returns immediately with the current snapshot
/// (deadline-first check on a zero budget). Capped at 86_400 s (24 h) for
/// parity with the client-side `wait_for_fresh` cap.
pub(in crate::cli::batch) fn dispatch_wait_fresh(
    ctx: &BatchView,
    args: &WaitFreshArgs,
) -> Result<serde_json::Value> {
    let wait_secs = args.wait_secs;
    let bounded_secs = wait_secs.min(86_400);
    let _span = tracing::info_span!("batch_wait_fresh", wait_secs, bounded_secs,).entered();
    let start = std::time::Instant::now();

    let initial = ctx.watch_snapshot();
    if initial.is_fresh() {
        tracing::info!("wait_fresh: already fresh on entry");
        return serde_json::to_value(&initial)
            .map_err(|e| anyhow::anyhow!("Failed to serialize WatchSnapshot: {e}"));
    }

    let deadline = start + std::time::Duration::from_secs(bounded_secs);
    let notifier = ctx.fresh_notifier();
    let woke_fresh = notifier.wait_until_fresh(deadline);

    let snap = ctx.watch_snapshot();
    if woke_fresh {
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            modified_files = snap.modified_files,
            "wait_fresh: woke on Fresh transition"
        );
    } else {
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            modified_files = snap.modified_files,
            pending_notes = snap.pending_notes,
            rebuild_in_flight = snap.rebuild_in_flight,
            "wait_fresh: deadline reached without Fresh transition"
        );
    }

    serde_json::to_value(&snap)
        .map_err(|e| anyhow::anyhow!("Failed to serialize WatchSnapshot: {e}"))
}

/// Git-hook-driven reconcile request. Flips the shared
/// `SharedReconcileSignal` AtomicBool to `true`; the watch loop swaps it
/// back to `false` and runs an immediate reconcile pass on its next tick.
///
/// The `hook` and `args` fields are advisory — they ride along for tracing
/// (so `journalctl --user-unit cqs-watch` shows which hook fired) but
/// don't change the reconcile algorithm. Returning the parameters in the
/// envelope makes the hook script's stderr useful when debugging
/// (`cqs hook fire ... --json | jq`).
///
/// `was_pending`: `true` if a previous request was still un-drained when
/// this call arrived. Always-`true` is fine — the watch loop coalesces
/// repeated requests into one walk, which is the right behavior for a
/// burst of git operations (e.g. `git rebase -i` firing post-rewrite once
/// per replayed commit).
pub(in crate::cli::batch) fn dispatch_reconcile(
    ctx: &BatchView,
    args: &ReconcileArgs,
) -> Result<serde_json::Value> {
    let hook = args.hook.as_deref();
    let _span =
        tracing::info_span!("batch_reconcile", hook = hook.unwrap_or("(unknown)")).entered();
    let was_pending = ctx.request_reconcile();
    tracing::info!(
        hook = hook.unwrap_or("(unknown)"),
        args_count = args.args.len(),
        was_pending,
        "Reconcile requested"
    );
    // No `queued` field on the wire: `Ok(...)` already conveys "accepted by
    // daemon".
    Ok(serde_json::json!({
        "was_pending": was_pending,
        "hook": args.hook,
        "args": args.args,
    }))
}

// Embedder-free misc handler tests. `dispatch_ping`, `dispatch_help`, and
// `dispatch_refresh` are the cheap healthcheck/metadata surface. Pin the
// contract here so a future regression in
// `BatchContext::ping_snapshot` or the help text emitter surfaces locally.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::batch::{checkout_view_from_arc, create_test_context, BatchContext, BatchView};
    use cqs::store::{ModelInfo, Store};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    /// Build a `BatchContext` wrapped in an `Arc<Mutex<...>>` plus a
    /// `BatchView` carrying it as `outer_lock`. Mirrors the daemon path so
    /// `dispatch_refresh` (which goes through `invalidate_via_outer`) can
    /// reach a real BatchContext to invalidate.
    fn empty_view() -> (TempDir, Arc<Mutex<BatchContext>>, BatchView) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
        }
        let ctx = create_test_context(&cqs_dir).expect("ctx");
        let arc = Arc::new(Mutex::new(ctx));
        let view = checkout_view_from_arc(&arc);
        (dir, arc, view)
    }

    #[test]
    fn dispatch_ping_returns_serializable_snapshot() {
        let (_dir, _ctx, view) = empty_view();
        let json = dispatch_ping(&view).expect("dispatch_ping");
        assert!(
            json.is_object(),
            "ping must serialize as a JSON object, got: {json}"
        );
        let obj = json.as_object().unwrap();
        assert!(!obj.is_empty(), "ping snapshot must not be empty");
    }

    /// `dispatch_wait_fresh` returns immediately when the snapshot is already
    /// Fresh on entry — no parking, no Condvar wait. Pins the "already-fresh
    /// tree pays no latency" contract.
    #[test]
    fn dispatch_wait_fresh_returns_immediately_when_already_fresh() {
        let (_dir, _ctx, view) = empty_view();
        // Seed the shared snapshot with a Fresh state. The handler reads
        // through the same `Arc<RwLock<...>>`, so updates here are
        // immediately visible.
        let fresh_snap = cqs::watch_status::WatchSnapshot {
            state: cqs::watch_status::FreshnessState::Fresh,
            modified_files: 0,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 42,
            dropped_this_cycle: 0,
            last_event_unix_secs: 1_700_000_000,
            last_synced_at: Some(1_700_000_000),
            snapshot_at: Some(1_700_000_001),
            active_slot: None,
            ops: None,
        };
        // Reach into the lock through the view's clone of the Arc.
        // (Tests under `cqs::watch_status` write directly; here we
        // mutate via the `BatchView`-internal field by name.)
        view.test_overwrite_watch_snapshot(fresh_snap.clone());

        let start = std::time::Instant::now();
        // wait_secs=10 — handler should return well before this.
        let args = WaitFreshArgs { wait_secs: 10 };
        let json = dispatch_wait_fresh(&view, &args).expect("dispatch_wait_fresh");
        let elapsed_ms = start.elapsed().as_millis();

        assert!(
            elapsed_ms < 100,
            "dispatch_wait_fresh on already-fresh snapshot must return immediately; took {elapsed_ms}ms"
        );
        assert_eq!(json.get("state").and_then(|v| v.as_str()), Some("fresh"));
        assert_eq!(
            json.get("incremental_count").and_then(|v| v.as_u64()),
            Some(42)
        );
    }

    /// `dispatch_wait_fresh` parks until a `false → true`
    /// transition fires `notify_all`. Spawns a "watch loop" thread that
    /// flips the notifier after a short sleep; the handler must wake
    /// promptly and return the freshly-published snapshot.
    #[test]
    fn dispatch_wait_fresh_wakes_on_notifier_flip() {
        let (_dir, _ctx, view) = empty_view();
        // Clone the Arc handles that the publisher thread will use.
        // BatchView itself is not Clone (it caches a few inner Arcs),
        // but the shared notifier and snapshot are exactly what the
        // watch loop holds — so cloning those is sufficient.
        let notifier_for_publisher = view.fresh_notifier();
        let snap_handle_for_publisher = view.test_watch_snapshot_handle();
        let publisher = std::thread::spawn(move || {
            // Wall-clock gap chosen to be >> the handler's 0 → park time
            // but small enough to keep the test fast.
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Flip the cached snapshot to Fresh, then notify.
            let fresh = cqs::watch_status::WatchSnapshot {
                state: cqs::watch_status::FreshnessState::Fresh,
                modified_files: 0,
                pending_notes: false,
                rebuild_in_flight: false,
                delta_saturated: false,
                incremental_count: 99,
                dropped_this_cycle: 0,
                last_event_unix_secs: 1_700_000_000,
                last_synced_at: Some(1_700_000_000),
                snapshot_at: Some(1_700_000_002),
                active_slot: None,
                ops: None,
            };
            *snap_handle_for_publisher
                .write()
                .unwrap_or_else(|p| p.into_inner()) = fresh;
            notifier_for_publisher.set_fresh(true);
        });

        let start = std::time::Instant::now();
        let args = WaitFreshArgs { wait_secs: 5 };
        let json = dispatch_wait_fresh(&view, &args).expect("dispatch_wait_fresh");
        let elapsed_ms = start.elapsed().as_millis();
        publisher.join().expect("publisher thread");

        // Should have woken on the notifier well within the 5 s budget,
        // and well after the publisher's 50 ms sleep.
        assert!(
            (40..2000).contains(&elapsed_ms),
            "dispatch_wait_fresh wake latency outside expected window: {elapsed_ms}ms"
        );
        assert_eq!(json.get("state").and_then(|v| v.as_str()), Some("fresh"));
        assert_eq!(
            json.get("incremental_count").and_then(|v| v.as_u64()),
            Some(99)
        );
    }

    /// `dispatch_wait_fresh` returns the still-stale
    /// snapshot when `wait_secs` runs out without a Fresh transition.
    /// Tight `wait_secs=1` keeps the test fast; the handler returns
    /// after ~1 s.
    #[test]
    fn dispatch_wait_fresh_returns_stale_snapshot_on_deadline() {
        let (_dir, _ctx, view) = empty_view();
        let stale = cqs::watch_status::WatchSnapshot {
            state: cqs::watch_status::FreshnessState::Stale,
            modified_files: 7,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 0,
            dropped_this_cycle: 0,
            last_event_unix_secs: 1_700_000_000,
            last_synced_at: Some(1_700_000_000),
            snapshot_at: Some(1_700_000_003),
            active_slot: None,
            ops: None,
        };
        view.test_overwrite_watch_snapshot(stale);

        let start = std::time::Instant::now();
        let args = WaitFreshArgs { wait_secs: 1 };
        let json = dispatch_wait_fresh(&view, &args).expect("dispatch_wait_fresh");
        let elapsed = start.elapsed();

        // Handler must wait the full second (within scheduler jitter)
        // before returning the stale snapshot.
        assert!(
            elapsed >= std::time::Duration::from_millis(900),
            "dispatch_wait_fresh exited too early: {}ms",
            elapsed.as_millis()
        );
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "dispatch_wait_fresh exited too late: {}ms",
            elapsed.as_millis()
        );
        assert_eq!(
            json.get("state").and_then(|v| v.as_str()),
            Some("stale"),
            "must return the post-deadline snapshot, got: {json}"
        );
        assert_eq!(json.get("modified_files").and_then(|v| v.as_u64()), Some(7));
    }

    /// `dispatch_status` against an empty `BatchContext` (no watch
    /// loop publishing) returns the default `unknown` snapshot, serialized
    /// to a JSON object matching the [`WatchSnapshot`] shape. The handler
    /// must not block, fail, or read disk — it's a pure RwLock-guarded
    /// clone of the in-memory snapshot.
    #[test]
    fn dispatch_status_returns_unknown_when_no_watch_loop() {
        let (_dir, _ctx, view) = empty_view();
        let json = dispatch_status(&view).expect("dispatch_status");
        assert!(
            json.is_object(),
            "status must serialize as a JSON object, got: {json}"
        );
        let obj = json.as_object().unwrap();
        assert_eq!(
            obj.get("state").and_then(|v| v.as_str()),
            Some("unknown"),
            "fresh context with no watch loop must report state=unknown, got: {json}"
        );
        // Pin a couple of stable shape invariants so a future field rename
        // (e.g. `modified_files` → `pending`) trips this test.
        assert!(
            obj.contains_key("modified_files"),
            "snapshot must carry `modified_files` field"
        );
        assert!(
            obj.contains_key("snapshot_at"),
            "snapshot must carry `snapshot_at` timestamp"
        );
        // snapshot_at is `Option<i64>` — serializes to a JSON number
        // on a healthy clock, JSON null on a clock-before-epoch system. CI
        // and dev workstations pass the healthy-clock path, so pin
        // `is_number()` to catch a regression that flips the wire shape.
        assert!(
            obj.get("snapshot_at")
                .map(|v| v.is_number())
                .unwrap_or(false),
            "snapshot_at must be a JSON number on a healthy clock; got: {:?}",
            obj.get("snapshot_at")
        );
    }

    /// `dispatch_reconcile` flips the shared
    /// `SharedReconcileSignal` AtomicBool. The handler is otherwise
    /// pure: no store access, no embedder. The view's reconcile_signal
    /// Arc is shared with the BatchContext's, so we can assert state
    /// from outside.
    #[test]
    fn dispatch_reconcile_flips_signal_and_reports_was_pending() {
        let (_dir, ctx, view) = empty_view();
        // Capture a clone of the signal before dispatch so the test can
        // observe the flip without holding the BatchView's borrow.
        let signal = {
            let g = ctx.lock().unwrap();
            std::sync::Arc::clone(&g.reconcile_signal)
        };

        // Initially false.
        assert!(!signal.load(std::sync::atomic::Ordering::Acquire));

        // First dispatch flips it; was_pending must be false.
        let args1 = ReconcileArgs {
            hook: Some("post-checkout".to_string()),
            args: vec!["abc".to_string(), "def".to_string(), "1".to_string()],
        };
        let json = dispatch_reconcile(&view, &args1).expect("dispatch_reconcile #1");
        // No `queued` field; Ok(...) implies queued.
        assert!(
            json.get("queued").is_none(),
            "queued field should be removed"
        );
        assert_eq!(
            json.get("was_pending").and_then(|v| v.as_bool()),
            Some(false),
            "first reconcile request must report was_pending=false, got: {json}"
        );
        assert_eq!(
            json.get("hook").and_then(|v| v.as_str()),
            Some("post-checkout")
        );
        assert!(signal.load(std::sync::atomic::Ordering::Acquire));

        // Second dispatch (without the loop draining the flag in
        // between) coalesces — was_pending must be true.
        let args2 = ReconcileArgs {
            hook: Some("post-merge".to_string()),
            args: Vec::new(),
        };
        let json2 = dispatch_reconcile(&view, &args2).expect("dispatch_reconcile #2");
        assert_eq!(
            json2.get("was_pending").and_then(|v| v.as_bool()),
            Some(true),
            "second reconcile request before drain must report was_pending=true, got: {json2}"
        );
    }

    #[test]
    fn dispatch_reconcile_with_no_hook_still_queues() {
        // `cqs hook fire` always passes a hook name, but the handler
        // must not require one — hand-rolled `cqs batch reconcile`
        // sessions skip it.
        let (_dir, _ctx, view) = empty_view();
        let args = ReconcileArgs {
            hook: None,
            args: Vec::new(),
        };
        let json = dispatch_reconcile(&view, &args).expect("dispatch_reconcile");
        // No `queued` field; Ok(...) implies queued.
        assert!(
            json.get("queued").is_none(),
            "queued field should be removed"
        );
        assert!(json.get("hook").is_some_and(|v| v.is_null()));
    }

    #[test]
    fn dispatch_help_carries_help_text() {
        let (_dir, _ctx, view) = empty_view();
        let json = dispatch_help(&view).expect("dispatch_help");
        let help = json
            .get("help")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("response must carry `help` string, got: {json}"));
        // Help output must mention at least one batch-mode command name.
        // `search` is a stable label.
        assert!(
            help.to_lowercase().contains("search"),
            "help text must mention at least one command, got: {help}"
        );
    }

    #[test]
    fn dispatch_refresh_succeeds_on_empty_store() {
        let (_dir, _ctx, view) = empty_view();
        let json = dispatch_refresh(&view).expect("dispatch_refresh");
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "refresh must return status:ok, got: {json}"
        );
    }
}
