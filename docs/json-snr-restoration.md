# JSON SNR Restoration — Design

**Status:** Phases 1-4 shipped in v1.40.0 (2026-05-08; PRs #1601/#1602/#1604/#1609/#1613). Phases 5-6 (per-source rate limit, tracing-noise-suppress) scoped out — telemetry-contingent. Breaking wire-format change on the CLI direct path; `CQS_OUTPUT_FORMAT=v1` is the consumer-migration hedge.
**Date:** 2026-05-08 (design + implementation in one cycle; supersedes `docs/json-noise-audit.md` from earlier same day; the audit framing was incremental, the actual problem turned out to need a wire-format simplification)
**Location:** `docs/json-snr-restoration.md`
**Tracking:** ROADMAP.md "Agent Adoption — Telemetry > Friction backlog"
**Cross-reference:** PR #1593 (always-on advisory inversion), telemetry analysis 2026-05-08.

---

## Plain-language opener

cqs's response shape has accumulated overhead over the project's history. Telemetry shows a real consequence: agents have moved away from the high-frequency, low-attention `cqs search` call. **Search dropped from 79% of code-intel calls in mid-April 2026 to 6% in early May 2026.** That's not voluntary tool refinement — that's agents avoiding a noisier surface. The envelope `{data, error, version, _meta}` plus per-result decorations (`trust_level`, `injection_flags`, `has_parent`, `signature`, `chunk_type`, `language`) makes every response carry ~10x more attention-weight than the bare answer the agent asked for. **High-SNR responses get used more.**

This design restores response shape to a high-SNR baseline. Default behavior emits bare data on stdout for success; structured error on stderr with non-zero exit for failure. The envelope and per-result metadata are preserved, but only emitted under `CQS_ULTRASECURITY=1` (where the verbose shape is what the adversarial-deployment caller actually wants).

## Telemetry evidence (the load-bearing observation)

`.cqs/telemetry*.jsonl` archives show the trend cleanly:

| Period | Total invocations | Code-intel | `search` count | Search/intel |
|---|---:|---:|---:|---:|
| 2026-04-04 → 04-09 | 3.9k | 3.5k | 2,337 | **66%** |
| 2026-04-09 → 04-23 (peak search era) | 44.9k | 7.1k | 5,636 | **79%** |
| 2026-04-23 → 04-28 | 887 | 598 | 13 | **2%** |
| 2026-04-28 → 05-02 | 873 | 371 | 42 | **11%** |
| 2026-05-02 → 05-08 (now) | 2.7k | 727 | 45 | **6%** |

The drop is real and persistent. It corresponds to the period during which the response shape kept accumulating fields — handling_advice (#1181), trust_level / injection_flags (#1167, #1221), chunk_type/language defaults, the `_meta.worktree_stale` plumbing (#1254). Each addition lowered SNR; agents stopped reaching for the cheap `search` call as readily and shifted toward the structured surfaces (`explain`, `impact`, `scout`) where the higher attention-weight is amortized over more value per call.

That shift is fine for high-leverage queries. It's bad for low-leverage browsing — which is where `search` lives. **Restoring SNR makes browsing cheap again.**

## What "high SNR" means here

For an agent caller, the highest-SNR response is the bare answer. A `cqs search "foo" --json` response that's a flat JSON array of `{file, line, name, score, content}` objects is what the agent actually wants. Anything else is overhead — sometimes load-bearing (errors), often not.

The current shape adds:
- An envelope wrap (`{data, error, version, _meta}`) on every response
- A `_meta` block carrying handling_advice (#1593-default-off but still appears under `CQS_ULTRASECURITY=1`), worktree_stale, worktree_name
- Per-result fields: `trust_level: "user-code"` (default 99% of the time), `injection_flags: []` (default 99% of the time), `has_parent: false` (default), `signature: ""` (default for many chunk types)

Adding up the bytes per response: a chunk-shape result that should be ~150 bytes is closer to ~400 bytes. **3x bloat in the typical case.** Multiply by 20-50 results per search; multiply by N searches per agent session; the cumulative attention-weight is significant.

## Goal

**Default response on success: bare payload on stdout, no envelope, no metadata.** A `cqs search` returns `[{...}, {...}]` directly. A `cqs explain` returns the explanation object directly. Errors emit structured JSON to stderr with non-zero exit code. Verbose envelope shape (current behavior) is preserved as opt-in via `CQS_ULTRASECURITY=1` for adversarial deployments that need the metadata.

## Non-goals

- Removing the envelope from the **batch / daemon JSONL protocol**. That protocol's whole point is line-by-line uniform parsing; each line has to be self-describing. The envelope is load-bearing there. Slim the envelope (drop `version`, skip-when-default `_meta`), don't drop it.
- Changing the text (non-`--json`) output. That's `--quiet`'s domain; orthogonal concern.
- A 4-level verbosity ladder. Already rejected; YAGNI math holds.
- New top-level CLI flag for verbosity. The default IS the lean shape; ULTRASECURITY is the only knob.
- Breaking change to text/exit-code semantics. Errors → stderr + non-zero exit is already the convention; reinforce, don't redefine.
- Polymorphic command routing (sibling roadmap item).

## The new wire format

### CLI direct invocations (`cqs <cmd> [args] --json`)

**Success:** bare JSON payload on stdout. Exit 0.
- `cqs search "foo" --json` → top-level JSON array of result objects
- `cqs explain my_func --json` → top-level explanation object
- `cqs stats --json` → top-level stats object
- Result objects use the slimmed per-result shape (below)

**Failure:** structured JSON to stderr, non-zero exit.
- stderr: `{"error": {"code": "not_found", "message": "..."}}`
- stdout: empty
- exit code: 1 for `not_found` / `invalid_input`, 2 for `internal`, etc.

**Why this works for agents:** the universal error check becomes `if exit_code != 0` — already CLI-canonical. Agents shelling out cqs and piping stdout through `jq`/Python already get this for free. The envelope's `.error` field check was redundant with exit code anyway.

### Batch / daemon JSONL protocol

Envelope structure stays. Slimmed:

**Success line:** `{"data": <payload>}` — drop `error: null`, drop `version`, drop `_meta` when empty
**Failure line:** `{"error": {"code": "...", "message": "..."}}` — drop `data: null`, drop `version`, drop `_meta` when empty

A consumer parses each line:
- `if "error" in obj: handle failure` — single contract test
- `else: payload = obj["data"]`

This preserves uniform per-line error handling while shedding 60-80 bytes per line. For a fixture batch of 1000 lines, that's ~70 KB saved.

### Verbose mode (`CQS_ULTRASECURITY=1`)

Restores the current envelope shape:
- CLI: full `{data, error, version, _meta}` to stdout on both success and failure
- Batch/daemon: full envelope per line
- Per-result: force-emit `trust_level`, `injection_flags` (even when empty), `_meta.handling_advice`

This is the adversarial-deployment shape. Operator opts in once at process start; everything is verbose.

### Per-result shape (chunk-shaped results)

Currently emitted by `build_chunk_json_inner` at `src/store/helpers/types.rs:220`. New rules:

| Field | Always | Skip when default | Posture-gated (force-emit under `CQS_ULTRASECURITY=1`) |
|---|---|---|---|
| `file` | ✓ | | |
| `line_start` | ✓ | | |
| `line_end` | ✓ | | |
| `name` | ✓ | | |
| `score` | ✓ | | |
| `content` | ✓ | | |
| `language` | ✓ | | |
| `chunk_type` | ✓ | | |
| `signature` | | skip when `""` | |
| `has_parent` | | skip when `false` | |
| `trust_level` | | skip when `"user-code"` | force-emit |
| `injection_flags` | | skip when `[]` | force-emit (even when `[]`) |
| `reference_name` | | already conditional, no change | |

`language` and `chunk_type` stay always-emitted: agents doing multi-language search use them to filter, and the values aren't a "default" the same way `trust_level: "user-code"` is.

### Posture as parameter, not env-read in leaf

`build_chunk_json_inner` is called per chunk. Reading `CQS_ULTRASECURITY` via `std::env::var` per call is one syscall × N chunks. Cheap, but stylistically wrong: leaf serializer becomes process-state-dependent.

**Right shape:** read the env var ONCE at request entry (CLI dispatch / batch dispatch / daemon-handler entry), convert to a `Posture` enum value, thread through:

```rust
#[derive(Debug, Clone, Copy)]
enum Posture {
    Friendly,    // skip-when-default rules apply; bare wire format
    Adversarial, // force-emit security signals; full envelope
}

impl Posture {
    fn current() -> Self {
        if std::env::var("CQS_ULTRASECURITY").as_deref() == Ok("1") {
            Self::Adversarial
        } else {
            Self::Friendly
        }
    }
}
```

Threading the parameter is mechanical (~12-15 call sites). Worth doing now to avoid the process-state-dependent leaf-serializer smell and to keep tests deterministic.

## Migration plan

### In-tree consumers (must update in same PR set)

1. **`evals/run_*.py` and friends.** Check current parse logic; update to handle bare-data-on-stdout success path. The eval harness is the most-exercised JSON consumer; if it breaks, regression tests fail loudly. Part of acceptance gate.
2. **Daemon batch consumer code paths.** `cqs batch` against the running daemon — verify the slimmed JSONL line shape (`{"data": ...}` or `{"error": ...}`) parses without modification. Test with the existing fixture batch.
3. **The chat REPL surface in cqs itself.** `cmd_chat` consumes the same envelope shape; trace through and update.
4. **Any tracking-script that greps cqs JSON output.** Check `evals/`, `scripts/` (if any), `tools/`.

### External consumers (none, by policy)

Per `~/.claude/projects/-mnt-c-Projects-cqs/memory/MEMORY.md` "no external users" — stranger pulling cqs from crates.io is not a constraint. Bump CHANGELOG entry under a minor version (v1.40.0 likely) since this is a breaking JSON output change.

### Tests

Updates required across:
- `src/cli/json_envelope.rs` — most assertions about envelope structure need updating
- `src/cli/commands/**/*.rs` — per-command output tests probably assert `data` field presence; flip to bare payload
- `tests/router_test.rs`, `tests/env_var_docs.rs` — likely orthogonal; verify
- New: integration tests for friendly-mode bare output AND adversarial-mode envelope output

## Acceptance criteria

- [ ] Discovery step run: `find src/ -name '*.rs' -exec grep -lE 'derive\(.*Serialize' {} \;` → audit each result for envelope-coupling.
- [ ] `Posture` enum + threading through `build_chunk_json_inner` and all CLI / batch entry points.
- [ ] CLI direct success path emits bare payload on stdout (no envelope). Test with `cqs search "foo" --json | jq type` returning `array`, not `object`.
- [ ] CLI direct failure path emits structured JSON to stderr + non-zero exit. Test with `cqs search /nonexistent --json` returning exit 1, stderr containing `error.code`.
- [ ] Batch / daemon JSONL lines emit slimmed envelope: `{"data": ...}` or `{"error": ...}`. No `version` field. No empty `_meta`.
- [ ] `CQS_ULTRASECURITY=1` restores full envelope shape: `{data, error, version, _meta: {handling_advice, ...}}` on every response, both surfaces.
- [ ] Per-result shape: skip-when-default for `trust_level == "user-code"`, `injection_flags == []`, `has_parent == false`, `signature == ""`.
- [ ] Eval harness tested end-to-end against post-change binary. All eval scripts pass without modification (or with documented one-time updates in same PR set).
- [ ] Daemon batch consumer fixture tested. Passes.
- [ ] Pre-vs-post byte-count comparison documented in PR description (representative `cqs search "foo" --json` and `cqs explain my_func --json`). Expect 60-75% reduction; record actual.
- [ ] CHANGELOG entry under v1.40.0 (breaking change to JSON output shape; minor bump justified).
- [ ] README JSON-output examples updated.
- [ ] No new env vars or CLI flags. `CQS_ULTRASECURITY=1` already exists.

## Cost estimate

| Phase | Work | Time |
|---|---|---|
| Discovery + struct inventory | walk src/ Serialize derives, classify | 2-3 hours |
| `Posture` enum + threading through entry points | mechanical | half day |
| `json_envelope.rs` rewrite (bare success path + slimmed batch envelope + posture-gated full mode) | core change | 1 day |
| Per-result `skip_serializing_if` + `build_chunk_json_inner` posture-gating | per-field cleanup | half day |
| Test rewrites (per-command + cross-cutting) | substantial | 2-3 days |
| Eval harness verification + any one-time updates | survey + fix | 1 day |
| Daemon batch consumer verification | survey + fix | half day |
| CHANGELOG + README + PR description (with byte-count tables) | docs | 2 hours |

**Total: ~1 week solo.** Reasonable for a wire-format change that touches every JSON-emitting site.

## Red-team caveats (read before executing)

### What if the telemetry trend has another cause?

Possible alternative explanations for the 79% → 6% search drop:
1. **Subagent dispatch pattern shifted.** When custom agents (`investigator`, `code-reviewer`, etc.) became the standard pre-implementation step, their tool reach is `scout`/`impact`/`gather` — not raw `search`. The trend might reflect *agent dispatch architecture* rather than per-call SNR.
2. **The pre-edit hook bridged the gap.** Pre-edit-hook auto-runs `impact`; that's structured, not search. Each pre-edit cycle adds an `impact` call and not a `search` call, mechanically shifting the ratio.
3. **The user's own usage shifted.** I (the agent) might have learned that `explain`/`impact` answer my actual questions better than `search`, independent of SNR considerations.

If any of these is the dominant cause, restoring SNR won't fully recover the search frequency. **The work still has hygiene value**, but the agent-adoption framing is weaker than the doc claims.

**Mitigation:** ship the change, then watch telemetry over the next 1-2 weeks. If `search` rate climbs back toward 30-50% of code-intel calls, the SNR-restoration hypothesis is supported. If it stays at 6%, the cause was structural (subagents, pre-edit hook) and the SNR work was hygiene only.

### What if breaking the envelope on CLI breaks something I haven't surveyed?

Memory says "no external users" but the eval harness, the chat REPL, and any custom scripts in `evals/` or elsewhere ARE consumers. The migration plan lists the known ones; an unknown consumer is the risk.

**Mitigation:**
1. Land the change behind a `CQS_OUTPUT_FORMAT=v1|v2` env-var feature flag for one release. Default is `v2` (the new shape); `v1` restores the old shape.
2. Keep the flag for one release cycle. If nothing breaks, remove the flag and the old shape in v1.41.0.
3. Document the flag in CHANGELOG with explicit "if your script breaks, set `CQS_OUTPUT_FORMAT=v1` and file an issue."

### What if posture-as-parameter is more invasive than the doc claims?

Threading a parameter through `build_chunk_json_inner` is mechanical for the ~12 direct call sites. But the function is also called transitively from any code path that produces chunk-shaped output for serialization, and those call paths might not have a `Posture` available without further plumbing.

**Mitigation:** if threading turns out to need 30+ call sites instead of ~12, accept the env-read-in-leaf style for this iteration; revisit later. The style smell is real but not load-bearing for SNR restoration.

## Phase ordering inside the PR

If this lands as one PR, structure it as commits in this order so review can proceed incrementally:

1. **Add `Posture` enum** + thread through entry points. No behavior change yet.
2. **Apply `skip_serializing_if`** to per-result fields. Friendly mode now emits less per result; envelope structure unchanged. Tests updated.
3. **Slim batch / daemon envelope** (drop `version`, skip-empty `_meta`). Tests updated.
4. **Switch CLI direct success to bare payload.** Add the `--json` bare-output behavior. Tests + eval harness updated.
5. **Wire `CQS_ULTRASECURITY=1` to restore full envelope** + force-emit per-result security fields. Posture-mode tests added.
6. **Documentation** — CHANGELOG, README JSON examples.

Each commit independently reviewable. PR-level review covers integration.

If the discovery in step 1 surfaces unexpected complexity, the PR can be split: commits 1-2 (additive cleanup, no breaking change) ship first; commits 3-5 (breaking change) ship together as v1.40.0.

## Out of scope

- Polymorphic command routing (sibling roadmap item).
- Pretty-printed text-mode output. Stays as is.
- Daemon socket protocol changes beyond the envelope slim.
- Schema migrations.
- HTTP server (`src/serve/`) response shape — different protocol entirely; survey separately if it ever ships agent-facing.

## What this is for, in one sentence

Restore cqs's response shape to a high-SNR baseline so agent calls are cheap again, with a `CQS_ULTRASECURITY=1` opt-in preserving the verbose envelope+metadata for adversarial deployments that need it.

---

## Appendix A: changes from `docs/json-noise-audit.md` (earlier same day)

The audit version's framing was incremental — "skip-when-default per field" — and would have produced ~30% byte reduction without changing the envelope. The telemetry pull (search dropping from 79% to 6%) showed the envelope itself is part of the noise, not just the per-result decorations. This rewrite escalates from per-field hygiene to wire-format simplification.

Specific shifts:
- **Premise:** from "hygiene cleanup" to "restore measured SNR." Telemetry is the load-bearing evidence, not just a contextual observation.
- **Scope:** from per-field `skip_serializing_if` only, to envelope structure on the CLI direct path.
- **Default behavior:** the lean shape becomes the default; the verbose shape becomes opt-in via `CQS_ULTRASECURITY=1`. Inverts the polarity that's been in place.
- **Cost:** from 3-4 days to ~1 week. Bigger surgery, more migration.
- **Phase 3 (`--lean`):** dropped entirely. The default IS lean now.
- **Acceptance:** byte-count comparison is now an explicit acceptance gate (record pre/post; expect 60-75% reduction).
- **Red-team:** explicit alternative-cause caveats for the telemetry trend; feature-flag mitigation for unknown consumers.

The audit version stays as-is in `docs/json-noise-audit.md` for traceability — a future-me reading both files sees the framing evolve from "polish per-field" to "restore wire SNR."
