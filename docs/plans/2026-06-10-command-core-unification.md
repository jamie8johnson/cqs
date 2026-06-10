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
- [ ] Request-scoped config pattern: fold the BFS-ceiling env caps (`CQS_TRACE_MAX_NODES`, `CQS_TEST_MAP_MAX_NODES`) into Args/ctx so the core-purity invariant (no env reads) becomes literally true for the Phase 1 cores too (search/query, read, context, gather, scout, onboard, brief, notes, diff, blame, drift, reconstruct)
- [ ] Cores + typed outputs (unblocks the `TODO(json-schema)` in query.rs — requires display module typed structs; do display first)
- [ ] `display_unified_results_json` replaced by `SearchOutput` struct
- Gate: same as Phase 1 + eval guard (search path touched — paired test+dev eval, both within noise).

### Phase 3 — infra/index commands (index, gc, stats, doctor, status, slot, cache, model, reference, hook, telemetry, audit-mode, init, ping, watch-adjacent)
- [ ] Cores + typed outputs; daemon adapters
- Gate: full suite + `/cqs-verify` pass.

### Phase 4 — review/train/eval commands + sweep
- [ ] Remaining commands; delete any now-unused `json: bool` plumbing and posture shims that became adapter-only (revisit CQ-V1.40-5/6 `_with_posture` here: adapters become the only emit sites — wire or delete)
- [ ] Final: grep for `dispatch_.*{` bodies >20 lines (should be none); grep inline `serde_json::json!({` in command code (should be near-zero outside adapters)
- Gate: full suite, eval paired check, `cqs health` no new dead code, CHANGELOG.

## Invariants for every phase

1. One PR per phase (or per command-group within a phase if a phase grows); main protected, CI green before merge.
2. No behavior change beyond documented additive JSON fields. Text output byte-stable where tests pin it.
3. `cargo fmt`, clippy clean, provenance lint clean (describe behavior in comments, not audit IDs).
4. After merge: rebuild + install binary, restart cqs-watch, `cqs index` (per MEMORY.md), update this doc's checkboxes + tears.
5. Implementer agents get one command-group each, disjoint files; code-reviewer before each PR.

## Audit-queue convergence

Phase 1 closes queue items 1 (with Phase 0), parts of 4, and test items in 7. Items NOT covered here (do separately): Kind enum split (API-V1.40-2 — keep out of Phase 1 to bound risk; revisit after pilot), DS-V1.40-1 data_version, DS-V1.40-8/10 shared snapshot, observability bundle (OB-*), perf bundle.
