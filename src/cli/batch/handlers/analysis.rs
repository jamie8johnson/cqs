//! Analysis dispatch handlers: dead, health, stale, suggest, review, ci.
//!
//! Handlers take a single `&XArgs` argument so the macro-driven
//! `BatchCmd::dispatch` calls every row uniformly.

use std::path::Path;

use anyhow::Result;

use super::super::BatchView;
use crate::cli::args::{CiArgs, DeadArgs, ReviewArgs, StaleArgs, SuggestArgs};

/// Identifies and reports dead code in a codebase.
/// Analyzes code to find functions that are never called, filtering results based on confidence level and visibility. Returns structured JSON containing categorized dead code findings.
/// # Arguments
/// * `ctx` - Batch context containing the code store and root directory path
/// * `include_pub` - Whether to include public functions in the dead code analysis
/// * `min_confidence` - Minimum confidence threshold for including results
/// # Returns
/// A JSON object with four fields:
/// - `dead`: Array of confidently identified dead functions
/// - `possibly_dead_pub`: Array of possibly dead public functions
/// - `count`: Count of confidently dead functions
/// - `possibly_pub_count`: Count of possibly dead public functions
/// Each function entry includes name, file path, line range, type, signature, language, and confidence level.
/// # Errors
/// Returns an error if the code store query fails.
pub(in crate::cli::batch) fn dispatch_dead(
    ctx: &BatchView,
    args: &DeadArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_dead").entered();

    // See `dispatch_callers`: clear leftover per-thread overlay meta before any
    // branch can return, so a reused daemon worker never leaks a prior query's
    // `_meta.worktree_overlay`. (`resolve_graph_overlay` clears it too, but
    // clearing first keeps the discipline uniform across handlers.)
    cqs::worktree_overlay::clear_overlay_meta();

    // Thin adapter over the shared `dead_overlay` — identical JSON shape across
    // the CLI and daemon surfaces.
    let verdict = match args.verdict.as_deref() {
        None => None,
        // A bad `--verdict` is user input → invalid_input client-facing so the
        // daemon surfaces the "invalid verdict" message (naming only the
        // caller-supplied value + the valid set) with that code, not an opaque
        // internal chain-id.
        Some(s) => Some(crate::cli::commands::DeadVerdict::parse(s).map_err(|e| {
            crate::cli::json_envelope::ClientFacingError::new(
                crate::cli::json_envelope::ErrorCode::InvalidInput,
                e,
            )
        })?),
    };
    let core_args = crate::cli::commands::DeadArgs {
        include_pub: args.include_pub,
        min_confidence: args.min_confidence,
        verdict,
    };
    // Resolve the worktree overlay (Part B): recomputes the dead set over the
    // merged caller graph. `None` ⇒ parent-truth (the default).
    let overlay = super::graph::resolve_graph_overlay(ctx, &args.overlay)?;
    let (output, overlay_participated) = crate::cli::commands::dead_overlay(
        &ctx.store(),
        &ctx.root,
        &core_args,
        overlay.as_deref(),
    )?;
    let mut value = serde_json::to_value(&output)?;
    if overlay_participated {
        // `dead`'s answer is fully determined by the merged caller graph (no
        // transitive/test/type sections), so `"full"` is honest here. Gated on
        // participation, not `overlay.is_some()`: a dirty worktree whose delta
        // flips no dead verdict returns the parent dead set untouched and carries
        // NO marker.
        super::attach_overlay_graph_meta_full(&mut value);
    }
    Ok(value)
}

/// Dispatches a request to identify stale and missing files in the batch store.
/// Retrieves the file set from the batch context and queries the store for files whose modification times have changed or are no longer present on disk. Returns a JSON report containing lists of stale files with their stored and current modification times, missing files, and summary statistics.
/// # Arguments
/// * `ctx` - The batch context containing the store and file set information.
/// * `count_only` - If true, emits only count fields (stale_count, missing_count,
///   total_indexed), omitting the per-file arrays. Matches the CLI `--count-only`
///   flag so forwarded invocations parse cleanly.
/// # Returns
/// A JSON object containing:
/// - `stale`: Array of stale files with their origin path, stored mtime, and current mtime (omitted if `count_only`)
/// - `missing`: Array of missing file paths (omitted if `count_only`)
/// - `total_indexed`: Total number of indexed files
/// - `stale_count`: Count of stale files
/// - `missing_count`: Count of missing files
/// # Errors
/// Returns an error if the file set cannot be retrieved from the context or if the store query fails.
pub(in crate::cli::batch) fn dispatch_stale(
    ctx: &BatchView,
    args: &StaleArgs,
) -> Result<serde_json::Value> {
    let count_only = args.count_only;
    let _span = tracing::info_span!("batch_stale", count_only).entered();

    // `file_set` is `Arc<HashSet<PathBuf>>` — the daemon keeps it cached so we
    // avoid re-enumerating the tree on every probe. `stale_core` takes the set
    // by ref so this hot path reuses the cache (the CLI enumerates once).
    let file_set = ctx.file_set()?;
    let output = crate::cli::commands::stale_core(
        &ctx.store(),
        &ctx.root,
        &file_set,
        &crate::cli::commands::StaleArgs::default(),
    )?;
    // `count_only` is the adapter-side render flag (read off the wire
    // `args::StaleArgs`, not the core Args): project to the 3-field count
    // subset here, after the core has computed the full report.
    if count_only {
        Ok(serde_json::json!({
            "stale_count": output.stale_count,
            "missing_count": output.missing_count,
            "total_indexed": output.total_indexed,
        }))
    } else {
        Ok(serde_json::to_value(&output)?)
    }
}

/// Performs a health check on the batch processing system and returns the results as JSON.
/// This function executes a comprehensive health check that validates the store, file set, and CQS directory, then serializes the health report to a JSON value for reporting purposes.
/// # Arguments
/// * `ctx` - The batch processing context containing the store, file set, and CQS directory paths.
/// # Returns
/// A `Result` containing a `serde_json::Value` representing the health check report, or an error if the health check fails or serialization fails.
/// # Errors
/// Returns an error if retrieving the file set fails, if the health check itself fails, or if serializing the report to JSON fails.
pub(in crate::cli::batch) fn dispatch_health(ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_health").entered();

    // `Arc<HashSet>` auto-derefs through `&file_set` for the borrowed-only
    // callee. Thin adapter over the shared `health_core`.
    let file_set = ctx.file_set()?;
    let report = crate::cli::commands::health_core(
        &ctx.store(),
        &file_set,
        &ctx.cqs_dir,
        &ctx.root,
        &crate::cli::commands::HealthArgs::default(),
    )?;

    Ok(serde_json::to_value(&report)?)
}

/// Suggests notes from codebase patterns. `--apply` is rejected on the
/// daemon path.
///
/// The daemon holds a `Store<ReadOnly>`, so the reindex half of
/// `--apply` (which goes through `index_notes` → `replace_notes_for_file`)
/// cannot compile here. `Commands::Suggest` is classified as
/// `BatchSupport::Cli` precisely because of this — a user who runs
/// `cqs suggest --apply` falls through to the CLI path, which opens a
/// `Store<ReadWrite>`. Bailing when `apply=true` documents that
/// invariant instead of silently producing a stale (unapplied) result.
pub(in crate::cli::batch) fn dispatch_suggest(
    ctx: &BatchView,
    args: &SuggestArgs,
) -> Result<serde_json::Value> {
    let apply = args.apply;
    let _span = tracing::info_span!("batch_suggest", apply).entered();

    if apply {
        anyhow::bail!(
            "suggest --apply requires a writable store; run `cqs suggest --apply` outside \
             the daemon. (Commands::Suggest is BatchSupport::Cli; reaching this branch \
             means a classifier regressed — see #946.)"
        );
    }

    // Thin adapter over the shared `suggest_core` — converges the daemon JSON
    // onto the CLI's typed `SuggestOutput` schema (`count` replaces the old
    // `total`; per-entry shape unchanged).
    let output = crate::cli::commands::suggest_core(
        &ctx.store(),
        &ctx.root,
        &crate::cli::commands::SuggestArgs::default(),
    )?;
    Ok(serde_json::to_value(&output)?)
}

// ─── eval single-flight ──────────────────────────────────────────────────────

/// At most one eval may run daemon-wide. A full fixture run holds real GPU time
/// for minutes; a second concurrent submission (a fallback retry, an MCP client
/// retry, a racing tool call) would double up on the device outside the
/// per-embed serialization the fairness argument relies on. The flag is flipped
/// by [`EvalSlotGuard::try_acquire`] and released on drop — including panic
/// unwind — so a failed or panicking run never wedges the slot.
static EVAL_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII single-flight token for [`dispatch_eval`]. `try_acquire` returns `None`
/// when an eval is already in flight; the returned guard releases the slot on
/// drop.
struct EvalSlotGuard;

impl EvalSlotGuard {
    /// Claim the single-flight slot, or `None` if an eval is already running.
    fn try_acquire() -> Option<EvalSlotGuard> {
        use std::sync::atomic::Ordering;
        EVAL_IN_FLIGHT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then_some(EvalSlotGuard)
    }
}

impl Drop for EvalSlotGuard {
    fn drop(&mut self) {
        EVAL_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Uniform, opaque rejection for a fixture the daemon must not read or parse.
/// Traversal (out-of-root) AND an in-root non-JSON file both surface THIS error,
/// so the daemon is never an oracle for whether an in-root path exists or holds
/// valid JSON, and never echoes the host path.
fn eval_fixture_rejected() -> anyhow::Error {
    crate::cli::json_envelope::ClientFacingError::new(
        crate::cli::json_envelope::ErrorCode::InvalidInput,
        "eval query_file is not a readable in-root JSON fixture",
    )
    .into()
}

/// Read + parse the fixture, OWNING the read so `eval_core` never touches the
/// path. Canonicalize + `starts_with` the served root, read the bytes, parse the
/// JSON — ANY failure collapses to the same opaque [`eval_fixture_rejected`]
/// error (no in-root content oracle, no path echo).
fn read_fixture_within_root(query_file: &Path, root: &Path) -> Result<cqs::eval::schema::QuerySet> {
    let canonical = dunce::canonicalize(query_file).map_err(|_| eval_fixture_rejected())?;
    let root_canonical = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !canonical.starts_with(&root_canonical) {
        return Err(eval_fixture_rejected());
    }
    let raw = std::fs::read_to_string(&canonical).map_err(|_| eval_fixture_rejected())?;
    serde_json::from_str(&raw).map_err(|_| eval_fixture_rejected())
}

/// Relativize the fixture path to the served root for the wire `query_file`
/// label — the absolute host path never rides the wire. Falls back to the bare
/// file name if the strip fails (it should not: the guard already confirmed
/// in-root membership before this runs).
fn relativize_query_file(query_file: &Path, root: &Path) -> String {
    let canonical = dunce::canonicalize(query_file).unwrap_or_else(|_| query_file.to_path_buf());
    let root_canonical = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    canonical
        .strip_prefix(&root_canonical)
        .ok()
        .map(|rel| rel.to_string_lossy().to_string())
        .or_else(|| {
            query_file
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        })
        .unwrap_or_default()
}

/// Run the eval harness on the daemon — the surface behind the MCP `cqs_eval`
/// tool and the `cmd_eval` relay dial. Serves the run from the daemon's
/// RESIDENT index (`index_substrate = "resident"`) with a single-flight cap and
/// an in-root path guard.
///
/// Degradation + sharing mode: at most one eval runs daemon-wide; a second
/// submission gets a typed retryable-busy (`unavailable`) BEFORE any read/embed
/// work. There is NO parked freshness wait — a minutes-long park would sit on
/// the single-flight slot; the output instead carries the watch-loop freshness
/// snapshot so the consumer decides. A disconnected client does not abort a
/// server-side run (bounded by single-flight). Selecting `reranker = onnx`
/// triggers the daemon's one-time cross-encoder load, which it can already
/// incur on the search path.
pub(in crate::cli::batch) fn dispatch_eval(
    ctx: &BatchView,
    args: &crate::cli::commands::EvalArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_eval", limit = args.limit).entered();

    // Single-flight: refuse a second concurrent eval BEFORE any read/embed work.
    // The guard releases the slot on drop, including on the `?` early-returns
    // and eval_core panics below.
    let Some(_slot) = EvalSlotGuard::try_acquire() else {
        tracing::warn!("eval rejected: another eval is already in flight (single-flight)");
        return Err(crate::cli::json_envelope::ClientFacingError::new(
            crate::cli::json_envelope::ErrorCode::Unavailable,
            "an eval is already running on this daemon; retry shortly",
        )
        .into());
    };

    // The guard OWNS the read + parse; any failure is the same opaque error, so
    // eval_core receives an already-parsed set and never touches the path.
    let set = read_fixture_within_root(&args.query_file, &ctx.root)?;

    let params = crate::cli::commands::EvalCoreParams {
        query_file_label: relativize_query_file(&args.query_file, &ctx.root),
        category_filter: args.category.clone(),
        limit: args.limit,
        reranker: args.reranker,
        substrate: crate::cli::commands::IndexSubstrate::Resident,
        freshness: Some(crate::cli::commands::EvalFreshness::from_snapshot(
            &ctx.watch_snapshot(),
        )),
    };

    let output = crate::cli::commands::eval_core(ctx, &set, &params)?;
    Ok(serde_json::to_value(&output)?)
}

#[cfg(test)]
mod eval_slot_tests {
    use super::{EvalSlotGuard, EVAL_IN_FLIGHT};
    use std::sync::atomic::Ordering;

    /// The single-flight slot admits one holder; a second `try_acquire` while
    /// the first is held returns `None`, and dropping the first frees it.
    #[test]
    #[serial_test::serial(eval_single_flight)]
    fn single_flight_admits_one_holder() {
        assert!(!EVAL_IN_FLIGHT.load(Ordering::SeqCst), "slot starts free");
        let first = EvalSlotGuard::try_acquire().expect("first acquire wins");
        assert!(
            EvalSlotGuard::try_acquire().is_none(),
            "second concurrent acquire must be refused"
        );
        drop(first);
        assert!(
            !EVAL_IN_FLIGHT.load(Ordering::SeqCst),
            "dropping the sole holder frees the slot"
        );
        // Freed → a fresh acquire succeeds again.
        let again = EvalSlotGuard::try_acquire().expect("slot reusable after release");
        drop(again);
    }

    /// The slot releases even when the holding scope panics (RAII on unwind),
    /// so a panicking eval never wedges the daemon.
    #[test]
    #[serial_test::serial(eval_single_flight)]
    fn single_flight_releases_after_panic() {
        assert!(!EVAL_IN_FLIGHT.load(Ordering::SeqCst), "slot starts free");
        let panicked = std::panic::catch_unwind(|| {
            let _slot = EvalSlotGuard::try_acquire().expect("acquire before panic");
            assert!(EVAL_IN_FLIGHT.load(Ordering::SeqCst), "slot held pre-panic");
            panic!("eval blew up mid-run");
        });
        assert!(panicked.is_err(), "the closure must have panicked");
        assert!(
            !EVAL_IN_FLIGHT.load(Ordering::SeqCst),
            "slot must be released after a panicking run"
        );
    }
}

/// Runs a diff-aware review and returns results as JSON.
/// Executes `git diff` against the given base ref (or HEAD) and runs the
/// review pipeline: diff impact, risk scoring, note matching, staleness.
pub(in crate::cli::batch) fn dispatch_review(
    ctx: &BatchView,
    args: &ReviewArgs,
) -> Result<serde_json::Value> {
    let base = args.base.as_deref();
    let tokens = args.tokens;
    let _span = tracing::info_span!("batch_review", ?base).entered();

    // See `dispatch_dead`: clear leftover per-thread overlay meta before any
    // branch can return, so a reused daemon worker never leaks a prior query's
    // `_meta.worktree_overlay`.
    cqs::worktree_overlay::clear_overlay_meta();

    // Thin adapter over the shared `review_overlay` — same schema + budgeting as
    // the CLI JSON surface. Adapter owns diff I/O (git only, no stdin).
    //
    // Resolve the worktree overlay (Part B PR3) FIRST, then acquire the diff from
    // the SAME tree the overlay describes: `run_git_diff_for_overlay` diffs
    // `overlay.worktree_root` when present, else the served project (`ctx.root`).
    // Order matters — the diff-tree must track the overlay-tree or the review is a
    // chimera (parent diff spliced with the worktree's caller-graph delta). The
    // worktree root is registry-validated by `set_validated_overlay_request`, so
    // git runs only in a vetted root; a `None` overlay ⇒ parent-truth (the
    // default), diffing `ctx.root` NOT the daemon's launch cwd.
    let overlay = super::graph::resolve_graph_overlay(ctx, &args.overlay)?;
    let diff_text = crate::cli::commands::run_git_diff_for_overlay(
        base,
        &ctx.root,
        overlay.as_ref().map(|o| o.worktree_root.as_path()),
    )?;
    let (output, overlay_participated) = crate::cli::commands::review_overlay(
        &ctx.store(),
        &ctx.root,
        &diff_text,
        &crate::cli::commands::ReviewArgs { tokens },
        overlay.as_deref(),
    )?;
    let mut value = serde_json::to_value(&output)?;
    if overlay_participated {
        // review's direct-callers section reflects the worktree delta, but its
        // affected-tests + per-function risk-scoring sections stay parent-truth
        // (a fully-merged call graph is a separate surface), so `"callers-only"`
        // is the honest marker — mirroring `impact`. Gated on participation, not
        // `overlay.is_some()`: a dirty worktree whose delta touches no caller of
        // any changed function returns the parent review untouched, NO marker.
        super::attach_overlay_graph_meta_callers_only(&mut value);
    }
    Ok(value)
}

/// Runs CI analysis (review + dead code + gate) and returns results as JSON.
/// Note: In batch mode, gate failure is reported in the JSON output rather than
/// causing a process exit, since the batch session must continue.
pub(in crate::cli::batch) fn dispatch_ci(
    ctx: &BatchView,
    args: &CiArgs,
) -> Result<serde_json::Value> {
    let base = args.base.as_deref();
    let gate = args.gate;
    let tokens = args.tokens;
    let _span = tracing::info_span!("batch_ci", ?gate).entered();

    // See `dispatch_dead`: clear leftover per-thread overlay meta before any
    // branch can return, so a reused daemon worker never leaks a prior query's
    // `_meta.worktree_overlay`.
    cqs::worktree_overlay::clear_overlay_meta();

    // Thin adapter over the shared `ci_overlay` — same schema + budgeting as the
    // CLI JSON surface. Gate failure is reported in the JSON (no exit) because
    // the batch session must continue. Adapter owns diff I/O (git only).
    //
    // Resolve the worktree overlay FIRST, then acquire the diff from the SAME tree
    // the overlay describes (see `dispatch_review`): `run_git_diff_for_overlay`
    // diffs `overlay.worktree_root` when present, else `ctx.root`. A parent diff
    // spliced with the worktree's caller-graph + dead delta would gate the wrong
    // tree. `None` overlay ⇒ parent-truth, diffing `ctx.root` NOT the launch cwd.
    let overlay = super::graph::resolve_graph_overlay(ctx, &args.overlay)?;
    let diff_text = crate::cli::commands::run_git_diff_for_overlay(
        base,
        &ctx.root,
        overlay.as_ref().map(|o| o.worktree_root.as_path()),
    )?;
    let (output, overlay_participated) = crate::cli::commands::ci_overlay(
        &ctx.store(),
        &ctx.root,
        &diff_text,
        gate,
        &crate::cli::commands::CiArgs { tokens },
        overlay.as_deref(),
    )?;
    let mut value = serde_json::to_value(&output)?;
    if overlay_participated {
        // `cqs ci` bundles a `"callers-only"` review (its tests + risk sections
        // stay parent-truth) and a `"full"` dead component. The weakest component
        // bounds the honest composite claim, so the combined marker is
        // `"callers-only"` — NEVER `"full"`, which would over-promise the review's
        // tests/risk sections as delta-aware. Gated on participation (review OR
        // dead consulted the delta), not `overlay.is_some()`: a dirty worktree
        // whose delta flips neither section returns the parent report, NO marker.
        super::attach_overlay_graph_meta_callers_only(&mut value);
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Parity tests — each `dispatch_*` is a thin adapter over the shared `*_core`,
// so the daemon JSON equals `serde_json::to_value(*_core(...))` for the same
// inputs. This equality is parity-BY-CONSTRUCTION: the dispatcher calls the
// very core it's compared against, so the byte-equality is structurally
// guaranteed, not independently verified. The dead/health tests therefore also
// carry a fixture-grounded value assert (core output reflects the seeded
// `foo` chunk) so a both-sides-empty regression — the failure mode
// by-construction parity can't catch — still fails the test. dead/health/
// suggest are embedder-free and git-free (they read the store directly), so
// the full dispatch path is exercised. review/ci acquire their diff via
// `run_git_diff` (needs a real repo with a diff), so their core-equivalence is
// asserted at the core level against the empty-diff convergence shape.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod parity_tests {
    use crate::cli::batch::create_test_context;
    use cqs::embedder::Embedding;
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::{DeadConfidence, ModelInfo, Store};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_chunk(id: &str, name: &str) -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            byte_start: 0,
            content_hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    fn seed_minimal_ctx() -> (TempDir, crate::cli::batch::BatchContext) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
            let chunks = vec![(make_chunk("src/lib.rs:1:foo", "foo"), embedding)];
            store.upsert_chunks_batch(&chunks, Some(0)).expect("upsert");
        }
        let ctx = create_test_context(&cqs_dir).expect("ctx");
        (dir, ctx)
    }

    /// `dispatch_dead` equals `serde_json::to_value(dead_core(...))` (parity by
    /// construction). The fixture-grounded assert pins that the seeded
    /// callerless non-pub `foo` actually surfaces as dead, so both sides can't
    /// silently converge on an empty list.
    #[test]
    fn parity_dead_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let core = crate::cli::commands::dead_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
                verdict: None,
            },
        )
        .expect("dead_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        // Fixture-grounded: `foo` has no callers and isn't pub → dead.
        let dead = core_val["dead"]
            .as_array()
            .unwrap_or_else(|| panic!("dead core must carry a dead array: {core_val}"));
        assert!(
            dead.iter().any(|d| d["name"] == "foo"),
            "seeded callerless `foo` must be reported dead: {core_val}"
        );

        let dispatched = super::dispatch_dead(
            &view,
            &crate::cli::args::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
                verdict: None,
                overlay: Default::default(),
            },
        )
        .expect("dispatch_dead");

        assert_eq!(dispatched, core_val, "dispatch_dead must equal dead_core");
    }

    /// Seed a context whose index holds a candidate-ONLY callee `maybe_fn`
    /// (zero `function_calls` edges, one `candidate_edges` row) so the
    /// candidate-edge consult (Lane 3) has something to flip. Returns the temp
    /// dir (kept alive) and the context.
    fn seed_candidate_only_ctx() -> (TempDir, crate::cli::batch::BatchContext) {
        use cqs::parser::CandidateSite;
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
            let chunks = vec![(make_chunk("src/lib.rs:1:maybe_fn", "maybe_fn"), embedding)];
            store.upsert_chunks_batch(&chunks, Some(0)).expect("upsert");
            // A `candidate_edges` row naming `maybe_fn`: the consult must relabel
            // it `low-confidence-live` instead of `dead`.
            store
                .upsert_candidate_edges(
                    std::path::Path::new("src/caller.rs"),
                    &[CandidateSite {
                        file: std::path::PathBuf::from("src/caller.rs"),
                        callee_name: "maybe_fn".to_string(),
                        ref_line: 9,
                        candidate_kind: "bare_arg_unresolved".to_string(),
                    }],
                )
                .expect("upsert candidate");
        }
        let ctx = create_test_context(&cqs_dir).expect("ctx");
        (dir, ctx)
    }

    /// CLI==daemon parity on a candidate-bearing fixture: a candidate-only callee
    /// surfaces as `low-confidence-live` (not `dead`) on BOTH surfaces, and the
    /// daemon JSON byte-equals the core. Fixture-grounded so a both-sides-empty
    /// or both-sides-wrong-verdict regression still fails.
    #[test]
    fn parity_dead_candidate_only_low_confidence_live() {
        let (_dir, ctx) = seed_candidate_only_ctx();
        let view = ctx.build_view(None);

        let core = crate::cli::commands::dead_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
                verdict: None,
            },
        )
        .expect("dead_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        // Fixture-grounded: `maybe_fn` is present and classified
        // `low-confidence-live` (the candidate consult flipped it from `dead`),
        // with the candidate kind named in the reason.
        let entry = core_val["dead"]
            .as_array()
            .unwrap_or_else(|| panic!("dead core must carry a dead array: {core_val}"))
            .iter()
            .find(|d| d["name"] == "maybe_fn")
            .unwrap_or_else(|| panic!("candidate-only `maybe_fn` must surface: {core_val}"));
        assert_eq!(
            entry["verdict"], "low-confidence-live",
            "candidate-only callee must be low-confidence-live, not dead: {entry}"
        );
        let reason = entry["verdict_reason"].as_str().unwrap_or("");
        assert!(
            reason.contains("candidate edge") && reason.contains("bare_arg_unresolved"),
            "reason must name the candidate kind: {reason}"
        );

        let dispatched = super::dispatch_dead(
            &view,
            &crate::cli::args::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
                verdict: None,
                overlay: Default::default(),
            },
        )
        .expect("dispatch_dead");

        assert_eq!(
            dispatched, core_val,
            "dispatch_dead must equal dead_core on a candidate-bearing fixture"
        );
    }

    /// `dispatch_health` equals `serde_json::to_value(health_core(...))` (parity
    /// by construction). The fixture-grounded assert pins that the seeded chunk
    /// is counted, guarding against both sides reporting an empty index.
    #[test]
    fn parity_health_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let file_set = view.file_set().expect("file_set");
        let core = crate::cli::commands::health_core(
            &view.store(),
            &file_set,
            &view.cqs_dir,
            &view.root,
            &crate::cli::commands::HealthArgs::default(),
        )
        .expect("health_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        // Fixture-grounded: the index holds the seeded `foo` chunk.
        assert!(
            core_val["stats"]["total_chunks"]
                .as_u64()
                .is_some_and(|n| n >= 1),
            "health core must count the seeded chunk: {core_val}"
        );

        let dispatched = super::dispatch_health(&view).expect("dispatch_health");

        assert_eq!(
            dispatched, core_val,
            "dispatch_health must equal health_core"
        );
    }

    /// `dispatch_suggest` (apply=false) is byte-equal to
    /// `serde_json::to_value(suggest_core(...))`. Pins the daemon-side schema
    /// convergence (`count`, not the old `total`).
    #[test]
    fn parity_suggest_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let core = crate::cli::commands::suggest_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::SuggestArgs::default(),
        )
        .expect("suggest_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        let dispatched =
            super::dispatch_suggest(&view, &crate::cli::args::SuggestArgs { apply: false })
                .expect("dispatch_suggest");

        assert_eq!(
            dispatched, core_val,
            "dispatch_suggest must equal suggest_core (count, not total)"
        );
        // Explicit guard against the pre-unification `total` field re-appearing.
        assert!(
            dispatched.get("total").is_none(),
            "daemon suggest must use `count`, not `total`: {dispatched}"
        );
        assert!(
            dispatched.get("count").is_some(),
            "daemon suggest must carry `count`: {dispatched}"
        );
    }

    /// `review_core` on an empty diff produces the converged empty-review shape
    /// (the daemon's old hand-rolled empty case omitted `relevant_notes` /
    /// `stale_warning`; the core now emits the full CLI shape). `dispatch_review`
    /// is parity-by-construction over `run_git_diff` + this core.
    #[test]
    fn review_core_empty_diff_converged_shape() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let out = crate::cli::commands::review_core(
            &view.store(),
            &view.root,
            "",
            &crate::cli::commands::ReviewArgs { tokens: None },
        )
        .expect("review_core");
        let val = serde_json::to_value(&out).expect("serialize");

        assert!(val.get("changed_functions").is_some());
        assert!(val.get("affected_callers").is_some());
        assert!(val.get("affected_tests").is_some());
        assert!(
            val.get("relevant_notes").is_some(),
            "converged empty review carries relevant_notes: {val}"
        );
        assert_eq!(val["risk_summary"]["overall"], "low");
        // No budget fields on the empty case even though they're optional.
        assert!(val.get("token_count").is_none());
        assert!(val.get("token_budget").is_none());
    }

    /// `ci_core` on an empty diff produces a gate-passing report with no budget
    /// telemetry; `dispatch_ci` is parity-by-construction over `run_git_diff`.
    #[test]
    fn ci_core_empty_diff_shape() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let out = crate::cli::commands::ci_core(
            &view.store(),
            &view.root,
            "",
            cqs::ci::GateThreshold::High,
            &crate::cli::commands::CiArgs { tokens: None },
        )
        .expect("ci_core");
        let val = serde_json::to_value(&out).expect("serialize");

        assert!(
            val.get("review").is_some(),
            "ci output carries review: {val}"
        );
        assert!(val.get("gate").is_some(), "ci output carries gate: {val}");
        assert!(val.get("token_count").is_none());
        assert!(val.get("token_budget").is_none());
    }

    /// Write an in-root fixture and return its path. The daemon path guard
    /// resolves the fixture relative to the served root, so it must live under
    /// `view.root` (the tempdir).
    fn write_fixture(root: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, body).expect("write fixture");
        path
    }

    /// `dispatch_eval` on an EMPTY fixture is byte-equal to
    /// `serde_json::to_value(eval_core(...))` with the daemon's substrate +
    /// freshness posture. Embedder-free by construction (an empty query set
    /// short-circuits before `ctx.embedder()`), so the parity holds without an
    /// ONNX session. Serial with the single-flight slot so a concurrent slot
    /// test can't busy-reject this dispatch.
    #[test]
    #[serial_test::serial(eval_single_flight)]
    fn parity_eval_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);
        let fixture = write_fixture(&view.root, "empty_queries.json", r#"{"queries":[]}"#);

        let args = crate::cli::commands::EvalArgs {
            query_file: fixture.clone(),
            limit: 20,
            category: None,
            reranker: crate::cli::args::RerankerMode::None,
        };
        let dispatched = super::dispatch_eval(&view, &args).expect("dispatch_eval");

        // Rebuild the core call the same way the handler does (same relativized
        // label, same resident substrate, same freshness snapshot).
        let set: cqs::eval::schema::QuerySet =
            serde_json::from_str(r#"{"queries":[]}"#).expect("parse empty set");
        let params = crate::cli::commands::EvalCoreParams {
            query_file_label: super::relativize_query_file(&fixture, &view.root),
            category_filter: None,
            limit: 20,
            reranker: crate::cli::args::RerankerMode::None,
            substrate: crate::cli::commands::IndexSubstrate::Resident,
            freshness: Some(crate::cli::commands::EvalFreshness::from_snapshot(
                &view.watch_snapshot(),
            )),
        };
        let core = crate::cli::commands::eval_core(&view, &set, &params).expect("eval_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        assert_eq!(
            dispatched, core_val,
            "dispatch_eval must equal eval_core on the empty fixture"
        );
    }

    /// The daemon dispatch stamps the RESIDENT substrate marker and a freshness
    /// snapshot on every eval output, and relativizes the fixture path (never the
    /// absolute host path). Serial with the single-flight slot.
    #[test]
    #[serial_test::serial(eval_single_flight)]
    fn eval_dispatch_stamps_substrate_and_freshness() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);
        let fixture = write_fixture(&view.root, "q.json", r#"{"queries":[]}"#);

        let args = crate::cli::commands::EvalArgs {
            query_file: fixture,
            limit: 20,
            category: None,
            reranker: crate::cli::args::RerankerMode::None,
        };
        let out = super::dispatch_eval(&view, &args).expect("dispatch_eval");

        assert_eq!(
            out["index_substrate"], "resident",
            "daemon eval must mark the resident substrate: {out}"
        );
        assert!(
            out.get("freshness").is_some(),
            "daemon eval must carry a freshness snapshot: {out}"
        );
        // The label is relativized to the served root, not the absolute host path.
        assert_eq!(
            out["query_file"], "q.json",
            "wire query_file must be relativized to the served root: {out}"
        );
    }

    /// The in-root path guard is not a content oracle. A traversal (out-of-root)
    /// fixture AND an in-root file that is NOT valid JSON produce the SAME opaque
    /// rejection — the daemon never reveals whether an in-root path exists or
    /// holds valid JSON, and never echoes the host path.
    #[test]
    fn eval_fixture_guard_traversal_and_non_json_are_indistinguishable() {
        let dir = TempDir::new().expect("root tempdir");
        let root = dir.path();

        // In-root, valid → Ok.
        let good = write_fixture(root, "good.json", r#"{"queries":[]}"#);
        assert!(
            super::read_fixture_within_root(&good, root).is_ok(),
            "an in-root valid fixture must read"
        );

        // In-root, NOT JSON → opaque error.
        let bad = write_fixture(root, "bad.txt", "definitely not json {{{");
        let non_json_err = super::read_fixture_within_root(&bad, root)
            .expect_err("in-root non-JSON must reject")
            .to_string();

        // Out-of-root (a valid JSON fixture in a sibling tree) → opaque error.
        let outside = TempDir::new().expect("outside tempdir");
        let escaped = write_fixture(outside.path(), "q.json", r#"{"queries":[]}"#);
        let traversal_err = super::read_fixture_within_root(&escaped, root)
            .expect_err("out-of-root must reject")
            .to_string();

        assert_eq!(
            non_json_err, traversal_err,
            "an in-root non-JSON file and a traversal must yield the SAME opaque error \
             (no content oracle): non_json={non_json_err:?} traversal={traversal_err:?}"
        );
        // And the message must not echo either host path.
        assert!(
            !non_json_err.contains(&bad.to_string_lossy().to_string())
                && !traversal_err.contains(&escaped.to_string_lossy().to_string()),
            "the opaque error must not echo the host path: {non_json_err:?}"
        );
    }
}
