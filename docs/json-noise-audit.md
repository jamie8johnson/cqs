# JSON Noise Audit — Design

**Status:** ready to execute (Phase 1+2). Phase 3 is contingent and explicitly gated; see "Phase 3 trigger" below.
**Date:** 2026-05-08 (red-team-revised; supersedes the first cut from same day)
**Location:** `docs/json-noise-audit.md`
**Tracking:** ROADMAP.md "Agent Adoption — Telemetry > Friction backlog"
**Cross-reference:** PR #1593 (the always-on advisory inversion that already shipped). This doc covers the residual cleanup, NOT a re-litigation of #1593.

---

## Plain-language opener

cqs's JSON output carries fields like `trust_level: "user-code"`, `injection_flags: []`, `worktree_stale: false`, `has_parent: false` on every result. In the deployment cqs actually has — operator owns the indexed code AND the indexer (memory: "no external users") — these fields are noise: their default values match the implicit-when-absent meaning, and the agent caller doesn't extract information from their presence. PR #1593 (2026-05-08) addressed the loudest piece: `_meta.handling_advice` is now opt-in via `CQS_ULTRASECURITY=1`. **This doc covers the residual cleanup**: per-field `skip_serializing_if` audits across every `Serialize`-deriving struct in the JSON-emitting paths.

## Honest framing

This is **hygiene work, not a confirmed-bottleneck fix.** The original observation that motivated this thread — "an agent drifted toward grep when cqs output was in the response" — was contextual: most of that grep usage was in admin/grooming work where literal-string matching is the right tool, and the alarming-shaped fields that genuinely affected agent behavior (handling_advice especially) were addressed by #1593's default-off inversion. After #1593, the residual noise is ergonomic but not load-bearing.

So: don't expect this audit to produce a dramatic agent-adoption bump. Expect it to produce **smaller responses, cleaner debug output, less confusion about what cqs metadata fields mean**, and a foundation that makes the polymorphic-routing roadmap item easier to ship cleanly. If those benefits aren't worth ~3-4 days of work, the audit shouldn't ship.

## Why an alternative architecture was rejected

A 4-level verbosity ladder (`bare` / `lean` / `standard` / `full`) was the first proposal. Red-team killed it for five reasons. Listed here so a future-me doesn't re-litigate:

1. **Misdiagnosed cause-of-friction.** The blocker was *alarming* metadata, not *volume*. #1593 already addresses the alarming piece. Volume is ergonomic.
2. **Posture isn't a separate axis.** `CQS_ULTRASECURITY=1` clamping verbosity to `full` is just "verbosity setting locked at full" — the axes weren't really orthogonal.
3. **Phantom levels.** `bare` would break universal error-handling. `full` is only used under posture lock. `lean` and `standard` is just a boolean.
4. **Wrong frame for per-field decisions.** "What level does this field belong at?" is harder than "Is this field worth emitting when its value is the default?" — the latter has a clear answer.
5. **YAGNI math.** 4-level enum + level-clamping + posture wiring + test matrix = ~2 weeks. Per-field audit + contingent `--lean` boolean = ~3-4 days. Same agent-UX win in the friendly case.

The architectural lesson: **fix per-field defaults first; reach for top-level mode flags only when the per-field cleanup leaves visible noise.**

## Goal

Reduce friendly-deployment JSON output to the smallest field set that meaningfully informs the agent caller, while preserving adversarial-deployment fidelity under `CQS_ULTRASECURITY=1`. Implemented as `serde` skip-when-default attributes on existing `Serialize` structs; no new flags or env vars in Phase 1+2; no breaking shape changes; no architectural surgery.

## Non-goals

- Four-level verbosity ladder (rejected; see above)
- Generalized posture-axis abstraction (`CQS_ULTRASECURITY=1` stays as the single, narrow opt-in)
- Stripping the `{data, error, version, _meta}` envelope structure (would break universal-error-handling parity for any consumer that relies on `.error` being present-but-null)
- Polymorphic-routing API surface (sibling roadmap item; out of scope here)
- Schema changes to `Serialize` field types (e.g., changing `String` → `Option<String>`). Skip-when-default attributes only; no type changes.
- Pretty-printed text output (`--quiet` and per-command human-readable formatters). This audit is JSON-only.

## The semantic bet (read this before executing)

Phase 1's per-field `skip_serializing_if` is **a bet that consumers interpret absent-as-default**. There's a non-zero risk that some consumer interprets `absent` as `unknown` rather than `default-value`, and that breaks them. Surveys before merging:

- **The eval harness (`evals/run_*.py`, etc.)** is the most-exercised JSON consumer. Test it against the post-audit cqs binary before merge.
- **The daemon batch socket consumer** (anyone running `cqs batch` against the running daemon). Test the existing fixture-batch script.
- **Agent harnesses (Claude Code MCP, etc.)** are unknown unknowns; they're not exercised in CI.

Mitigation if the bet looks wrong on a given field: leave that field always-emitted, add a comment justifying it, move on. Per-field cost; partial audit is fine.

## Field-decision precedence

When a field could plausibly fit multiple buckets, apply this precedence:

1. **Posture-gated** beats skip-when-default beats always-emit.
   *Example:* `trust_level: "user-code"` is BOTH a security signal AND a default value. Under `CQS_ULTRASECURITY=1` it's force-emitted (posture wins). In friendly mode it's skipped (skip-when-default fires).
2. **Skip-when-default** beats always-emit when the default is the implicit-when-absent meaning.
   *Example:* `worktree_stale: false` — absent and false carry the same meaning to the agent.
3. **Always-emit** when neither rule applies.
   *Example:* `score: 0.0` — score zero carries the information "we evaluated this and it ranked at the floor"; absent would mean "we didn't compute a score." Different meanings; never skip.

The precedence is normative, not descriptive. A field that LOOKS like it belongs in two buckets gets the higher-precedence treatment.

## Inventory: discover, then execute

Phase 1 starts with discovery, not with the partial list below. The structs I found via cqs + grep on 2026-05-08 are a non-exhaustive starting point — I missed at least the `src/cli/batch/`, `src/serve/`, `src/daemon_translate.rs`, and probably-others paths.

**Discovery step (do first):**

```bash
# Find every Serialize-deriving struct in src/ that lands on JSON output.
find src/ -name '*.rs' -exec grep -lE 'derive\(.*Serialize' {} \; \
  | xargs grep -lE '#\[derive.*Serialize' \
  | grep -v test \
  | sort -u
```

Then triage each file: which structs end up serialized to JSON output (vs internal-only types that happen to derive `Serialize` for `serde_json::to_value` round-trips)? The CLI command modules and `src/store/helpers/types.rs` are the high-density zones.

**Known starting points** (non-exhaustive):

- `src/store/helpers/types.rs:220` `build_chunk_json_inner` — canonical chunk-shape emitter; covers every search/gather/scout/context/read result. **Phase 1.5 anchor.**
- `src/cli/json_envelope.rs` — `Envelope<T>`, `EnvelopeMeta`, the typed and hand-built emission paths. Already mostly clean post-#1593.
- `src/cli/commands/eval/{baseline,runner}.rs` — eval output structs.
- `src/cli/commands/graph/{callers,deps,explain,trace,impact}.rs` — graph command output structs. Several have no `skip_serializing_if` today.
- `src/cli/commands/io/{read,context,notes}.rs` — chunk-shaped result emitters via the canonical site, plus per-command wrappers.
- `src/cli/commands/index/stats.rs` — `StatsOutput` (the v1.39.2 GC PR added per-chunk coverage fields; re-walk for older fields).
- `src/cli/commands/search/{gather,scout}.rs` — search-shape output structs.
- `src/cli/batch/mod.rs` — `write_json_line` and any batch-specific output structs.
- `src/serve/` — HTTP server payload types.
- `src/daemon_translate.rs` — daemon socket protocol types (the `{status, output}` framing references envelope shape).
- `src/store/notes.rs` — `NoteSummary` and friends.
- `src/index.rs` — `IndexResult`.

**The discovery step is non-optional.** Don't trust the list above; run the find command, audit what it returns, file what you find against the precedence table.

## Phase 1.5 anchor: `build_chunk_json_inner`

This function at `src/store/helpers/types.rs:220` is the single highest-leverage site. Every chunk-shaped JSON result flows through it. Three field changes recommended; one design improvement required.

### Field changes (apply skip-when-default)

| Field | Default | Change | Posture override |
|---|---|---|---|
| `has_parent` | `false` | Skip when false (already a `bool` default; trivial) | Always-emit policy unchanged |
| `trust_level` | `"user-code"` (when `ref_name=None && !chunk.vendored`) | Skip when value equals `"user-code"` | Force-emit under `CQS_ULTRASECURITY=1` |
| `injection_flags` | `[]` | Skip when empty array | Force-emit even when `[]` under `CQS_ULTRASECURITY=1` (the "we checked, found nothing" signal IS load-bearing in adversarial mode) |
| `signature` | `""` (markdown chunks, certain language types) | Skip when empty string | Always-emit policy unchanged |

`reference_name` is already conditionally added; no change.

### Design improvement (required, not optional): pass posture as a parameter

`build_chunk_json_inner` is called per chunk; a result with 50 chunks invokes it 50 times. Reading `CQS_ULTRASECURITY` via `std::env::var` per call is one syscall × 50 = ~50 µs. Cheap, but it's *style-wrong*: the leaf serializer becomes process-state-dependent, which makes tests brittle and forecloses any future "posture per request" use case (e.g., if cqs ever serves multiple projects per process and one is adversarial-shaped).

**Right shape:** read `CQS_ULTRASECURITY` ONCE at request entry (CLI dispatch / batch dispatch / daemon-handler entry); convert to a `Posture` enum value; thread that through to `build_chunk_json_inner` as a parameter:

```rust
#[derive(Debug, Clone, Copy)]
enum Posture {
    Friendly,    // skip-when-default rules apply
    Adversarial, // force-emit security-signal fields even when default
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

fn build_chunk_json_inner(
    &self,
    ref_name: Option<&str>,
    base: Option<&Path>,
    posture: Posture,  // NEW
) -> serde_json::Value { ... }
```

Threading the parameter is mechanical (~12 call sites). Worth doing now to avoid the "process-state-dependent leaf serializer" smell.

## Tests (Phase 2)

For every struct touched in Phase 1+1.5:

1. **Default-state shape contract.** Construct the struct with all default values, serialize, assert specific *field absences* — not byte counts. Pin the expected JSON literal.
   ```rust
   #[test]
   #[serial_test::serial]
   fn chunk_json_default_state_omits_user_code_trust_level() {
       std::env::remove_var("CQS_ULTRASECURITY");
       // ... build a default chunk, serialize via build_chunk_json_inner ...
       assert!(v.get("trust_level").is_none());
       assert!(v.get("injection_flags").is_none());
       assert!(v.get("has_parent").is_none());
   }
   ```
2. **Non-default emission contract.** Set ONE field to a non-default value; assert it now appears.
3. **Posture override contract** (only for posture-gated fields). Set `CQS_ULTRASECURITY=1`; assert force-emission. `serial_test::serial`. Always restore env to original after the test (`scopeguard::defer!`-style or explicit `std::env::remove_var` in test cleanup; the existing `json_envelope.rs` tests use the explicit pattern).
4. **`Posture` parameter threading test.** Construct the function with `Posture::Adversarial` directly (without setting the env var); assert force-emission. This pins the *parameter* contract, decoupled from env var state.

**Cross-cutting measurement (descriptive, not a gate).** Capture a representative `cqs search "foo" --json` against a fixed fixture before and after the audit; record the byte counts in the PR description. **Do not assert a percentage reduction in tests** — the number depends on the fixture, the query, and which structs landed in this PR. Recording it in the PR is honest; gating tests on it is fake precision.

## Acceptance criteria

- [ ] Discovery step run; full struct inventory documented in the PR description (or in a follow-up scratch doc).
- [ ] Every struct in the inventory has been reviewed; per-field decisions documented in the PR description.
- [ ] Every field has either: explicit always-emit (with a one-line comment justifying it), `skip_serializing_if`, or a posture-gate.
- [ ] `Posture` enum + threading through `build_chunk_json_inner` shipped (the design improvement above).
- [ ] All struct-level tests above pass.
- [ ] Eval harness (`evals/run_*.py`) tested against the post-audit binary; output parses without modification.
- [ ] Daemon batch consumer (e.g. an existing `cqs batch` fixture) tested; passes.
- [ ] Pre-vs-post `cqs search "foo" --json` byte counts recorded in the PR description.
- [ ] No new env vars or CLI flags. Phase 3's `--lean` is contingent; do not ship without explicit triggering criteria (see "Phase 3 trigger" below).
- [ ] No breaking shape change. Existing parsers continue working. Where a field changed from "always present" to "skip-when-default," document it in the PR description so a future debugger sees the change.
- [ ] CHANGELOG entry under `[Unreleased]` describing the audit + the surveyed-consumer test results + the byte-count win.
- [ ] README env-var table unchanged.

## Phase 3 (`--lean` flag) — explicit trigger condition

Phase 3 ships ONLY if all of the following are true:

- [ ] Phase 1+2 has shipped and been merged to main for ≥1 week
- [ ] An agent session has shown observable cqs-output noise affecting tool-choice behavior AFTER Phase 1+2 (concrete example: an agent grep'd when cqs would have answered, AND the agent harness reports this was due to JSON volume rather than alarm-shaped content)
- [ ] OR the user explicitly requests it
- [ ] At least one downstream consumer has confirmed Phase 1+2's metadata reduction is insufficient for their use case

**Without all of the above, file Phase 3 as a tracking issue and move on.**

If shipped, the `--lean` flag's behavior:

- Per-command CLI flag (NOT process-global env var; this is a per-call output preference)
- Suppresses the entire `_meta` block on the wire (envelope shrinks to `{data, error, version}`)
- **Cleaner alternative to wholesale `_meta` strip:** apply skip-when-default at the BLOCK level. Emit `_meta` only when at least one of its fields would emit; otherwise omit `_meta` entirely. This preserves the worktree-stale signal (which IS load-bearing when true) without forcing per-call decisions about it.
- Cost: ~1 day. Defer until triggered.

## Open questions to settle when executing

1. **`Envelope.version: 1` skip-when-default.** Skip saves ~15 bytes per response but might break consumers that gate on the field's presence. Run `grep -rn "version.*1\|version.*== 1" tests/ evals/` and check the daemon batch consumer; if any code asserts on `version` as a contract field, leave as-is.
2. **The `Posture` enum threading scope.** Once threaded into `build_chunk_json_inner`, should it ALSO be threaded into command-level Output structs? Probably yes for any struct with security-relevant fields; probably no for purely-informational outputs (eval reports, stats). Decide per struct; document in PR.
3. **`signature: ""` skip-vs-Option-rewrite.** Markdown chunks have empty-string signatures. Skip-when-empty is the no-breaking-change choice; rewriting `signature: String` → `signature: Option<String>` is the cleaner type but breaks any consumer destructuring on `.signature: String`. **Default position: skip-when-empty; do not rewrite the type.**
4. **Per-result `language` skip-when-matches-the-query-filter.** Tempting (if user passed `--lang rust`, every result's `language: "rust"` is redundant) but skip-when-default needs a static rule, not a contextual one. Phase 3 territory at most; out of scope here.
5. **What's the `injection_flags` default-empty behavior under `CQS_ULTRASECURITY=1`?** The doc says "force-emit even when empty," but is `[]` distinguishable from "we didn't check"? In current code the answer is "yes, `[]` means we checked and found nothing" — confirmed by tracing through `detect_all_injection_patterns`. Pin this in a test so a future refactor doesn't accidentally introduce "we didn't check" semantics.

## Cost estimate (honest revision)

| Phase | Work | Time |
|---|---|---|
| **Discovery** | `find`/`grep` walk of `src/`, audit results | 1-2 hours |
| **Phase 1**: Per-struct `skip_serializing_if` application | ~15-20 structs | 1 day |
| **Phase 1.5**: `Posture` enum + threading + `build_chunk_json_inner` changes | 1 enum + ~12 call sites | half day |
| **Phase 2**: Tests (default-state contracts + non-default + posture × structs) | per-struct test pair × ~15-20 structs | 1-1.5 days |
| **Consumer survey** (eval harness + daemon batch fixture) | 2 known consumers, ~2 hours each | half day |
| **Documentation** (CHANGELOG, PR description, byte-count measurements) | summary + table | 2 hours |

**Total: 3-4 days solo.** Half of which is test writing — that's not a flaw, that's the discipline working.

## Discipline follow-up: prevent re-decay

The audit produces a clean snapshot. Without an enforcement mechanism, new fields added later will default to always-emit and the noise rebuilds. **File a separate tracking issue** for:

- A discipline test that walks all `Serialize`-deriving structs in JSON-emitting paths and asserts every field has either an explicit `// always-emit` comment OR a `skip_serializing_if` attribute. Hard to write without a custom syn-based check, but plausible as a clippy lint or a small custom test.

This is OUT OF SCOPE for the audit PR itself; file it as a follow-up so the audit's work doesn't decay over the next year of feature additions.

## Out of scope

- Polymorphic command routing (sibling roadmap item)
- Pretty-printed text-mode output ergonomics
- Daemon socket protocol changes
- Schema migrations
- Adversarial-deployment new behavior beyond the existing `CQS_ULTRASECURITY=1` semantics

## What this is for, in one sentence

Walk every `Serialize` struct in the JSON-emitting paths, apply `skip_serializing_if` where the default value carries no information beyond the field's absence, thread an explicit `Posture` parameter through `build_chunk_json_inner`, test the contracts (not the byte counts), survey known consumers, ship.

---

## Appendix A: changes from the first cut (2026-05-08)

The first version of this doc was rewritten the same day after a red-team review caught seventeen issues. Notable changes preserved here so a future-me can see what was learned:

- **Premise honesty.** First cut implied this audit was a confirmed-bottleneck fix; the rewrite frames it as hygiene. The agent-grep observation was contextual; #1593 already covered the load-bearing piece.
- **No fake-precision byte gate.** First cut had a "30% byte reduction" acceptance test. The number was made up. Replaced with contract tests + descriptive measurement.
- **Posture as parameter, not env-read in leaf.** First cut would have read `CQS_ULTRASECURITY` inside `build_chunk_json_inner`. Rewrite threads a `Posture` enum.
- **Phase 3 trigger has explicit criteria.** First cut said "build only if Phase 1+2 leaves visible noise." That's not a trigger. Now: a 4-condition checklist that all must be true.
- **Inventory is non-exhaustive by design.** First cut listed structs as if the list were complete. Rewrite leads with a mandatory `find`/`grep` discovery step.
- **Consumer survey is now an acceptance criterion.** First cut's "no breaking change" claim was hopeful. Now: explicit list of known consumers (eval harness, daemon batch) that must be tested before merge.
- **Cost estimate honesty.** First cut said 2 days; rewrite says 3-4. Half is test writing.
- **Field-decision precedence is normative.** First cut's "rule of thumb" decomposed into three rules with no precedence. Rewrite has explicit precedence: posture-gate > skip-when-default > always-emit.
- **`--lean` flag's `_meta` handling.** First cut's "strip `_meta` wholesale" lost the worktree-stale signal. Rewrite recommends block-level skip-when-default.
- **Discipline follow-up filed.** Without an enforcement mechanism the audit decays. Follow-up tracked.

The pattern across all of these: **don't optimize the design for the prompt that produced it; optimize for what survives a hostile re-read six months later.**
