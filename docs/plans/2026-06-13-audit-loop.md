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

## Roles: orchestrator (agent) / governor (code) / workers (agents)

The loop is three roles, and conflating them — calling the whole thing "the governor" — is the trap. Pull them apart:

- **Orchestrator** (conductor / foreman) — the judgment-bearing director: reads coverage state → decides what to audit (judgment beyond the deterministic priority) → dispatches auditor subagents → reads findings → dispatches verifiers → **disposes** of each (auto-land hardening / dispatch a fix lane / file an issue / escalate to the human) → manages the fix pipeline → updates the ledger. The ledger is its *memory*. **Agent-shaped and capability-sensitive** — orchestration/triage/disposition is where model capability pays off, and it's Fable's documented lane (the model split: *fable orchestrates/reviews/audits, opus implements*). So the **expensive model lives HERE**, on the conductor — Fable when it returns from the export-order freeze, opus until then. The "strategist/lab" role is *subsumed*: the conductor's judgment **is** the strategy (escalate a hot region, declare the loop spinning, re-tune the governor's knobs).
- **Governor** (the leash) — the deterministic budget / WIP / value-density-backoff limits the orchestrator operates *within*. **NOT an agent, and NOT the orchestrator.** Governance is arithmetic + queue discipline where you want determinism: an LLM in the per-cycle budget path is the anti-pattern — it *burns budget to decide budget*, and an LLM's budget-discipline is unreliable. The governor is enforceable code (a budget file, a WIP counter) the conductor cannot blow past. The conductor is the dog; the governor is the leash.
- **Workers** — auditors (find), per-finding verifiers (real? — the false-positive filter), fix-lanes (implement). Dispatched by the orchestrator; model per role (opus implements; auditors per their own rules; security stays opus).

**Cost factoring (decide — Q13).** An expensive conductor in a *perpetual* hot loop is the cost commitment: you pay conductor-tier every cycle, forever. Two shapes: **(a) one expensive orchestrator** does dispatch+triage+disposition every cycle (simple, pricier); **(b) tiered** — a *cheap* per-cycle dispatcher (mostly mechanical once the governor's deterministic priority picks the cell) + the *expensive* conductor only on strategic ticks (triage a cluster, re-tune, escalate, dispose of the hard findings). (b) spends the money on judgment instead of dispatch plumbing; the governor bounds either.

**The real cost isn't the model tier — it's the human's attention.** Model tokens (even Fable's) are the rounding error; the most expensive orchestrator in the loop is the *user*, and right now they sit in the per-cycle conductor seat (approving staffings, gating each land). So the factory's first-order optimization is **minimize user-touchpoints per cycle, not model spend.** Apply the role split to the user too: spend them in the rare *strategist* seat (which auditors exist, the auto-land aggressiveness, "is this a real new null") and evict them from the *conductor* seat — the model becomes the conductor, the code the governor, the user the exception-handler who reads a digest. The eviction has a prerequisite: the user only leaves the hot loop once the conductor+governor are trustworthy enough to trust the auto-closed set — which is precisely what test-firing every auditor + the durable-guard discipline earns. Building the factory is the user paying attention now to buy their attention back later.

**This loop is already running, by hand.** The #1826 arc — dispatching the auditor trio, test-firing each new auditor, running the gate batteries, managing the fix-rounds, landing, tracking in tears — *is* the orchestrator role, performed manually by the autonomous `/loop`. audit-loop = specialize that loop, give it a coverage ledger + a budget leash, let the conductor self-task.

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
- **lab** (periodically probe for *new shapes* — the "is there a 7th null?" work; a completeness-critic asking "what region/shape is unaudited?") — **not a separate role; the orchestrator's strategic facet** (see Roles). A slow tick where the conductor evolves the auditor *set* rather than just running it. Bolt on after the factory works.
- **swarm** (uncoordinated parallel fan-out) — weakest; without the matrix's prioritization it spends tokens on redundant coverage. Skip.

## Mostly not new infra

Already have: the orchestrator-by-hand (the autonomous `/loop`), the workers (the family), the target-selector (`/idle`), residuals-to-issues, the gate-battery landing. **Net-new:** the coverage ledger, the invalidation mapping (diff → cells; partly encoded in the magnet-area habit), the auto-land-hardening path + the confidence gate (Q1), the **governor** (deterministic leash), and formalizing the **conductor's disposition judgment** (the expensive-model role) into a self-tasking loop. A target-selection + ratchet layer, not a rewrite. Likely shape: a `/audit-loop` skill, or a mode of `/idle`.

## Open questions (decide before building)

1. **Auto-land aggressiveness / the confidence gate** (shapes everything): "managing fixes" turns this from a find→guard ratchet into a find→**fix**→land factory, which crosses the human-gate line. The risk isn't uniform — a *hardening guard* (a test-add, no behavior change) is safe to auto-land; auto-fixing-and-landing a *real bug* unseen is the aggressive end. Proposed gate: the conductor may close a finding itself ONLY if (a) a verifier confirms it, (b) the fix passes the full magnet-area battery, (c) it's below a blast-radius threshold; everything else → a digest for the human. The conductor's core judgment is *which findings it's allowed to close on its own*. Default: auto-land hardening guards; bug-fixes gated by (a)+(b)+(c).
2. **Ledger location**: a committed file (`docs/audit-coverage.toml`) — explicit but can drift from reality; vs derived from the guards present + git blame — self-truthing but harder to query; vs a SQLite table. Which?
3. **Region granularity**: file / module / subsystem / function? Finer = precise invalidation + more overhead; coarser = cheap + blunt.
4. **Invalidation mapping ownership**: is diff→cells the magnet-area path heuristic, or per-shape "what change invalidates me" rules? Who maintains it as the code evolves?
5. **Governor knobs**: token budget per cycle? dispatches per cycle? value-density backoff threshold K? How are they set / tuned?
6. **Concurrency**: how many auditors in parallel per cycle (GPU + cost contention — the swarm risk)?
7. **Driver + the orchestrator/governor split**: the orchestrator is an agent (the autonomous `/loop` specialized, or a dedicated conductor) running *within* the deterministic governor (code) — the per-cycle budget/WIP enforcement is never an LLM. Driver options for the agent: the autonomous `/loop`, a cron, or a daemon; only-when-idle / continuously / on-merge-trigger?
8. **Human touchpoint — largely answered: gh issues ARE the digest (free as in beer).** Don't build a digest; the conductor files above-gate findings as GitHub issues and your issue triage *is* reading the digest. Why it's the answer, not just an option: (a) the auto-closed set produces merged PRs with no issue, so the open-issues list = exactly what needed a human; (b) it closes the loop with one object — the conductor *files*, `/idle` *consumes*, a PR *closes* (the factory's outbox = your inbox = `/idle`'s work queue, all the same issue); (c) issue-backlog-growth is a free over-production signal feeding the governor; (d) persistent + searchable, and dedup is a free `gh issue list --search` before filing. **Discipline it inherits:** curated, not raw — above-gate findings only, batched into umbrella issues (the residuals-to-issues over-filing rule), labeled by severity/shape/region; an issue-per-raw-finding turns the false-positive tax into issue-spam. **Keep separate from the ledger (Q2):** issues are the *exception queue* (red cells that escalated); the coverage matrix is the *state* (which cells are green) and wants its own store — labels can tag, but the matrix isn't an issue list. Residual sub-question: what ALWAYS files regardless of the gate (scoring / security / schema-touching findings reach the human even at high confidence).
9. **Lab component**: in v1 or deferred? How/when does it probe for new shapes vs run existing?
10. **Relationship to `/audit` (16-category) and `/idle`**: subsume, complement, or a mode of `/idle`?
11. **Pause/scope control**: how does the user pause/resume or scope it ("only `src/store` this week")?
12. **Success metric** — denominated in the scarcest resource, the user's attention: **fraction of cycles requiring a human → 0**, user-touchpoints per real bug caught. (Secondary: matrix % green, guards added/week, bugs caught pre-merge vs post-, false-positive rate.)
13. **Cost factoring** (see Roles): the first-order cost is *user-touchpoints*, not model tier — optimize for evicting the user from the per-cycle seat. Second-order: one expensive orchestrator per cycle (simple, pricier in tokens) vs tiered cheap-dispatcher + expensive-conductor-only-on-strategic-ticks. Decides where both the expensive model AND the human actually run.

## Prerequisite already in place

The six-auditor family + the durable-guard-per-auditor discipline + the grant/withhold (per-finding verifiers) + residuals-to-issues + the magnet-area gate-battery habit. The factory is the automation layer over all of it.

## Decision status — proposed defaults (2026-06-13 discussion; STILL PARKED, leans not final)

The discussion resolved five questions and proposed defaults for the rest. Decide (or override) these before building.

**Resolved:**
- **Q8 (digest)** → curated **gh issues** (above-gate only, umbrella-batched, labeled by severity/shape/region); your triage *is* the digest; `/idle` closes the loop. Issues = the exception queue, NOT the ledger.
- **Q12 (metric)** → denominated in the scarce resource (your attention): **fraction of cycles needing a human → 0**, user-touchpoints per real bug. (Matrix-green / guards-per-week / FP-rate are secondary.)
- **Roles (was tangled in Q7)** → **orchestrator** (agent, the expensive seat — Fable's lane, opus until it returns) / **governor** (deterministic code, the budget/WIP leash) / **workers**. Never an LLM in the per-cycle budget path.
- **Q1 gate *structure*** → the conductor may auto-close a finding only if (a) a verifier confirms it, (b) it passes the full magnet-area battery, (c) it's below a blast-radius threshold; else → an issue.
- **Q13 *principle*** → optimize user-touchpoints first, model tier second.

**Remaining — with proposed defaults:**
- **Q2 ledger — GATES A BUILD:** derive the matrix from guards-present + git-blame (self-truthing, no drift), cache in SQLite for query speed. *Cost: a guard-tagging convention — a test names the (region, shape) it covers.* That convention is the real decision.
- **Q1-residual — GATES A BUILD: v1 aggressiveness** → **hardening-guards-only + file all bugs as issues** in v1 (earn the trust that lets you leave the hot loop); enable gated auto-fix-and-land in v2.
- **Q3 granularity** → invalidate at **file**, display/track at **module** (two granularities).
- **Q4 invalidation** → **magnet-area heuristic** first; the lab tick proposes updates; promote to per-shape rules only where it mis-fires.
- **Q6 concurrency** → **low (1–3)**; the governor caps it (swarm-risk knob).
- **Q7-residual (driver)** → the existing autonomous **`/loop`**, on-merge-trigger + idle-filler; no new daemon.
- **Q9 lab** → **defer** to post-v1.
- **Q10 integration** → a **mode of `/idle`**; complements `/audit` (does not subsume the one-shot 16-category sweep).
- **Tuning (safe initial values, re-tune from the metric):** Q5 governor knobs (small budget, backoff K=2–3), Q11 pause/scope (a config + an `audit-loop:paused` label the conductor respects), Q13-residual tiering (start with one orchestrator = the `/loop`; tier to cheap-dispatcher + expensive-conductor only if cost demands).

**The two that actually gate a build:** Q2 (derived-vs-stored ledger + the guard-tagging convention) and Q1-residual (v1 aggressiveness). The rest have safe defaults.
