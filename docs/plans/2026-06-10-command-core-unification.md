# Command-Core Unification (agentic navigability refactor)

Status: **Implementation phases complete (0–4 landed 2026-06-10).** Only the
post-campaign docs truth sweep remains. See "Campaign status (phases 0–4)" and
the "Post-campaign deferred ledger" near the bottom for the full state. This
doc is the resume point if the session dies.

## Motivation (evidence, 2026-06-09/10)

Every cqs command exists twice: CLI-direct (`cmd_*` in `src/cli/commands/`) and daemon-path (`dispatch_*` in `src/cli/batch/handlers/`). The dual surface is the dominant source of drift and doubled agent work:

- #1632 capped kind-fallback definitions on impact + daemon paths; the four sibling CLI commands stayed uncapped (the audit-queue item 1 DoS gap).
- Redirect-note strings exist as ~24 near-duplicate literals across the two surfaces (CQ-V1.40-4).
- `detect_kind_for_store` is dead in production while 8 call sites inline its body (CQ-V1.40-1/2).
- The 6 graph dispatchers diverge on `format: &OutputFormat` vs `json: bool` (API-V1.40-1).
- Output JSON is built inline (see `TODO(json-schema)` in `search/query.rs`), so shape changes require grepping string literals.

Primary consumers are agents; the refactor optimizes for: one edit site per behavior, one definition site per schema, `cqs impact <core_fn>` showing all callers.

## Target architecture

```
src/cli/commands/<area>/<cmd>.rs
  pub(crate) struct <Cmd>Args { ... }          // typed input, surface-agnostic
  pub(crate) struct <Cmd>Output { ... }        // serde Serialize — THE schema
  pub(crate) fn <cmd>_core(ctx, args) -> Result<<Cmd>Output>   // ALL logic

  pub(crate) fn cmd_<cmd>(...)      // thin: parse CLI -> Args, core, render (text|json via emit_json)
src/cli/batch/handlers/<area>.rs
  dispatch_<cmd>(...)               // thin: parse wire -> Args, core, wrap_value
```

**MCP-readiness (design driver, NOT an implementation target):** an eventual MCP server is a third thin adapter over the same cores — each core is one MCP tool. Shape every core so that adapter is mechanical when the day comes:
- `Args` structs derive `Deserialize` (an MCP tool call's params deserialize straight into them) with doc comments on every field (future tool/param descriptions; keep them schema-derivable — no exotic types in Args).
- `Output` structs derive `Serialize` (already the rule) — they are the tool results.
- Cores take `ctx` explicitly, never global/env state, and are safe for many invocations per process (the daemon adapter already enforces this).
- Command name → core enumeration stays mechanical (the `CqsCommands` derive registry is the seed).
- Do NOT add an MCP dependency, server, or schema-generation crate in this campaign. The deliverable is cores an MCP adapter could wrap without touching them.

Rules:
- A core never prints, never reads env posture, never knows its surface. Adapters own I/O.
- Output structs are the only JSON source (`serde_json::to_value(output)`); text rendering reads the same struct.
- Shared agent-facing strings (kind-fallback notes, redirect hints) live in `src/cli/commands/notes_text.rs` (or similar) as consts.
- Extract cores IN-PLACE in existing files first. Moving functions to new files shifts eval-fixture gold `(file, name)` origins — file moves happen, if ever, as a dedicated PR with a fixture refresh.
- JSON output may gain additive fields during unification; field removals/renames need a CHANGELOG note (no external users, but agents parse).

## Phases

### Phase 0 — cap parity + helper hoist (branch `fix/kind-fallback-cap-parity`)
- [x] `chunk_to_definition_value` + cap consts hoisted to `graph/mod.rs`; all 6 graph commands + daemon use it
- [x] Per-command cap tests
- Closes audit queue item 1. **Merged; the cores now route every fallback through `chunks_to_definitions`.**

### Phase 1 — graph commands (pilot, 6 commands × 2 surfaces)
Order: callers, callees (same file), deps, test_map, trace, impact (hardest: const/kind fallback variants).
- [x] `notes_text.rs` consts for all kind-fallback/redirect strings — `graph/notes_text.rs` holds per-(command,kind) `note` + `text_redirect` consts and templated `*_lead` formatters; CLI and daemon reference the same strings.
- [x] Adopt `detect_kind_for_store` inside cores — `graph/mod.rs::detect_fallback` is the single classification site (calls the now-generic `detect_kind_for_store`); the 8 inlined `lookup_by_name`+`classify_hits` incantations are gone.
- [x] Unify dispatcher signatures via cores — every `dispatch_*` is a thin adapter over a `*_core`; deleted the hand-rolled const-fallback duplicate in impact.
- [x] Exhaustive `match` on `Kind` in `fallback_kind` — every variant named, no `_ => {}`.
- [x] Typed `KindFallbackOutput` (shared) + per-command core-output enums (trace keeps its own `source`/`target`-shaped fallback).
- [x] Daemon `dispatch_*` for all 6 reduced to adapter calling the core; deleted `try_kind_fallback` / `KindNotes` / `build_kind_fallback_value`.
- [x] Parity tests: 6 tests (one per command) assert daemon adapter == direct core `serde_json::Value` for happy + const-fallback inputs. Hand-rolled-JSON shape tests migrated to the typed `KindFallbackOutput` / `ImpactCoreOutput` builders.
- Gate: targeted graph + handler + kind tests green; CHANGELOG entry added. Full-suite + `cqs eval` fixture-sensitivity check deferred to the orchestrator's post-collection run (cores produce byte-stable JSON; no retrieval path touched).

### Phase 2 — search/io commands

**Split into 2a (search — eval-sensitive) and 2b (io commands).**

#### Phase 2a — search (done)
- [x] Request-scoped config pattern established: the BFS-ceiling env caps
  (`CQS_TRACE_MAX_NODES`, `CQS_TEST_MAP_MAX_NODES`) are folded into
  `TraceArgs::max_nodes` / `TestMapArgs::max_nodes`, resolved once at each
  adapter boundary (CLI + daemon) via the now-`pub(crate)` `trace_max_nodes`
  / `test_map_max_nodes` helpers. The Phase-1 graph cores no longer read env;
  `bfs_shortest_path` / `build_test_map` take `max_nodes` as a parameter.
  Search's own `CQS_FORCE_BASE_INDEX` is folded into `QueryArgs::force_base_index`
  the same way.
- [x] Display module typed structs: `SearchResultOutput` (per-result; delegates
  the chunk base + posture-gated trust fields to the store serializer) +
  `SearchOutput` (envelope: `results`/`query`/`total` + optional
  `token_count`/`token_budget`/`source`). Both CLI search-JSON paths
  (`display_unified_results_json`, `display_tagged_results_json`) build them;
  the `TODO(json-schema)` in query.rs is unblocked. Wire shape byte-stable.
- [x] `query_core(ctx, &QueryArgs) -> Result<QueryOutput>` extracted in-place
  for the plain (non-`--ref`, non-`--include-refs`) project path + the non-ref
  name-only path, incl. audit-mode and NameOnly-FTS-first. `QueryArgs` derives
  `Deserialize` (MCP param surface) with doc-commented fields. `cmd_query` is
  the CLI adapter (render + `NoResults` exit). The `--ref` / `--include-refs` /
  ref-name-only sub-paths stay in the adapter, marked `TODO(phase-2b)`, because
  they emit the tagged-result shape rather than the core's `UnifiedResult`
  model. The daemon `dispatch_search` keeps its own `ChunkOutput` wire shape
  and distinct retrieval semantics; a CLI-vs-daemon parity test pins the shared
  name-only retrieval primitive. Full daemon→`query_core` unification is
  `TODO(phase-2b)` (needs a shared search-context trait spanning
  `CommandContext` + `BatchView`).
- Gate: build + targeted tests + clippy + fmt clean. Retrieval semantics
  provably untouched (eval's `runner.rs` is byte-identical; the shared
  primitives `search_hybrid`/`search_code_results`/`classify_query` are
  unchanged). Observed eval movement is confined to two rank-20-cliff queries
  whose position is decided by `codegen-units=1` FP coalescing, NOT logic —
  confirmed because a graph-only subset of this change (which cannot reach the
  eval path) reproduces the identical shift.

#### Phase 2b — search half (daemon + schema)
- [x] Cores + typed outputs for the io commands (read, context, gather, scout,
  onboard, brief, notes, diff, blame, drift, reconstruct). *(io half — separate
  PR.)* **Merged 2026-06-10 (#1696, branch `refactor/command-cores-io`):**
  - **Cored + daemon-routed** (typed `*Args` w/ Deserialize + `#[serde(default)]`
    clap-pinned, named `*_core`, both surfaces drive it): `diff` (`diff_core`),
    `drift` (`drift_core`), `scout` (`scout_core`), `onboard` (`onboard_core`),
    `gather` (`gather_core`), `context` (`context_core`), `blame` (`blame_core`).
  - **Schema convergence on the daemon side** (the 2b "reconciliation" note):
    `dispatch_diff`/`dispatch_drift` dropped their hand-rolled inline JSON and
    now serialize the CLI's typed `DiffOutput`/`DriftOutput` (drift's
    `chunk_type` now goes through `ChunkType::to_string`; diff added/removed
    `similarity` is correctly skipped, modified present). `dispatch_context`
    full-mode dropped its inline per-chunk JSON and now emits the CLI's
    `FullOutput` shape (gains `external_callers`/`external_callees`/
    `dependent_files`/`injection_flags`, per-chunk `line_start`/`line_end`
    replacing `lines:[s,e]`, drops `language`/`total`). `dispatch_gather` gains
    the CLI's reading-order re-sort + ≥50 over-fetch on `--tokens`. Compact /
    summary context already shared builders — unchanged.
  - **gather/scout/onboard env audit (audit-queue convergence):** cores read no
    env. scout/onboard clamp via the `SCOUT_LIMIT_MAX` const, gather's
    `gather_max_nodes()` stays an adapter-side text-display read; the only
    request-scoped knob folded into Args is gather's `json_overhead` (resolved
    at the adapter boundary, CLI format-dependent / daemon always-serialize).
  - **`GatherDirection` gained `#[derive(Deserialize)]`** (additive; Serialize
    output byte-identical — no `rename_all`) so `OnboardArgs`/`GatherArgs`
    deserialize. `src/gather.rs` is not an eval-reachable dir; retrieval
    untouched.
  - **Typed-output-only (no daemon path, display-oriented, trivial logic — plan
    option 3):** `brief`, `reconstruct`. Already carried typed `*Output`
    structs; left as-is. No `*Args`/`*_core` added (single positional `path`,
    no defaults to drift, so the clap-pin would be vacuous).
  - **Deferred (schema divergence needs a deliberate union-schema PR):**
    - `read`: full-read JSON diverges *in both directions* (daemon emits
      `notes_injected`+`trust_level` the CLI omits; CLI focused emits
      `injection_flags` the daemon omits) plus the daemon full-read does
      file-path vendored detection the CLI doesn't. Reconciling needs a
      union-schema decision (which fields win) — its own schema-change PR.
    - `notes`: the CLI list path reads `docs/notes.toml` while the daemon reads
      the store-cached notes, and the two emit *different* schemas (CLI `id`+
      `type`, daemon `sentiment_label`, no shared `id`). A single
      surface-agnostic core needs a unified data source + union schema first.
  - **Parity tests:** `parity_context_compact_daemon_equals_core` +
    `parity_context_full_daemon_equals_core` (byte-equal daemon vs core,
    embedder-free) pin the biggest schema change. The other cored commands are
    structurally parity-by-construction (the `dispatch_*` adapter literally
    calls the same `*_core`). Per-command clap-pin tests
    (`*_args_default_matches_clap_defaults`) + minimal-deserialize tests added
    for diff/drift/scout/onboard/gather/blame/context.
  - **Eval (gate b):** paired v3.v2, release build, against a force-rebuilt
    index — TEST .459/.743/.862, DEV .514/.817/.927; both at/above the brief's
    bands. No eval-reachable source touched (gate a); the force-reindex was
    incidental (refreshed line anchors), not required by the diff.
- [x] Daemon `dispatch_search` → `query_core` via a shared search-context trait.
  `SearchCtx` (in `commands/search/search_ctx.rs`) is the lean common surface
  (store / cqs_dir / root / embedder / reranker / splade_encode / splade_index /
  vector_index / base_vector_index / audit_state); `CommandContext` and
  `BatchView` both implement it. `dispatch_search` is now a thin adapter:
  wire `SearchArgs` → `daemon_query_args` → `query_core` → `build_*_value`.
  The daemon's documented semantic differences are preserved as **Args-level
  settings, not separate logic**: `always_route: true` (always classify, even
  with `--rrf`/`--rerank`), `fts_first: false` (no NameOnly-FTS-first
  short-circuit), and the limit clamp 100 at the adapter boundary. SPLADE
  `&SpladeIndex` (CLI cache borrow) vs `Arc<SpladeIndex>` (daemon snapshot) is
  unified behind a zero-copy `SpladeIndexRef` deref handle.
- [x] **Schema reconciliation (the 2b friction note, resolved).** The daemon no
  longer projects per-result JSON through `ChunkOutput`; it projects through the
  same `SearchResultOutput` / `to_json_with_origin` shape the CLI uses, built by
  the shared `display::build_unified_results_value` /
  `build_tagged_results_value`. `ChunkOutput` + `batch/types.rs` deleted.
  Consumer survey (evals/*.py, scripts/, .claude/, docs/, tests/) confirmed no
  reader of a `ChunkOutput`-only field; field-level delta CHANGELOG'd (added
  `type`/`has_parent`; `trust_level`/`injection_flags` now skip-when-default
  under Friendly posture; name-only now includes `content`).
- [~] `--ref` / `--include-refs` / ref-name-only search sub-paths: **typed-output
  convergence done, core-extraction not applicable.** Reference-index
  resolution needs config load + multi-store fan-out, which doesn't fit the
  single-store surface-agnostic `query_core` (a `SearchCtx` exposes one project
  store, not the reference LRU + config). Both the CLI (`cmd_query_ref_*`,
  `cmd_query_project`) and daemon (`dispatch_search_with_refs`) keep their
  reference retrieval in the adapter, but **both now serialize through the
  shared `SearchResultOutput` schema** (`display::build_tagged_results_value`),
  so reference results carry the same per-result `trust_level` /
  `reference_name` / `source` shape as project results. Inline daemon JSON for
  the ref paths is gone. Full core-extraction of the multi-store path is
  deferred (would need a multi-store search-context abstraction — out of scope
  for the search half).
- [x] Parity test: `parity_daemon_dispatch_equals_core_plus_serializer` asserts
  `dispatch_search` is byte-equal to `build_unified_results_value(query_core(
  view, daemon_args))` for happy + empty + the converged trust-labeled schema
  (name-only surface, embedder-free).
- Gate: same as Phase 1 + eval guard — amended 2026-06-10: byte-exact eval match is unachievable under codegen-units=1 (any code change re-coalesces FP ops and can flip rank-boundary ties; proven via graph-only-subset A/B in phase 2a). The enforceable invariant: (a) **eval-reachable source byte-identical in the diff** — defined as the retrieval/scoring dirs `src/search/`, `src/store/`, `src/cli/pipeline/`, `src/splade/`, `src/hnsw/`, `src/embedder/` plus the eval matcher/scorer, which lives at `src/cli/commands/eval/runner.rs` (the `(file, name)` rank match + R@K accumulation), NOT `src/eval/` — `src/eval/` holds only the gold/query schema types (`EvalQuery`/`GoldChunk` in `schema.rs`), so a `src/eval/` edit is only eval-reachable if it changes those types — AND (b) paired test+dev numbers within the clean-HEAD rebuild variance band.

### Phase 3 — infra/index commands (index, gc, stats, doctor, status, slot, cache, model, reference, hook, telemetry, audit-mode, init, ping, watch-adjacent)
- [x] Cores + typed outputs; daemon adapters. **Merged 2026-06-10 (#1697, branch
  `refactor/command-cores-infra`):**
  - **Group 1 — read-query, cored + (where a dispatcher exists) daemon-routed
    + parity:**
    - `stats` (daemon-routed): `StatsArgs` + `stats_core(store, root, cqs_dir)`.
      Folded the staleness-walk + `created_at`/`hnsw_vectors` population (was
      duplicated inline in `cmd_stats` and `dispatch_stats`) into the core.
      `dispatch_stats` now = `stats_core(..) + errors`. **Additive JSON:** the
      daemon stats path gains `created_at` + `hnsw_vectors` (previously
      CLI-only). Parity test `parity_stats_dispatch_equals_core_plus_errors`.
    - `stale` (daemon-routed): `StaleArgs` + `stale_core(store, root,
      file_set)`. Core takes the file_set by ref so the hot daemon path keeps
      its cached set (CLI uses `enumerate_for_stale`). `count_only` is an
      adapter-honored Arg (daemon projects a 3-field subset). Parity test
      `parity_stale_dispatch_equals_core`.
    - `cache stats`: typed `CacheStatsOutput` + `cache_stats_core` (bytes-only
      JSON contract preserved).
    - `model list`: typed `ModelListOutput` + `model_list_core`.
    - `slot list` / `slot active`: `SlotListOutput`/`SlotActiveOutput` +
      `slot_list_core`/`slot_active_core`.
    - `reference list`: `RefListOutput` + `ref_list_core`. **Behavior fix:** the
      text path now opens reference stores read-only (was `Store::open` RW) and
      normalizes the source path (was raw `to_string_lossy`), matching JSON.
    - `telemetry` dashboard: `TelemetryArgs` + `telemetry_core` (the streaming
      file-read aggregation; `TelemetryOutput` already existed).
  - **Group 2 — mutations, cored / typed-output:**
    - `gc`: `GcArgs` + `gc_core(store, root, cqs_dir)` (ReadWrite). All prune +
      HNSW-rebuild logic moved into the core; `render_gc_text` reads the typed
      `GcOutput`. No daemon path (`dispatch_gc` bails by design), so no parity
      test. HNSW dirty-flag abort-on-failure semantics preserved.
    - `cache prune`/`compact`/`clear`: `CachePruneOutput`/`CacheCompactOutput`/
      `CacheClearOutput` + `cache_prune_core`/`cache_compact_core`/
      `cache_clear_core`. Mutual-exclusion + atomic-VACUUM behavior preserved.
    - `slot create`/`promote`/`remove`: typed `SlotCreateOutput`/
      `SlotPromoteOutput`/`SlotRemoveOutput` replacing inline JSON; the
      lock-guarded lifecycle logic (incl. the active-daemon guard) is unchanged.
    - `telemetry reset`: typed `TelemetryResetOutput`; the atomic
      rename + `atomic_replace` reset stays adapter-owned.
    - `audit-mode on/off/query`: `AuditModeArgs` + `audit_mode_core(cqs_dir)`
      (query path pure-read, on/off persist via `save_audit_state`).
    - `reference add`/`remove`: typed `RefAddOutput`/`RefRemoveOutput`
      (orchestration stays in the adapter; only the success-envelope shape is
      typed).
  - **Group 3 — orchestration, typed-output-only (logic stays in the
    adapter; cores would force process/subprocess/multi-store side effects
    into a "core" — incorrect architecture per the plan):**
    - `index/build`: `IndexSummaryOutput` for the `--json` final summary
      (adapter-level, `src/cli/pipeline/` untouched).
    - `init`: `InitOutput` (downloads model + warms embedder — orchestration).
    - `convert`: `ConvertEntry`/`ConvertOutput` (document conversion).
    - `project add`/`list`/`remove`: `ProjectAddOutput`/`ProjectListOutput`/
      `ProjectRemoveOutput` (registry read/write).
  - **Deferred (with reasoning):**
    - `ping` / `status`: CLI `cmd_ping`/`cmd_status` are *socket clients*;
      daemon `dispatch_ping`/`dispatch_status` are *server snapshots*
      (`PingResponse` / `WatchSnapshot` are already the shared schema). They
      are not the same logic on two surfaces — no shared core to extract.
    - `gc` daemon path: `dispatch_gc` intentionally bails (writable store can't
      be shared with the serving snapshot) — no daemon adapter to wire.
    - `index/build` line-266 daemon-reconcile-queued early-return envelope:
      hand-rolls the full `{data,version,error}` wrapper for a special-case
      return; left as-is to bound risk on the index adapter (its own typed-shape
      cleanup, low value).
    - `doctor`: env/hardware probes, no inline JSON to type (already prints via
      its own report structs); no Args/core surface worth adding.
    - `hook`: install/status orchestration, no inline `serde_json::json!`;
      deferred — typed-output cleanup is low-value and it has no daemon path.
    - `umap`: python subprocess driver — typed output already present; subprocess
      side effects correctly adapter-owned; deferred.
    - `model swap`: long daemon-stop → reindex → daemon-restart orchestration;
      already carries typed `ModelSwapOutput`/`ModelShowOutput`. Core extraction
      would pull process control into a "core" — left as adapter.
    - `reference update` (`reindex`): full ref reindex pipeline (LLM/HyDE/docs
      enrichment passes); deferred — touches the enrichment pipeline, out of
      scope for an infra-group typed-output pass.
  - **Gate:** eval-reachable source (search/store/pipeline/splade/hnsw/embedder/
    eval) byte-identical — diff is confined to `src/cli/commands/{index,infra}/`,
    `src/cli/batch/handlers/{info,analysis}.rs`, and the two mod re-export
    files. `build.rs` changes are adapter-only (the summary struct);
    `src/cli/pipeline/` untouched. Eval run skipped per the amended gate (diff
    provably touches no eval-reachable dir). Build/clippy/fmt clean; targeted +
    parity tests green.
- Gate: full suite + `/cqs-verify` pass.

### Phase 4 — review/train/eval commands + sweep
**Landed 2026-06-10 (branch `refactor/command-cores-phase4`).** This closes the
implementation phases of the campaign.
- [x] **review group** — cores + typed Args/Output, daemon adapters + parity:
  - `dead` (`dead_core` + `DeadArgs`): both surfaces route through it; daemon
    `dispatch_dead` reduced to a 6-line adapter. `DeadArgs` derives
    `Deserialize` with a local `de_confidence` helper so the lib enum
    `DeadConfidence` stays `Serialize`-only (no eval-reachable source touched).
  - `health` (`health_core` + `HealthArgs`): adapter owns file enumeration
    (CLI builds the set via `enumerate_for_health`; daemon passes its cached
    `file_set`), mirroring the `stale_core` split. `dispatch_health` routes
    through the core.
  - `suggest` (`suggest_core` + `SuggestArgs`): read side cored; the `--apply`
    write stays CLI-only (writable store). **Daemon schema converged onto the
    CLI's typed `SuggestOutput` — `count` replaces the old `total`.**
  - `review` (`review_core` + `ReviewArgs` + `ReviewOutput`): flattens the lib
    `ReviewResult` + optional `token_count`/`token_budget`. CLI JSON + daemon
    drive the core; CLI text keeps its `json=false` budgeting + dashboard. The
    daemon empty-diff case now emits the full CLI shape (additive
    `relevant_notes`/`stale_warning`). Deleted dead `apply_token_budget_public`
    + `empty_review_json`.
  - `ci` (`ci_core` + `CiArgs` + `CiOutput`): flattens the lib `CiReport` +
    token telemetry. CLI JSON + daemon drive the core; the gate-failure
    `process::exit` stays adapter-owned (the core only reports `gate.passed`).
    Deleted dead `apply_ci_token_budget`.
  - `affected` (`affected_core` + `AffectedArgs` + `AffectedOutput`): CLI-only
    today (no dispatcher) but daemon-ready by construction. `AffectedOutput` is
    a thin `Serialize` newtype over the lib-owned `diff_impact_to_json`
    projection + the command-specific `overall_risk` field.
  - Parity tests (`analysis::parity_tests`): `parity_dead_dispatch_equals_core`,
    `parity_health_dispatch_equals_core`, `parity_suggest_dispatch_equals_core`
    (byte-equal daemon vs core; suggest test also guards `count`-not-`total`).
    `review`/`ci` acquire their diff via `run_git_diff` (needs a real repo
    diff), so their core-equivalence is pinned at the core level via
    `review_core_empty_diff_converged_shape` / `ci_core_empty_diff_shape`; the
    dispatchers are parity-by-construction (they call the core with
    `run_git_diff` output then `to_value`). Per-Args minimal-deserialize tests
    added for Dead/Review/Ci/Plan.
- [x] **train group** — orchestration-heavy → typed-output-only for most;
  cored the genuine pure queries:
  - `plan` (`plan_core` + `PlanArgs` + `PlanOutput`): pure read query (store +
    embedder), dual-surface; `dispatch_plan` routes through it.
  - `task` (`task_json_core`): the genuinely-shared JSON projection (waterfall
    budget when `--tokens`, else full serialize) both surfaces previously
    duplicated as an inline `if tokens { … } else { … }` branch. Resource
    *provisioning* stays per-surface (CLI `task()` builds resources internally;
    daemon `task_with_resources()` injects cached `graph`/`test_chunks` for
    perf — the same per-surface split the plan blessed for `stale_core`).
  - `train-data`, `train-pairs`, `export-model`: left adapter-owned — git
    history walk / JSONL file writer / `optimum.exporters.onnx` subprocess.
    Not pure queries; cores would force side effects into a "core".
- [x] **eval command adapter** — already conformant: `EvalReport` (runner.rs)
  is the typed Output serialized via `emit_json`; `EvalArgs` (`clap::Args`) is
  the typed input; the error path uses `emit_json_error`. `runner.rs`
  matcher/scoring is eval-reachable and was **not** touched (verified
  byte-identical in the diff).
- [x] **`_with_posture` wire-or-delete → DELETE.** `emit_json_with_posture` /
  `emit_json_error_with_posture` had zero callers (`#[allow(dead_code)]`).
  Decision: **delete**, per the plan's stated preference ("delete preferred if
  wiring adds indirection without behavior"). Justification: `Posture::current()`
  is `OnceLock`-cached and read once cheaply inside `emit_json`; the daemon-flip
  risk is already foreclosed by the cache pinning the posture for the process
  lifetime. Wiring an explicit `Posture` through every review/train/eval adapter
  would thread a value purely to call a variant that produces **byte-identical**
  output to what `emit_json` already emits — pure indirection, zero behavior
  change. The live posture-threaded variants that DO have callers
  (`Envelope::ok_with_posture`/`err_with_posture`, `wrap_value_with_posture`,
  `emit_json_error_with_data_and_posture`, used by the batch JSONL surface)
  stay. The `CQS_ULTRASECURITY` env knob is left intact (its deletion is #1690's
  own PR). Closes CQ-V1.40-5/6.
- [x] Final greps: no Phase-4-touched `dispatch_*` carries duplicated business
  logic (dead/health/review/ci/plan/task are all thin adapters now; the
  remaining >20-line dispatchers are doc-comment-heavy adapters or Phase-2b
  deferrals like `dispatch_notes`/`dispatch_gather`). Inline `serde_json::json!`
  in review/train command code is test fixtures + the documented lib-owned
  `affected` projection boundary only.
- Gate: build + clippy + fmt clean; targeted review/train/eval + 5 new parity
  tests green. **Eval-reachable source byte-identical** — the diff is confined
  to `src/cli/commands/{review,train}/`, `src/cli/batch/handlers/{analysis,misc}.rs`,
  the two mod re-export files, and `src/cli/json_envelope.rs`. Paired v3 eval
  run **provably skipped**: no eval-reachable dir
  (search/store/pipeline/splade/hnsw/embedder/eval) appears in the diff, so
  retrieval cannot move (amended Phase-2/3 gate).

### Campaign status (phases 0–4)
- **Phase 0** — ✅ merged (#1688, `fix/kind-fallback-cap-parity`). Cap parity + helper hoist.
- **Phase 1** — ✅ merged (#1689). 6 graph commands × 2 surfaces cored.
- **Phase 2a** — ✅ merged (#1694). Search core (`query_core`) + request-scoped config pattern.
- **Phase 2b** — ✅ merged (#1695 search-half + #1696 io-half). io commands + daemon `dispatch_search` → `query_core` + schema reconciliation.
- **Phase 3** — ✅ merged (#1697, `refactor/command-cores-infra`). infra/index commands.
- **Phase 4** — ✅ merged (#1698, `refactor/command-cores-phase4`, this entry). review/train/eval + `_with_posture` delete. **Implementation phases complete.**
- **Post-campaign docs truth sweep** — 🔄 in progress (this PR; see below).

## Post-campaign deferred ledger

Consolidated list of every deferral made across the campaign, with its reason
and tracking. None block the campaign's structural goal (one core per behavior,
one schema per command); each is a deliberate scope boundary.

| Item | Phase | Reason deferred | Tracking |
|------|-------|-----------------|----------|
| `read` full/focused union-schema | 2b | Daemon emits `notes_injected`+`trust_level` the CLI omits; CLI focused emits `injection_flags` the daemon omits; daemon does vendored-path detection the CLI doesn't. Needs a union-schema decision (which fields win) — its own schema-change PR. | dedicated schema PR |
| `notes` list union-schema | 2b | CLI list reads `docs/notes.toml` (emits `id`+`type`); daemon reads store-cached notes (emits `sentiment_label`, no shared `id`). A surface-agnostic core needs a unified data source + union schema first. | dedicated schema PR |
| `brief`, `reconstruct` cores | 2b | Single positional `path`, trivial display logic, no daemon path, no defaults to drift → a clap-pin would be vacuous. Typed `*Output` already present. | none (typed-output-only is correct) |
| `--ref`/`--include-refs`/ref-name-only full core-extraction | 2b | Reference-index resolution needs config load + multi-store fan-out; doesn't fit the single-store `SearchCtx`. Both surfaces already serialize through the shared `SearchResultOutput` schema. | multi-store search-context abstraction (out of scope) |
| `ping` / `status` shared core | 3 | CLI `cmd_ping`/`cmd_status` are *socket clients*; daemon `dispatch_ping`/`dispatch_status` are *server snapshots* (`PingResponse`/`WatchSnapshot` already shared schema). Not the same logic on two surfaces — no core to extract. | none (correct as-is) |
| `gc` daemon path | 3 | `dispatch_gc` intentionally bails — a writable store can't be shared with the serving snapshot (typestate-enforced). No daemon adapter to wire. | none (correct as-is) |
| `doctor`, `hook`, `umap`, `model swap`, `reference update` cores | 3 | Env/hardware probes, install orchestration, python subprocess, daemon-stop→reindex→restart, full enrichment pipeline respectively. Process/subprocess/multi-store side effects belong in adapters, not cores. Typed outputs already present where JSON is emitted. | none (typed-output-only / adapter-owned is correct) |
| `index/build` line-266 reconcile early-return envelope | 3 | Hand-rolls the full `{data,version,error}` wrapper for a special-case return; left as-is to bound risk on the index adapter. | low-value typed-shape cleanup |
| `train-data`/`train-pairs`/`export-model` cores | 4 | Git-history walk / JSONL file writer / `optimum` subprocess — orchestration, not pure queries. | none (adapter-owned is correct) |
| `CQS_ULTRASECURITY` env knob removal | 4 | Confirmed for deletion but owned by #1690's own PR; not removed here to keep this diff scoped. | #1690 |
| Docs truth sweep | post | `/docs-review` across README/CONTRIBUTING/SECURITY/PRIVACY/lib.rs/Cargo.toml + repo description for campaign-stale claims and legacy references. | post-campaign docs PR |

- Gate: full suite, eval paired check, `cqs health` no new dead code, CHANGELOG.

### Post-campaign — docs truth sweep
Reviewer-supplied hunts (phase-4 review): (1) audit-triage CQ-V1.40-5/6 marked resolved — verify nothing else in that doc claims _with_posture is pending; (2) docs/audit-findings-v1.15.1.md:379 describes daemon suggest `total` — historical doc, flag as frozen-not-current; (3) pin the exact eval-runner path (src/cli/commands/eval/runner.rs vs src/eval/) wherever "eval-reachable" is defined; (4) confirm the plan's phase-status lines reflect merged state, not working-tree state.
- [~] Run `/docs-review` across README, CONTRIBUTING, SECURITY, PRIVACY, lib.rs docs, Cargo.toml metadata, and the GitHub repo description — hunting "tiny lies" (claims the campaign made stale: command behavior, JSON shapes, env knobs incl. the #1690 CQS_ULTRASECURITY deletion) and legacy references (removed helpers, old dispatch names, pre-core architecture descriptions in CONTRIBUTING's Architecture Overview). *(In progress 2026-06-10 — branch `docs/post-campaign-truth-sweep`.)*
- [ ] Fix drift in one docs PR; docs-lying-is-P1 severity applies

## Invariants for every phase

1. One PR per phase (or per command-group within a phase if a phase grows); main protected, CI green before merge.
2. No behavior change beyond documented additive JSON fields. Text output byte-stable where tests pin it.
3. `cargo fmt`, clippy clean, provenance lint clean (describe behavior in comments, not audit IDs).
4. After merge: rebuild + install binary, restart cqs-watch, `cqs index` (per MEMORY.md), update this doc's checkboxes + tears.
5. Implementer agents get one command-group each, disjoint files; code-reviewer before each PR.

## Audit-queue convergence

Phase 1 closes queue items 1 (with Phase 0), parts of 4, and test items in 7. Items NOT covered here (do separately): Kind enum split (API-V1.40-2 — keep out of Phase 1 to bound risk; revisit after pilot), DS-V1.40-1 data_version, DS-V1.40-8/10 shared snapshot, observability bundle (OB-*), perf bundle.
