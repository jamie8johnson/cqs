# Polymorphic Command Routing — Design

**Status:** Phase 1 complete (60/60 dispatch points shipped 2026-05-08; PRs #1610/#1612/#1616/#1617/#1618/#1620). Phase 2 contingent — telemetry-gated.
**Date:** 2026-05-08 (design); shipped same day
**Location:** `docs/polymorphic-routing.md`
**Tracking:** ROADMAP.md "Agent Adoption — Telemetry > Friction backlog"
**Cross-reference:** `docs/json-snr-restoration.md` (sibling agent-adoption work). The two designs are complementary: SNR restoration makes responses cheap; this design makes routing forgiving. Together they reduce the friction that pushes agents toward grep.

---

## Plain-language opener

cqs's command surface is fragmented along symbol-kind lines. Some commands work only for functions (`impact`, `callers`, `callees`, `test-map`, `trace`); some only for types (`deps`); some for any chunk-shaped lookup (`explain`, `read`, `context`); some for freeform queries (`search`, `scout`, `gather`). When an agent has a name and wants to know about it, they have to know in advance what kind of name it is, and pick the matching command.

The failure mode this produces: **misrouted-to-empty.** `cqs impact HANDLING_ADVICE` (a `const`) returns an empty Vec because the call-graph path is function-only. The agent triages the failure → reaches for a different cqs subcommand → eventually falls back to grep, which always returns *something* even if the something is text-noise.

This design eliminates misrouted-to-empty. **Same answer through different doors is fine; misrouted-to-empty is the bug** (user, 2026-05-08). The fix has two layers:

- **Phase 1:** existing commands gracefully broaden when called with a name that isn't the kind they were built for. `cqs impact <const>` returns a useful response (location + content references + "this is a const") instead of empty.
- **Phase 2 (contingent):** a single polymorphic entry point — `cqs about <name>` or extending `cqs <query>` to do name resolution first — for the agent that doesn't know which command to reach for.

## Honest framing

This is **agent ergonomics**, not a load-bearing fix. The polymorphic-routing item was filed because agents reach for grep when cqs's surface mismatches their mental model. PR #1593 addressed the alarming-shaped friction (advisory text); the SNR restoration design (`docs/json-snr-restoration.md`) addresses response-size friction. Polymorphic routing addresses **routing friction**: the cost of knowing which command to call.

The expected impact is qualitative — agents reach for cqs more readily because the failure mode shifts from "empty result, try something else" to "useful result with kind metadata." The telemetry trend that motivates this is the same one as the SNR work (search dropped from 79% to 6% of code-intel calls); we don't expect a single number to validate this fix, just a steady drift back toward higher cqs reach over the next few weeks.

## Goal

When an agent has a name (function, type, const, module, file) and asks any cqs subcommand for information about it, the response is **useful, structured, and labeled with the kind that was detected**. Empty responses are reserved for genuine "no match in this index" cases; they are never the result of a name being the wrong kind for the chosen command.

## Non-goals

- Replacing existing subcommands with a single polymorphic dispatcher. Subcommands stay; they get smarter about kind mismatch.
- Changing how the call graph, type graph, or chunk index are built. Only changing what the dispatch layer does with them.
- Cross-project routing. Stays scoped to the active project / slot.
- Optimizing kind-detection latency below SQLite-index lookup speed (~1-2 ms). Adequate for interactive use; daemon batch path may need a per-batch cache if it bottlenecks (Phase 1 will measure; Phase 2 will fix only if needed).
- Disambiguating overloaded names automatically (`fn len` exists on many types). Overloaded matches are returned as a kind-detection result with all candidates; the caller decides.

## Kind-detection model

Given a name string, classify it by querying the index:

```
Kind:
  Function       — exact name match in chunks table where chunk_type ∈ {Function, Method}
  Type           — exact name match where chunk_type ∈ {Struct, Enum, TypeAlias, Trait, Class, Interface}
  Const          — exact name match where chunk_type ∈ {Const, Static, Variable}
  Module / File  — exact name match where chunk_type ∈ {Module, File} OR name resolves to a file path
  Ambiguous      — exact name matches across multiple kinds (e.g., `len` is a method AND a const)
  Multiple       — N matches of the same kind (e.g., a function defined in multiple langs/files)
  FreeformQuery  — no exact name match; treat as a search query
  NotFound       — no match in any kind, no useful search results either
```

Detection is one SQL query against the chunks table:

```sql
SELECT id, chunk_type, file, line_start, language
  FROM chunks
 WHERE name = ?1
 ORDER BY chunk_type, file, line_start
```

Then classify by counting hits and grouping by chunk_type. The classifier itself is ~30 lines of Rust.

## Phase 1: graceful broadening of existing commands

Each existing function-or-type-specialized subcommand grows a kind-mismatch fallback. Default to detection + a useful response shape; never return empty for a kind mismatch.

### Per-command behavior matrix

| Command | Function | Type | Const | Module / File | Ambiguous |
|---|---|---|---|---|---|
| `cqs impact <name>` | call-graph impact (current) | type usage references (deps-style) | content references in chunks table | files importing this module | aggregate: all kinds, with `kind` field per result |
| `cqs callers <name>` | direct callers (current) | "types don't have callers; X is used by [...]" with content refs | content references | imports of this module | aggregate |
| `cqs callees <name>` | direct callees (current) | "types don't have callees; the type's methods are [...]" | "consts don't have callees; the const's value is [...]" | files in this module | aggregate |
| `cqs test-map <name>` | tests covering this fn (current) | tests using this type | tests referencing this const | tests in this module | aggregate |
| `cqs trace <name>` | call-chain trace (current) | "types don't have call chains; here's where this type is used" | content references along chains | first-hop file imports | aggregate |
| `cqs deps <name>` | function's type deps | type's type deps (current) | "consts have no type deps; here's the const's content" | "modules don't have type deps in this view" | aggregate |
| `cqs explain <name>` | always works (chunk-shape) | always works | always works | always works | aggregate |
| `cqs read <name>` | always works (chunk-shape) | always works | always works | file content | aggregate |

The pattern: **if the command's primary intent doesn't apply to the detected kind, return the most-related useful information instead of empty.** Each cell of the matrix above is a handful of lines of Rust composing existing query helpers.

### Response shape: kind in the envelope

Every response from a kind-aware command includes a `kind` field at the top level (or, post-SNR-restoration, as a structured indicator on the response):

```json
{
  "kind": "const",
  "name": "HANDLING_ADVICE",
  "fallback_from": "impact",
  "results": [
    {"file": "src/cli/json_envelope.rs", "line_start": 73, ...},
    ...
  ],
  "note": "HANDLING_ADVICE is a const; call-graph impact does not apply. Showing content references."
}
```

The `note` is for humans reading the JSON; the `kind` and `fallback_from` are for agent programmatic consumption (so an agent can tell when its initial intent was rerouted and decide whether that's the answer it wanted or whether to retry differently).

Under SNR-restoration default (bare data on stdout, no envelope), the `kind` field lifts to the top level of the bare response. Agents always see it.

### Per-kind helper organization

Compose around a shared `KindIntel` trait:

```rust
trait KindIntel {
    fn for_function(name: &str, store: &Store) -> KindResult;
    fn for_type(name: &str, store: &Store) -> KindResult;
    fn for_const(name: &str, store: &Store) -> KindResult;
    fn for_module(name: &str, store: &Store) -> KindResult;
    fn for_ambiguous(name: &str, store: &Store, candidates: &[Candidate]) -> KindResult;
    fn for_freeform(query: &str, store: &Store) -> KindResult;
}
```

Each existing command implements `KindIntel` for its target kind (the current behavior is `for_function` for `impact`, etc.) and provides a "graceful degrade" implementation for the others. When the dispatcher detects a kind mismatch, it calls the appropriate degraded method.

Default degraded implementations are shared (e.g., `for_const` defaults to "find content references in chunks table" + the chunk's content). Commands override only when they have a smarter answer (e.g., `cqs deps <type>` knows about type_edges; `cqs deps <const>` falls back to chunk content because consts don't participate in the type graph).

## Phase 2 (contingent): unified `cqs about <name>` entry point

If Phase 1 lands and is well-received, Phase 2 adds a single polymorphic entry: `cqs about <name>`. Behavior:

```
cqs about <name>:
  detect kind
  match kind {
    Function   → bundle: definition + callers + callees + tests + content
    Type       → bundle: definition + usages + methods + content
    Const      → bundle: definition + references + content
    Module     → bundle: file listing + dominant chunks + exports
    Ambiguous  → bundle: all candidates with their per-kind summaries
    Freeform   → fallback: cqs scout (search + callers + tests + staleness)
    NotFound   → empty bundle + suggestion
  }
```

`cqs about` is the "I just want to know" entry. Distinct from `cqs scout <task>` which is task-shaped (where to add code, etc.).

**Phase 2 trigger conditions** (must all be true to ship):

- [ ] Phase 1 has shipped and been merged for ≥1 week
- [ ] Telemetry shows agent reach for kind-aware existing commands has shifted (specifically: `cqs impact / callers / test-map` calls succeed without follow-up grep at higher rates than pre-Phase-1)
- [ ] OR: explicit user request

If neither: file as a tracking issue, do not preemptively ship.

## Open questions to settle when executing

1. **Should `cqs <query>` (no subcommand) become polymorphic?** Today it's `cqs search "<query>"`. Extending it to "if `<query>` resolves to a single named symbol, return symbol intel; else search" would cover the most common path but breaks existing search-shape contract for any query that happens to match a name. Default position: keep `cqs <query>` as search-only; add `cqs about <name>` in Phase 2 as the polymorphic entry. Don't quietly change semantics of an existing command.
2. **Ambiguous-name disambiguation order.** `len` matches dozens of methods. Return all? Return the top N by chunk_count, file recency, or relevance? Default position: return all with a `count` field; let the caller paginate. If the response gets too large, the SNR-restoration audit will catch it.
3. **Cross-language name collisions.** `String` is a Rust type; `string` is also a Python concept. Should case sensitivity be loose? Default position: case-sensitive exact match. Loose matching is freeform query territory.
4. **Vendored / reference code in kind detection.** A `HashMap` match in a vendored crate vs the current project — both surface? Default position: filter detection results by `chunk.vendored == false` AND `ref_name IS NULL` for the friendly default; under `CQS_ULTRASECURITY=1` (or an explicit `--include-vendored` flag) include all.
5. **Performance under daemon batch dispatch.** Each kind detection adds ~1-2 ms. A batch of 100 dispatches is ~100-200 ms cumulative. Likely fine, but measure under load before assuming. If it bottlenecks, cache kind detection per (project_path, name) for a short TTL inside the daemon.
6. **Response shape stability across kinds.** `cqs impact <fn>` returns one shape; `cqs impact <const>` returns a different shape. Agent's parser has to handle both. Default position: every kind-mismatch response carries a `kind` field at top level + a `fallback_from` field naming the original command. Agents that care about the original-command shape can filter on `kind == Function`. The shape itself is per-kind specific; we don't try to flatten everything to one shape, because the per-kind information is genuinely different.

## Acceptance criteria

### Phase 1

- [ ] Kind detection helper (`detect_kind(name, store) -> Kind`) implemented + tested with all six kinds.
- [ ] Each function-or-type-specialized subcommand has a graceful-degrade path. The matrix above covers `impact`, `callers`, `callees`, `test-map`, `trace`, `deps`. (Commands like `explain` and `read` already work for any kind; no changes needed.)
- [ ] Every kind-mismatch response carries `kind` and `fallback_from` fields at the top level.
- [ ] No subcommand returns empty results for a name that exists in the index in any kind. Empty is reserved for genuine NotFound.
- [ ] Tests: per-command, per-kind. For each command in the matrix, write a test asserting the response shape for each kind it handles. Roughly 6 commands × 6 kinds = 36 tests; many are trivial smoke tests for the always-applicable commands.
- [ ] Telemetry-friendly logging: kind detection emits a `tracing::info_span!("kind_detection", kind = %k)` so post-merge analysis can correlate kind-mismatch frequency with command frequency.
- [ ] CHANGELOG entry under `[Unreleased]` describing the broadening.
- [ ] README updates documenting the kind-mismatch behavior (one paragraph; agents read this).

### Phase 2 (only if triggered)

- [ ] `cqs about <name>` subcommand wired to dispatch by kind.
- [ ] Per-kind bundle composers (each producing the right rich shape).
- [ ] Tests: per-kind bundle shape + ambiguous-name handling + NotFound suggestion.
- [ ] CHANGELOG + README + tracking issue closed.

## Cost estimate

| Phase | Work | Time |
|---|---|---|
| Phase 1: Kind detection helper + shared classifier | 1 SQL query + 30 lines Rust + tests | half day |
| Phase 1: Per-command graceful-degrade implementations | 6 commands × ~50 lines each | 2-3 days |
| Phase 1: Response shape (`kind` + `fallback_from`) | thread through 6 commands' output paths | half day |
| Phase 1: Per-command-per-kind tests | ~36 tests, mostly trivial | 1-2 days |
| Phase 1: Documentation (CHANGELOG, README, ROADMAP cross-ref) | half day |
| **Phase 1 total** | | **5-6 days** |
| Phase 2: `cqs about` subcommand + per-kind bundles | new command surface, kind dispatch logic | 3-4 days |
| Phase 2: Tests | per-kind bundle + ambiguous + NotFound | 1-2 days |
| **Phase 2 total** | | **4-6 days** |

**Phase 1: ~1 week solo. Phase 2: contingent, +~1 week if triggered.**

## Red-team caveats

### What if kind detection is the wrong primitive?

The detection-then-dispatch model assumes "name → unique kind" most of the time, with `Ambiguous` as the escape hatch. But many real names are ambiguous: `len`, `from`, `default`, `new`, `parse`. If `Ambiguous` becomes the dominant case, the per-command matrix simplifies (everything routes to "aggregate") and the design's main contribution is just "don't return empty."

**Mitigation:** measure the kind-detection distribution on a representative session before assuming the detection-then-dispatch is the load-bearing piece. If `Ambiguous` is >40% of detections, the design simplifies.

### What if "graceful degrade" hides real failures?

When `cqs impact CONST_NAME` falls back to content references, the agent might assume those are call-graph references and act on them. The structured `kind` + `fallback_from` fields mitigate this for agents that read them; agents that don't get burned. The mitigation is the response shape itself: degraded responses MUST be visibly different from primary-intent responses.

**Mitigation:** every degraded response carries `kind != original_target_kind` AND a `fallback_from` field. Tests assert both fields are present. Agent harnesses that key on `kind` get the correct interpretation.

### What if Phase 2 (`cqs about`) is the better default?

Phase 1's per-command broadening is conservative — preserves existing command shapes, adds graceful failure. But maybe agents WANT a single entry that handles everything; the per-command surface is the legacy artifact, not the right shape.

**Mitigation:** ship Phase 1 first, observe whether agents drift toward `cqs about` (Phase 2) or keep using existing commands. If `cqs about` becomes the dominant entry over 1-2 weeks, deprecate the per-command surface in a future release. If existing commands stay dominant, keep both.

### What if the eval harness or daemon protocol breaks?

Adding a `kind` field at the top level of responses is technically a shape change. Existing parsers that destructure on the current shape (`results: [...]`) might choke if `kind` appears alongside.

**Mitigation:** the SNR-restoration design (`docs/json-snr-restoration.md`) is shipping in parallel and includes the same migration concerns (eval harness + daemon batch protocol). Land both designs in the same release window so consumer migration is one-time, not two times.

### What about names that look like queries?

`cqs <query>` for `"hash map"` (with spaces) is unambiguously freeform — no symbol named "hash map" exists. But `cqs <query>` for `"HashMap"` could be either freeform OR a name match. Default position: prefer name match when exact, fall through to search when no exact match. Don't treat single-word vs multi-word as a heuristic; that's brittle.

## What this is for, in one sentence

Make every cqs command return a useful, kind-labeled response when called with a name that isn't quite the kind it was built for, so agents stop hitting the misrouted-to-empty failure mode that nudges them toward grep.

---

## Appendix A: relationship to SNR restoration

This design and `docs/json-snr-restoration.md` are siblings under "Agent Adoption — Telemetry > Friction backlog." They address different friction surfaces:

| Friction | Design | Approach |
|---|---|---|
| Response is too noisy → agent attention bloats | SNR restoration | Bare wire format on success; envelope-as-opt-in |
| Wrong subcommand for the kind → empty result | Polymorphic routing | Kind detection + graceful degrade per command |

Both reduce the friction that pushes agents to grep. Neither alone is sufficient; together they give cqs's surface the responsiveness of a tool that "just works" for code-intelligence questions regardless of what the agent asks for or how they ask it.

Ship them in the same release window if possible — consumer migration concerns overlap (eval harness, daemon batch protocol, agent harnesses). One round of breakage is cheaper than two.

## Appendix B: explicit non-coupling with #1593

This design does NOT depend on the SNR restoration shipping first. Phase 1 (graceful broadening) works under either response shape — bare data or full envelope. If only one of the two designs ships, this one is the higher-value pick because misrouted-to-empty is a binary failure (no answer) where SNR is a continuous tax (more attention per answer). A failed query is worse than an expensive answer.

Recommended order if shipping serially:
1. Phase 1 of polymorphic routing (this doc, ~1 week)
2. SNR restoration (~1 week)
3. Phase 2 of polymorphic routing only if triggered

That ordering surfaces the higher-leverage fix first and lets the SNR work observe a baseline post-broadening.
