# audit-loop — a perpetual auditor factory (DESIGN SKETCH, for review)

**Status: PARKED / draft.** Not scheduled. Captures the design discussion of 2026-06-13. Decide the open questions before implementing.

## The idea

A loop that perpetually tasks the auditor family (seam / property / interleaving / sweep / legacy-state / adequacy / red-team) so coverage is continuously extended and re-verified — a "factory" rather than a one-shot `/audit`.

## The reframe that makes it viable (read this first)

The naive version — "perpetually hunt bugs" — **fails**: bug-hunting is a diminishing function. You find the easy ones, then the medium ones, then you re-audit clean code producing nothing but token burn. A swarm that gets quieter and more expensive.

**The product is durable guards, not bugs found.** Every auditor in the family was built to emit a *guard* (a test that bites), not just a find — that's why each was test-fired before shipping. So the loop's job is **"convert un-guarded regions into guarded regions, monotonically."** That's a ratchet: finite-but-growing (grows with the code), never regresses, has a completion gradient. The test-fire-every-auditor discipline (the #1826 arc) was the prerequisite for this, whether or not it was framed that way at the time.

## Backbone: the (region × shape) coverage matrix

- **Rows** = code regions (granularity TBD — see Q3).
- **Columns** = auditor shapes (seam, property, interleaving, sweep, legacy-state, adequacy, red-team).
- **Cell** = "is region R guarded against shape S, at what code version?"

The factory keeps the matrix green. Requires **persisted state** — a coverage ledger (see Q2) — without it the loop can't prioritize or avoid redundancy.

## Engine: invalidation, not perpetual re-sweep

The high-value fuel is **change**. A merged diff re-opens the cells it touches, using the magnet-area mapping we already apply by hand (overlay change → seam+interleaving; codec/round-trip → property; migration/format → legacy-state; new-variant/dual-surface → sweep; security-surface → red-team; logic-dense-with-thorough-looking-tests → adequacy). The #1900 gate battery was a manual instance.

So "perpetual" really means: **always keep just-changed regions re-audited (reactive, where the bugs are); when idle, extend coverage to never-audited cells (background).** This is `/idle` generalized from "pick an issue" to "pick the next under-covered high-value cell."

## Pipeline per cycle

1. Read the ledger + the latest merges.
2. Pick the highest-value cell within budget: re-opened-by-change > high-risk-uncovered (cqs health hotspots: scoring/index/daemon) > never-audited.
3. Dispatch that shape's auditor on that region.
4. Verify the finding (per-finding adversarial verifier — the false-positive filter; this is why red-team/adequacy got the Agent tool and the relational five didn't).
5. Route the output:
   - **green-hardening guard** (boundary held, no bug) → low-risk test-add → **auto-land** (see Q1).
   - **bug-finding** (real defect) → fix lane + review + **human-gated** queue / issue.
6. Update the ledger.

## The hard problems (these kill naive versions)

- **No "done" → governors required.** A token budget per cycle *and* a value-density backoff (region audited clean K times → deprioritize until it changes). Every loop needs a stop predicate; a perpetual one needs a throttle.
- **Landing is the bottleneck, not finding.** Auditors produce faster than fixes land → unbounded `audit-findings.md` WIP. Fix = the auto-land/human-gate asymmetry above: continuously auto-ratchet coverage, queue the rarer real bugs. Production throttled to land capacity.
- **False-positive tax.** Equivalent mutants, intended exceptions, refuted seams. Without the verify stage the backlog floods with noise.

## factory vs lab vs swarm

- **factory** (ratchet guard coverage, balanced to landing capacity) — the valuable core. Build this.
- **lab** (periodically probe for *new shapes* — the "is there a 7th null?" work; a completeness-critic asking "what region/shape is unaudited?") — a slow background tick that evolves the auditor *set*. Bolt on after the factory works.
- **swarm** (uncoordinated parallel fan-out) — weakest; without the matrix's prioritization it spends tokens on redundant coverage. Skip.

## Mostly not new infra

Already have: the driver (the autonomous `/loop`), the workers (the family), the target-selector (`/idle`), residuals-to-issues, the gate-battery landing. **Net-new:** the coverage ledger, the invalidation mapping (diff → cells; partly encoded in the magnet-area habit), the auto-land-hardening path, the governors. A target-selection + ratchet layer, not a rewrite. Likely shape: a `/audit-loop` skill, or a mode of `/idle`.

## Open questions (decide before building)

1. **Auto-land aggressiveness** (shapes everything): green-hardening-guards-only (safe — a hardening guard is a pure test-add) vs *also* auto-land trivially-confirmed fixes (faster, riskier)? Default: hardening-only.
2. **Ledger location**: a committed file (`docs/audit-coverage.toml`) — explicit but can drift from reality; vs derived from the guards present + git blame — self-truthing but harder to query; vs a SQLite table. Which?
3. **Region granularity**: file / module / subsystem / function? Finer = precise invalidation + more overhead; coarser = cheap + blunt.
4. **Invalidation mapping ownership**: is diff→cells the magnet-area path heuristic, or per-shape "what change invalidates me" rules? Who maintains it as the code evolves?
5. **Governor knobs**: token budget per cycle? dispatches per cycle? value-density backoff threshold K? How are they set / tuned?
6. **Concurrency**: how many auditors in parallel per cycle (GPU + cost contention — the swarm risk)?
7. **Driver**: the existing autonomous `/loop`, a cron, or a new daemon? Runs only-when-idle / continuously / on-merge-trigger?
8. **Human touchpoint**: the loop auto-triages (verifier) + auto-lands hardening; bug-findings go to — an issue? a digest for the user? a fix lane it dispatches itself?
9. **Lab component**: in v1 or deferred? How/when does it probe for new shapes vs run existing?
10. **Relationship to `/audit` (16-category) and `/idle`**: subsume, complement, or a mode of `/idle`?
11. **Pause/scope control**: how does the user pause/resume or scope it ("only `src/store` this week")?
12. **Success metric**: matrix % green? guards added/week? bugs caught pre-merge vs post-? false-positive rate?

## Prerequisite already in place

The six-auditor family + the durable-guard-per-auditor discipline + the grant/withhold (per-finding verifiers) + residuals-to-issues + the magnet-area gate-battery habit. The factory is the automation layer over all of it.
