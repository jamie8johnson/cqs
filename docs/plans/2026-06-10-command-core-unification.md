# Command-Core Unification (agentic navigability refactor)

Status: **Phase 0 in flight** — update the per-phase checkboxes as work lands; this doc is the resume point if the session dies.

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

#### Phase 2b — io commands (not started)
- [ ] Cores + typed outputs for the io commands (read, context, gather, scout,
  onboard, brief, notes, diff, blame, drift, reconstruct).
- [ ] `--ref` / `--include-refs` / ref-name-only search sub-paths adopt the
  shared output model (lift them out of the `cmd_query` adapter).
- [ ] Daemon `dispatch_search` → `query_core` via a shared search-context trait.
- Gate: same as Phase 1 + eval guard — amended 2026-06-10: byte-exact eval match is unachievable under codegen-units=1 (any code change re-coalesces FP ops and can flip rank-boundary ties; proven via graph-only-subset A/B in phase 2a). The enforceable invariant: (a) eval-reachable source (search/store/pipeline/splade/hnsw/embedder/eval) byte-identical in the diff, AND (b) paired test+dev numbers within the clean-HEAD rebuild variance band.

**2b friction note (from 2a review):** the daemon search path projects per-result JSON through `ChunkOutput` while the CLI now projects through `SearchResultOutput`/`to_json_with_origin` — two different schemas, not just two call sites. 2b's shared-context-trait work must reconcile the schema, not only the dispatch. The graph commands had already converged on one schema before coring; search has not.

### Phase 3 — infra/index commands (index, gc, stats, doctor, status, slot, cache, model, reference, hook, telemetry, audit-mode, init, ping, watch-adjacent)
- [ ] Cores + typed outputs; daemon adapters
- Gate: full suite + `/cqs-verify` pass.

### Phase 4 — review/train/eval commands + sweep
- [ ] Remaining commands; delete any now-unused `json: bool` plumbing and posture shims that became adapter-only (revisit CQ-V1.40-5/6 `_with_posture` here: adapters become the only emit sites — wire or delete)
- [ ] Final: grep for `dispatch_.*{` bodies >20 lines (should be none); grep inline `serde_json::json!({` in command code (should be near-zero outside adapters)
- Gate: full suite, eval paired check, `cqs health` no new dead code, CHANGELOG.

### Post-campaign — docs truth sweep
- [ ] Run `/docs-review` across README, CONTRIBUTING, SECURITY, PRIVACY, lib.rs docs, Cargo.toml metadata, and the GitHub repo description — hunting "tiny lies" (claims the campaign made stale: command behavior, JSON shapes, env knobs incl. the #1690 CQS_ULTRASECURITY deletion) and legacy references (removed helpers, old dispatch names, pre-core architecture descriptions in CONTRIBUTING's Architecture Overview)
- [ ] Fix drift in one docs PR; docs-lying-is-P1 severity applies

## Invariants for every phase

1. One PR per phase (or per command-group within a phase if a phase grows); main protected, CI green before merge.
2. No behavior change beyond documented additive JSON fields. Text output byte-stable where tests pin it.
3. `cargo fmt`, clippy clean, provenance lint clean (describe behavior in comments, not audit IDs).
4. After merge: rebuild + install binary, restart cqs-watch, `cqs index` (per MEMORY.md), update this doc's checkboxes + tears.
5. Implementer agents get one command-group each, disjoint files; code-reviewer before each PR.

## Audit-queue convergence

Phase 1 closes queue items 1 (with Phase 0), parts of 4, and test items in 7. Items NOT covered here (do separately): Kind enum split (API-V1.40-2 — keep out of Phase 1 to bound risk; revisit after pilot), DS-V1.40-1 data_version, DS-V1.40-8/10 shared snapshot, observability bundle (OB-*), perf bundle.
