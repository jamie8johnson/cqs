# Observability Audit — post-v1.38.0

Scope: code merged after v1.38.0 release through commit `fe7126c4` (PR #1506). Prior obs cleanup landed in #1208/#1362 (v1.33.0); this pass focuses on additions in PRs #1466..#1506 plus a few latent gaps the new code re-exposed.

---

#### OB-V1.38-1: `cmd_ref_update` lacks an entry span — entire ref reindex path invisible in tracing
- **Difficulty:** easy
- **Location:** `src/cli/commands/infra/reference.rs:599`
- **Description:** PR #1506 grew `cmd_ref_update` into a full LLM/HyDE/doc-comment/enrichment pipeline (~150 added lines, parity with `cmd_index`). The parent `cmd_ref` at line 130 has `tracing::info_span!("cmd_ref")`, but the per-subcommand handler `cmd_ref_update(cli, name, json, opts)` carries no span of its own. `cmd_ref_add`, `cmd_ref_list`, `cmd_ref_remove` are also span-less. Operators correlating "why did the ref reindex pipeline take 18 minutes?" against journal logs see only the `cmd_ref` parent — they can't tell which subcommand ran, which ref name was targeted, or which post-pipeline pass (LLM / HyDE / docs / enrichment) is the slow leg without RUST_LOG=debug per-pass tracing.
- **Suggested fix:** Add `let _span = tracing::info_span!("cmd_ref_update", ref_name = name, llm_summaries = opts.llm_summaries, improve_docs = opts.improve_docs, hyde_queries = opts.hyde_queries).entered();` at line 612 (after the flag-invariant bail). Mirror in `cmd_ref_add` / `cmd_ref_remove` / `cmd_ref_list` for consistent surface coverage.

#### OB-V1.38-2: `check_index_model_drift` bails silently — no `tracing::warn!` accompanies the fatal error
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:1496-1525` (call site at `:357`)
- **Description:** PR #1505's drift-detection footgun fix (`cqs index --model X` against a Y-built store) is the new actionable failure mode: `anyhow::bail!` returns a long help string. But it never emits a `tracing::warn!` or `tracing::error!`, so when the daemon-spawned `cqs index` is invoked from a hook or via the daemon and the operator only has journald logs, the structured event "index drift detected, refusing to clobber" never lands. The bail message goes to stderr only — daemon-mode loses it.
- **Suggested fix:** Before `anyhow::bail!` at line 1518, add `tracing::warn!(stored_model = %stored_name, requested_model = %requested_name, index_path = %index_path.display(), "Index model drift detected — refusing to clobber, exiting");`. Operators harvesting journald can then alert on this condition without parsing stderr.

#### OB-V1.38-3: synonym/classifier overlay install is silent — operators can't see whether their TOML config was applied
- **Difficulty:** easy
- **Location:** `src/search/synonyms.rs:81` (install_synonym_overlay), `src/search/router.rs:519` (install_classifier_vocab_overlay), `src/search/router.rs:689` (install_slot_splade_alpha_overrides), all called from `src/cli/dispatch.rs:143/165/185`
- **Description:** PRs #1472, #1482, #1483 added three TOML overlay surfaces (per-slot SPLADE α, synonym table, classifier vocab). Each `install_*` runs once at dispatch entry. None of them log at info level on success — `load_synonym_overlay` does emit `tracing::debug!(entries, "Loaded synonym overlay")` but at debug, and the classifier vocab loader has no completion event at all. An operator who edits `~/.config/cqs/synonyms.toml` cannot verify from journald whether the file was loaded, parsed, or applied without enabling RUST_LOG=debug. Per CLAUDE.md memory ("user-actionable info hidden behind RUST_LOG=debug" pattern), this should be info.
- **Suggested fix:** Promote `tracing::debug!` at `synonyms.rs:197` to `tracing::info!`. Add equivalent info-level events at `install_synonym_overlay` (count of merged entries), `install_classifier_vocab_overlay` (count of negation/multistep additions), and `install_slot_splade_alpha_overrides` (count of category overrides) — fire only when the input is non-empty, so the default no-overlay case stays silent.

#### OB-V1.38-4: `wait_for_batch` polling loop has no entry span and no closing elapsed_ms — Anthropic Batches API latency invisible
- **Difficulty:** easy
- **Location:** `src/llm/batch.rs:147`
- **Description:** `LlmClient::wait_for_batch` polls the Batches API every `BATCH_POLL_INTERVAL` until the batch reports `ended` / `canceling` / `canceled` / `expired`. A typical Claude batch takes 10-30 minutes; the only structured event for this entire wait is `tracing::info!(batch_id, "Batch complete")` on the success arm at line 183. There is no entry span, no elapsed_ms on the completion event, no warn/error event for the canceling/canceled/expired arms (they return `Err(LlmError::BatchFailed(...))` with no log line). Operators investigating "why did the LLM summary pass take 47 minutes?" cannot distinguish API slowness from local processing without timing the wait themselves. The `submit_or_resume` parent span (line 487) covers this transitively, but the leaf span where the actual wall-clock waiting happens emits nothing structured.
- **Suggested fix:** (1) Add `let _span = tracing::info_span!("wait_for_batch", batch_id).entered();` and `let started = std::time::Instant::now();` at the top. (2) On the `ended` arm, change the info to `tracing::info!(batch_id, elapsed_ms = started.elapsed().as_millis() as u64, "Batch complete")`. (3) On the canceling/canceled/expired arm, fire `tracing::warn!(batch_id, status = %batch.processing_status, elapsed_ms, "Batch ended in non-success state")` before returning `Err`.

#### OB-V1.38-5: CAGRA "ineligible — falling through" is at debug — operators can't see the GPU-fallback decision in journald
- **Difficulty:** easy
- **Location:** `src/cagra.rs:1530-1536`
- **Description:** When `CagraIndex::resolve_for_search` decides not to use CAGRA (chunk count below threshold OR GPU unavailable), it logs at `tracing::debug!`. The CAGRA-rebuilt and CAGRA-loaded paths (`tracing::info!(backend = "cagra", source = ..., "Vector index backend selected")` at lines 1552 and 1578) are info-level. The HNSW-fallback path is debug — so a query that should have been on GPU but silently fell back to HNSW (e.g. CUDA was momentarily unavailable, or chunk count fell below threshold after a `cqs gc`) leaves no info-level trace. Per the audit prompt: "Fallback paths (e.g., GPU unavailable → CPU) — should fire `tracing::info!` once so operators know they're on the slow path."
- **Suggested fix:** Promote the fall-through log to `tracing::info!` and tag with the same `backend = "hnsw", source = "cagra-ineligible"` shape used by the success arms so log filters on `backend` work uniformly: `tracing::info!(backend = "hnsw", source = "cagra-ineligible", chunk_count, cagra_threshold, dim, gpu_available, "Vector index backend selected")`. One line per Store init — not per-query, no spam.

#### OB-V1.38-6: skip-first-pass-embed sentinel batch logs at debug — operator running `cqs index --llm-summaries` can't see why search returns zero hits mid-run
- **Difficulty:** easy
- **Location:** `src/cli/pipeline/embedding.rs:297-301`
- **Description:** PR #1497 introduced the skip-first-pass optimisation: when `--llm-summaries` is set, the first embedding pass emits zero-vec sentinels (`needs_embedding=1`) and defers real embedding to the post-summary `enrichment_pass`. Mid-run, the index is in a state where ~50% of chunks have zero vectors and are explicitly excluded from search. The only structured event for this hot loop is `tracing::debug!(cache_hits, stamped_unembedded, "skip-first-pass: emitted zero-vec batch")` — fires per batch, but at debug. An operator searching while a `cqs index --llm-summaries` is half-way through and getting unexpectedly poor recall has no info-level signal that the index is in a transient zero-vec state.
- **Suggested fix:** Emit a single info-level event at the start of the index run (in `cmd_index` where `skip_first_pass_embed` is decided) saying "skip-first-pass enabled — chunks awaiting summary will land vectors during enrichment pass." Keep the per-batch debug log unchanged. This gives operators one stable journal line they can correlate against "search went weird at 14:32" without re-instrumenting at debug.

#### OB-V1.38-7: SPLADE save .bak rollback restore-failure is `tracing::error!` but the error path bails without surfacing to operator console
- **Difficulty:** medium
- **Location:** `src/splade/index.rs:524-545`
- **Description:** PR #1491 added `.bak` rollback: on atomic_replace failure, restore `.bak` → live name. If that restore *also* fails the error path emits `tracing::error!` and returns `SpladeIndexPersistError::Io`. The error message is great ("manual recovery — rename {bak} to {path}") but it's only emitted via `tracing::error!`. In daemon mode this lands in journald, but in interactive `cqs index` mode the rolled-up `Result<()>` propagates up as a generic `anyhow::Error` and the operator sees a one-line message — they have no way to know they need to manually rename `.bak` back unless they happen to scrollback through stderr. The same shape exists for the stale-`.bak` guard at line 470: returns Err with the recovery hint embedded but no structured tracing event records that the index was refused.
- **Suggested fix:** At line 470 (stale-`.bak` guard), emit `tracing::warn!(bak_path = %bak_path.display(), live_path = %path.display(), "SPLADE save refused — stale .bak from prior failed save, manual recovery required")` before the early return. Already-present `tracing::error!` at 524 is fine as-is — but consider re-emitting it to stderr (via `eprintln!`) when running outside the daemon context so the operator definitely sees it.

#### OB-V1.38-8: `tracing::warn!("--rerank is not supported with multi-index search...")` lacks `multi_index_count` field
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/query.rs:527`
- **Description:** This warn fires when a user combines `--reranker llm` (or `bge`) with multi-index search. Single-line warn with no fields — operators can't see how many indexes were loaded, which one was active, or what the rerank flag value was. With agents running thousands of searches, when this warn fires a few times the journal entry doesn't carry enough context to reproduce the misuse.
- **Suggested fix:** `tracing::warn!(reranker = ?reranker_kind, index_count, "--rerank is not supported with multi-index search, skipping re-ranking");` so the journal line names the reranker and the multi-index breadth. Helps operators decide whether to pin to a single index or drop the rerank flag.

#### OB-V1.38-9: dispatch entry's three overlay loads have no top-level span — config-load layer is opaque in tracing
- **Difficulty:** easy
- **Location:** `src/cli/dispatch.rs:143-186`
- **Description:** Three discrete overlay-install blocks run unconditionally at dispatch entry: per-slot SPLADE α, synonym table, classifier vocab. Each block calls a `load_*_overlay` function which may hit two filesystem paths (user-global + project-local), parse TOML, validate, merge. None of these is wrapped in a `tracing::debug_span!` — so a slow disk or a wedged TOML parse appears in tracing output as unattributed time between `cqs` root span entry and the first command-level span. With no span, structured-trace consumers can't aggregate "time spent in overlay loading" across runs.
- **Suggested fix:** Wrap the three blocks (and the slot-α block before them) in a single `let _ = tracing::debug_span!("config_overlays_load").entered();` covering lines 138-186. Cheap, makes the dispatch-entry latency attributable.

#### OB-V1.38-10: index pipeline's UMAP step warn drops `chunk_count` and `dim` — debugging UMAP failures is guesswork
- **Difficulty:** easy
- **Location:** `src/cli/commands/index/build.rs:983`
- **Description:** `tracing::warn!(error = %e, "UMAP projection failed — cluster view will be unavailable")` — error field is good, but UMAP failure modes correlate strongly with chunk count (too few rows = degenerate manifold) and dimension (fp16/fp32 type mismatch on GPU). Without those fields the journal entry says "UMAP failed" with no actionable diagnostic.
- **Suggested fix:** `tracing::warn!(error = %e, chunk_count, dim, "UMAP projection failed — cluster view will be unavailable")`. `chunk_count` is in scope at the call site; `dim` is on `store.dim()` and is cheap to read.

---

## Summary

10 findings, all easy except OB-V1.38-7 (medium). Themes:

1. **New code in #1505/#1506 missed entry spans** (OB-V1.38-1, OB-V1.38-2). The post-v1.38 LLM/HyDE/doc-comment parity arm in `cmd_ref_update` is span-less, and the new `cqs index --model` drift detection bails without a structured warn.
2. **Slow-path latency is unattributable** (OB-V1.38-4, OB-V1.38-7). `wait_for_batch` polls Anthropic for 10-30 minutes per LLM pass with zero spans and no elapsed_ms; SPLADE save rollback failures emit error events but no operator-facing surface.
3. **User-actionable info trapped behind RUST_LOG=debug** (OB-V1.38-3, OB-V1.38-5, OB-V1.38-6). TOML overlay loads, GPU/CPU fallback decisions, and skip-first-pass mode are all at debug — operators can't tell from journald whether their config landed, whether queries ran on GPU, or why mid-index search recall is poor.
4. **Warns missing structured fields** (OB-V1.38-8, OB-V1.38-10). Two warns drop the most diagnostic field of their context (reranker kind, chunk_count/dim) — easy adds, but lost in journal triage today.
5. **Dispatch-entry overhead is invisible** (OB-V1.38-9). Three TOML overlay loads run unconditionally; no span aggregates them, so structured-trace consumers can't measure config-load cost across runs.

The pattern across PRs #1505/#1506 is consistent with prior audit waves: feature additions ship with `tracing::warn!(error = %e, ...)` on error paths but skip the entry span. The CLAUDE.md memory rule ("Every new public function must have a `tracing::info_span!` at entry") needs to extend to per-subcommand handlers (`cmd_ref_update`, the dispatch-shim leaves), not just the dispatcher.
