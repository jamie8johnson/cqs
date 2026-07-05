# Eval command-core lift (#2217) — decisions (post-attack, v2)

Status: ATTACKED AND REVISED. Investigator brief (2026-07-05) + two-adversary attack (red-team: 3 requirements, 1 holds; seam: D5 SOUND, D1–D4 + parity REVISE). The adversaries converge — seam's single-flight IS red-team's RT-DOS-1 cap — so this v2 is the consensus design. v1's structural error, in one sentence (seam's root cause): **the draft adopted suggest's routing template without its hidden precondition — suggest's output is substrate-invariant; eval is the first runtime-routed command whose NUMBERS depend on which surface computes them, so every relay behavior benign for suggest (silent inline fallback, flag-based routing, adapter-skip on forward) becomes a lie for eval.**

Ground truth (independently verified): the per-embed serialization point is `Embedder.session: Mutex<Option<Session>>` (src/embedder/core.rs) — per-embed fair, no wholesale blocking; relay timeout = single base knob `CQS_DAEMON_TIMEOUT_MS` × class multiplier (src/daemon_translate.rs:406-455); a daemon-forwarded command NEVER enters its CLI adapter (src/cli/dispatch.rs:28-35); read-expiry and busy-reject both silently rerun inline via `DaemonOutcome::Fallback` (dispatch.rs:857-876, 1012-1023); `daemon_json_args_request` (daemon_translate.rs:946) is the in-adapter dial primitive.

## The structural decision: adapter-owned relay, not generic routing

`cqs eval` stays `batch = "cli"` in definitions.rs — the generic auto-forward machinery never touches it. Instead `cmd_eval` (the CLI adapter) OWNS the relay:

1. Parse/validate flags; run the freshness gate exactly as today (`wait_for_fresh` socket dial — same semantics, same env opt-outs, on every invocation; D4-v1's "gate inverts daemon-side" is dead for the CLI surface).
2. Explicitly try the daemon path via `daemon_json_args_request(cqs_dir, "eval", wire_args)` with the eval budget (below).
3. Outcome handling is HONEST, never silent:
   - Daemon answered → use its report (`_meta.index_substrate: "resident"`).
   - Daemon absent, or busy-rejected BEFORE booking → print an explicit one-line notice ("daemon unavailable — running CLI-side on a fresh-built index") and run the local cold path (`substrate: "fresh-built"`). An explicit substrate switch is acceptable; a silent one is the lie.
   - Daemon BOOKED the job but the response was lost (read expiry past the eval budget) → HARD ERROR telling the operator the daemon is still running the eval; do NOT rerun inline. The double-GPU/second-ORT-session rerun is the trap; `daemon_booked` machinery (dispatch.rs:857-876) is the discriminator.
4. `--save`/`--baseline`/`--tolerance` are pure post-processing of whichever report returned — they never influence routing (v1-D3's routing dependence would have made "the flag that saves a baseline change the number saved": resident scores bare, fresh-built scores with `--save` → phantom cross-substrate regressions).
5. Exit codes stay adapter-side; the core returns data and never exits.

`dispatch_eval` (the daemon handler) exists and is registered, but its consumer is the MCP surface + the adapter's explicit dial — not generic CLI routing.

## D1 (revised) — no GPU gate; single-flight enforces the fairness premise

Per-embed fairness holds for ONE eval (live query waits ≤1 embed, tens of ms). "One" is enforced by nothing in v1 — fallback reruns and MCP client retries manufacture N concurrent evals, including inline ones in a SECOND ORT session outside the resident-session mutex the fairness argument relies on. Fixes: **daemon-side eval single-flight** (= RT-DOS-1: at most one in-flight eval in `dispatch_eval`; a second submission gets a typed, retryable busy — before any embedder work), and the no-inline-rerun rule above (kills the second-session path). No idle-gate, no dedicated slot — the v1 rationale stands, now with its premise enforced.

## D2 (revised) — eval budget with a floor independent of the base knob

`EVAL_COMMAND_TIMEOUT_MULTIPLIER = 40` (40 × 30s = 20 min default) **with a hard floor** `EVAL_TIMEOUT_FLOOR = 20 min`: budget = `max(40 × base, floor)`. Rationale: the 40× rides the shared base knob, so an operator setting `CQS_DAEMON_TIMEOUT_MS=5000` for snappy searches would silently shrink eval to 200s and guarantee expiry on large fixtures; the floor decouples them. Still no new env var. One-shot request/response; Tasks/progress/cancel remain 3b (a disconnected MCP client does not abort the server-side run — documented; bounded by single-flight).

## D3 (revised) — wire surface

Core `EvalArgs` (Deserialize + JsonSchema): `query_file`, `limit`, `category`, `reranker`. save/baseline/tolerance/freshness are CLI-adapter concerns and simply do not exist on the wire struct (post-processing, not routing — see structural decision). Smuggle-direction verified safe by seam (cores drop off-schema fields; json-args pre-force pattern; dispatch bail precedent) — keep the dispatch-side bail + pinned guard test anyway (RT-LEAK-2's second half).

## D4 (revised) — freshness

- CLI surface: today's `wait_for_fresh` dial, verbatim, before the relay. All flags/env keep working on every invocation.
- MCP surface: `dispatch_eval` runs immediately (no parked wait — a 600s park would sit on the single-flight slot) and the output carries freshness metadata from `view.watch_snapshot()` (fresh/stale/unknown + last-synced) alongside the substrate marker. The MCP consumer decides; the metadata makes it honest. `WatchSnapshot::unknown()` (batch-stdin surface) is representable, not a brick.

## D5 (unchanged, + one line) — fixture path trust

Wire path: read_core-style guard (canonicalize + starts_with served root + uniform opaque error), and per RT-LEAK-2 the guard OWNS the read — the core receives already-read bytes, so `run_eval`'s path-echoing `.with_context()` errors (runner.rs:102-105) never reach the wire; an in-root non-JSON file produces the same opaque error as traversal (no in-root content oracle). CLI adapter: canonicalize pre-relay; an out-of-root fixture (operator authority) routes local instead of failing opaquely over the wire.

## Red-team requirements (all land in `dispatch_eval` / wire output)

- **RT-DOS-1** = the single-flight cap (D1). Required for untrusted MCP-HTTP (SECURITY.md:24/:43); the 20-min hold is the amplification the seconds-scale count-caps don't cover.
- **RT-LEAK-2**: opaque wire errors for fixture read/parse (guard owns the read); dispatch bail on excluded flags + pinned test.
- **RT-LEAK-3**: `EvalOutput.query_file` relativized to the served root (or omitted) on the wire — the success path currently echoes the absolute host path (runner.rs:258).
- **RT-DOS-4 HOLDS**: `--reranker onnx` is a one-time idempotent OnceCell load the daemon can already incur; doc note only.

## Parity + substrate honesty

- `dispatch == serialize(core)` parity on the empty-fixture case (embedder-free, exit codes are adapter-only so the core returns data) — REQUIRES the core's resource bundle to be lazy/injectable; if `eval_core` demanded a constructed `&Embedder` up front, shape-parity is unwritable in unit scope. The lazy-bundle is a build requirement, not a nicety.
- json-args == argv parity per the house pattern; wire-cannot-save guard; traversal-rejection test; MCP name-map pin.
- **`_meta.index_substrate: "resident" | "fresh-built"` on every eval output, all surfaces.** Score-parity across substrates is explicitly NOT a contract (HNSW reconstruction nondeterminism); the marker is what makes the recall-gate discipline coherent post-lift.
- `.claude/skills/recall-gate/SKILL.md` updated in the same PR: gate runs pin their substrate (either `CQS_NO_DAEMON=1` for fresh-built-always, or assert the marker is uniform across compared runs). Bare `cqs eval` lines would otherwise measure different substrates depending on daemon liveness.

## Pre-existing residuals surfaced by the attack (NOT this lane — filed as an issue)

Busy-reject → `FreshnessWait::Transport` mislabels a busy daemon as hung/crashed (daemon_translate.rs:1515-1517); MCP json-args path drops the retryable marker (busy → INVALID_PARAMS "rejected the request"). Both pre-date this lift.

## Build ladder (one lane, one PR)

1. Core extraction: `eval_core` + `EvalArgs`/`EvalOutput` (+ substrate field), lazy resource bundle, behavior-preserving for the CLI surface (existing eval tests pass unchanged).
2. Adapter-owned relay in `cmd_eval` (gate → dial → honest outcomes → post-processing), eval budget with floor.
3. `dispatch_eval` + single-flight + D5/RT guards; BatchCmd::Eval + macro row (registered for the adapter dial + MCP; `batch` stays `"cli"`).
4. MCP rows (tool_table, core-map, `typed(EvalOutput)`), doc tool-count sync, recall-gate skill update, docs/skills/CHANGELOG per checklist.

`part of #2217`; close after merge + verification.
