# Design: Result Trust — Edge Provenance, Dead Verdicts, Worktree Overlay, Ranking Provenance

Status: §1 (edge provenance) + §2 (dead verdicts) IMPLEMENTED (schema v30,
PARSER_VERSION 6). §3 (worktree overlay), §4 (ranking provenance), §5 (docs)
remain PROPOSED.
Origin: 2026-06-12 session. The motivating gaps were hit live during the
v1.43.0 campaign by our own agents. A candidate feature
(`cqs review --base`) turned out to already exist — see §5.

## Thesis: one program, not separate features

cqs's consumers are agents, and an agent's binding constraint on a retrieved
result is not relevance — it's *calibration*: how much should I believe this
before acting on it? cqs has been accumulating trust metadata for several
releases without naming the program:

| Signal | Trust question it answers | Shipped |
|---|---|---|
| `trust_level` / `injection_flags` | should I trust this **content**? | v1.30.1+, three-tier #1221 |
| `_meta.stale_origins` warnings | should I trust this **freshness**? | #1752, both surfaces |
| audit-mode | should I trust my own **priors** (notes)? | v1.23.x era |
| CLI==daemon parity tests | should I trust the **surface** I queried? | command-core campaign |

This doc extends the family to the remaining axes:

- **§1 Edge provenance** — should I trust this **edge**? (syntactic ground
  truth vs attribute grammar vs token-tree heuristic)
- **§2 Dead verdicts** — should I trust this **absence**? Dead-code is an
  absence claim, the hardest kind to calibrate: "no callers found" conflates
  "none exist" with "none visible to the walker".
- **§3 Worktree overlay** — should I trust that this answer is about **my
  reality**? A result can be perfectly correct for the wrong corpus, and is
  then indistinguishable from a lie.
- **§4 Ranking provenance** — should I trust **why** this ranked? A hit that
  matched the agent's literal string and one that matched its concept
  warrant different follow-up.

**The success metric is already written down, inconveniently.** The agent
defs instruct: "treat cqs results as hints; read the actual files before
acting." Every feature in this family should delete a clause of that
sentence. When a lane can act on `cqs impact` without the defensive re-read,
the program is done — and the savings compound, because the re-read tax is
paid on every lane, every session. (Scoreboard issue: #1821.)

**Calibration cuts both ways — the cry-wolf lesson.** This repo has already
shipped the opposite failure: the security-posture era attached cautionary
metadata (`handling_advice` on every envelope, the Posture matrix,
ULTRASECURITY) to *every* response, and consuming agents grew
correspondingly over-cautious — a warning that fires on every result
carries zero bits and just taxes every query to insure against rare events.
It got worse, not better, as models improved: stronger instruction-following
means boilerplate caution gets *obeyed* rather than habituated away. The
walk-back (the V2Bare SNR split, Posture/ULTRASECURITY deletion #1703,
skip-when-default as the chunk-JSON convention) is why every signal in this
doc is skip-when-default: trust metadata must be discriminative or it is
noise wearing a safety costume. The rule for any new signal: if it would
appear on the majority of results, it is a default, not a signal — invert
it or drop it.

---

## 1. Edge provenance on the call graph

### Problem

`function_calls` rows are kind-blind:

```sql
CREATE TABLE IF NOT EXISTS function_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file TEXT NOT NULL,
    caller_name TEXT NOT NULL,
    caller_line INTEGER NOT NULL,
    callee_name TEXT NOT NULL,
    call_line INTEGER NOT NULL
);
```

We now emit three edge sources with very different evidentiary weight:
syntactic `call_expression` (ground truth), serde string-callback attributes
(#1808 — high confidence, attribute grammar is explicit), and macro
token-tree heuristics (#1819 — `ident(`-shape matching inside opaque token
streams). `callers`/`impact`/`test-map` flatten all three into
indistinguishable rows, so a consuming agent cannot weight "directly called
at serve/mod.rs:162" differently from "heuristically matched inside a
macro". Every future edge type (fn-pointer arguments — the open half of
#1818 — and dyn-dispatch resolution, #1573 tier 3a) makes the flattening
worse.

Precedent in our own schema: `type_edges.edge_kind TEXT NOT NULL DEFAULT ''`
already classifies type edges (Param, Return, Field, Impl, Bound, Alias).
Call edges should get the same treatment.

### Design

- **Schema (v30):** `ALTER TABLE function_calls ADD COLUMN edge_kind TEXT
  NOT NULL DEFAULT 'call'`. Additive; existing rows default to `'call'`,
  which is correct for every pre-v30 row except serde/macro/fn-pointer/
  doc-reference edges — those re-extract on the next reindex (PARSER_VERSION
  bumped 5→6 with this work; the staleness pre-filters treat that version
  drift as stale, so a plain `cqs index` re-parses and re-tags drifted files —
  no `--force`, no backfill logic). Values: `call`, `serde_callback`,
  `macro_heuristic`, `fn_pointer`, `doc_reference`.
- **Extractor:** `CallSite` gains a `kind` field; the three emit sites tag at
  the source. The serde and macro passes already exist as distinct functions
  (`filter_serde_callbacks`'s emitting sibling, `extract_macro_call_edges`),
  so tagging is one field per pass, no restructuring.
- **Surfaces:** `callers`/`callees`/`impact`/`test-map` outputs gain a
  skip-when-default `edge_kind` per entry (absent ⇒ `call`), following the
  chunk-JSON skip-when-default convention (trust_level precedent). Parity
  tests extend mechanically. Breaking-JSON note: additive key only; no
  external users regardless.
- **Filtering:** `--edge-kind <k>` on `callers`/`callees` is cheap once the
  column exists (agent-facing knob; knob-count is not a concern for agent
  consumers).

### Gate criteria

- Parity tests pin `edge_kind` identical CLI==daemon on a fixture containing
  all three kinds.
- A migration test pins fresh-create == migrated shape (v29→v30), and the
  contiguity test extends.
- `cqs callers auth_banner_tty --json` shows `edge_kind: "macro_heuristic"`
  on the live index after reindex.
- Full `--tests` sweep (schema change = global state; v29's two CI round
  trips are the cautionary precedent — integration fixtures assert
  schema_version).

### Non-goals

No confidence *scores* — kind is the confidence signal; a float would be
false precision on heuristic edges.

---

## 2. `cqs dead` verdicts (self-classifying output)

### Problem

The 2026-06-12 confident-dead sweep returned 44 items. Manual triage took
them to: ~30 test-only fixtures (`make_*`/`mock_*` builders, `#[test]` fns
in `#[cfg(test)]` modules), 4 JS functions behind a known js-edge gap, a
handful of macro-only callees (now fixed by #1819), and a small genuinely-
dead remainder. That triage was mechanical — every input it used is already
in the store — yet it cost an orchestrator pass over 44 names. The tool
should ship the verdict, not the homework.

### Design

`DeadOutput` entries gain `verdict` + `verdict_reason` (skip-when-default;
absent ⇒ `unclassified`, the current behavior). Classification runs in
`dead_core`, ordered (first match wins):

1. `test-only` — chunk's origin under `tests/`, or enclosing module chain
   contains `#[cfg(test)]` (the chunk row already records the module path /
   the content carries the attribute — use whichever the store can answer
   without a parse; if neither, this tier degrades to origin-prefix only and
   the limitation is documented in the output).
2. `low-confidence-live` — callers exist but **all** edges are heuristic
   kinds (`macro_heuristic`, later `fn_pointer`). Depends on §1; without it
   this tier cannot exist, which is the argument for shipping them together.
3. `known-gap` — language/extension in a static gap table (currently: `.js`
   served assets — event handlers wired from HTML; Python dunder protocol
   methods like `__aenter__` invoked by the runtime). Table lives next to
   the dead-code filters with one comment per row stating the gap.
4. `dead` — none of the above; the actionable residue.

Text surface groups by verdict; JSON carries it per-entry. `--verdict <v>`
filters (so `cqs dead --verdict dead` is the actionable list, directly
consumable by an /idle-style loop).

### Gate criteria

- Fixture store with one chunk per verdict class; parity test pins CLI==daemon.
- On the live index: `cqs dead --verdict dead --json | length` ≤ 10 (today's
  manual triage says the genuine residue is single-digit), and zero
  `test-only` items appear under `--verdict dead`.

### Non-goals

No auto-deletion, no `--fix`. The verdict is evidence; the delete decision
stays with the consumer (no-external-users does not mean no-wrong-deletes).

---

## 3. Worktree search overlay (the deep one)

### Problem

Lane agents work in `.claude/worktrees/<agent>` checkouts. Reads resolve to
the **parent** index (deliberate, #1254), so every `cqs` search/impact/
callers result reflects main's branch state, not the lane's own edits.
Writes are now guarded (#1814), but reads remain subtly wrong: an agent that
just renamed a function still sees the old name in search; impact on a
function it modified reports the parent's call sites. Today agents
compensate by re-reading files at relative paths — documented in the agent
defs as "treat cqs results as hints", which is a standing tax on every lane.

Full per-worktree indexes are the wrong fix: model-loading cost per lane,
index churn for short-lived branches, and the #1809 write-guard would need
per-worktree carve-outs.

### Design — ephemeral query-time overlay

Reuse the multi-store merge seam that `--ref` search already built (#1793):
`SearchCtx::references()` returns `Arc<ReferenceIndex>` values whose results
merge via `reference::merge_results`. The overlay is architecturally a
reference index whose corpus is *the worktree's dirty delta*.

- **Delta discovery:** in a worktree whose discovery crossed to the parent
  root (the exact predicate #1814 built — `parent_index_boundary_crossed`),
  compute `git diff --name-status <merge-base>` against the parent's HEAD
  plus uncommitted changes. Typical lane delta: < 20 files.
- **Overlay build:** parse those files with the normal pipeline into an
  in-memory store (SQLite `:memory:` — the Store API already abstracts the
  pool; schema-create on open). Embed with the session embedder; with the
  daemon serving, embedding ~20 files is the same cost as one watch tick.
  Cache the overlay keyed by `(worktree_root, dirty-state fingerprint)` in
  the daemon's BatchContext cells (invalidation epoch machinery from #1739
  applies unchanged) so repeat queries within a lane session pay once.
- **Query path:** `prepare_query` runs once (the #1805 `ProjectSurface`
  split means the overlay can reuse the prepared query the way ref fan-out
  does). Results merge: overlay hits **shadow** parent hits for the same
  `(origin, name)` — a file changed in the worktree must never surface its
  parent version. Deleted-in-worktree origins are masked from parent
  results. Then standard `merge_results` ranking.
- **Graph commands:** phase 2. Search-only first; `callers`/`impact` overlay
  requires merging call-graph tables, which has harder shadowing semantics
  (a caller deleted in the worktree must subtract from parent counts).
  Search-only is already the 80% win — lanes scout far more than they
  impact-check.
- **Opt-in first:** `--overlay` flag + `CQS_WORKTREE_OVERLAY=1`, flipped to
  default-on for worktree CWDs after a soak period in our own lanes. The
  detection predicate exists; the flag is the safety margin.

### Gate criteria

- A worktree fixture: rename a function in the worktree → `cqs "old name"`
  does not return the worktree file's parent version; `cqs "new name"
  --name-only` finds it. Deleted file masked. Unchanged-file results
  byte-identical to non-overlay search.
- Overlay build cost measured and printed at debug level; repeat-query cache
  hit pinned by test.
- Recall gate unaffected (eval runs from the main checkout, no worktree —
  pin with one explicit no-worktree-no-overlay test).

### Risks

- **Embedder availability in CLI-direct mode:** overlay embedding needs the
  model; cold CLI pays the load. Mitigation: daemon-served overlay only at
  first (CLI-direct prints "overlay skipped, daemon not running" warn) —
  honest degradation, matches existing daemon-optional patterns.
- **Shadowing correctness is the whole feature.** A wrong merge silently
  feeds agents stale code with fresh confidence — worse than today's known
  bias. The shadow tests above are the gate; reviewer should attack the
  `(origin, name)` shadow key (renames produce origin changes — a renamed
  file shadows by old origin via the deletion mask, new origin via overlay
  presence).

---

## 4. Ranking provenance (per-result "why did this rank")

### Problem

A search hit carries a score and nothing about its origin story. An agent
cannot distinguish:

- ranked by the **dense leg** (semantic neighbor — "matched my concept"),
- ranked by the **sparse/FTS leg** (lexical overlap — "matched my literal
  string"),
- lifted by a **name-match boost** (identifier equality — strongest signal
  for definition-lookup, noise for conceptual queries),
- lifted by a **note boost** (a prior opinion, not evidence — exactly what
  audit-mode exists to suppress),
- or admitted by **centroid routing** choosing a category-specific α.

These warrant different follow-up. A concept-match justifies reading the
chunk; a string-match on a conceptual query is a known false-friend; a
note-boosted rank should trigger exactly the skepticism audit-mode encodes.
Today the agent pays the calibration cost by reading everything — the
re-read tax again. This is distinct from the queued `cqs trace` roadmap
item (a debugging command that explains one query end-to-end); ranking
provenance is lightweight per-result metadata on **every** search response.

### Design

The implementation hook already exists: the #1719 ScoreSignal refactor made
the scoring pipeline a fold over a signal slice — one place where every
signal fires. Provenance is "record which signals contributed non-trivially
per result" at fusion time:

- Each result gains a skip-when-empty `rank_signals` array of compact
  entries: `{signal, value}` where signal ∈ `dense`, `sparse`, `fts`,
  `name_match`, `note_boost`, `type_boost`, … (the SCORING_KNOBS /
  ScoreSignal names are the vocabulary — no new taxonomy). `value` is the
  signal's contribution in its native unit (rank for RRF legs, multiplier
  for boosts). RRF leg ranks are known at fusion (`rrf_fuse` consumes
  per-leg rank lists); boost signals already flow through the ScoreSignal
  fold — the recording is a side-channel write, not a scoring change.
- **Bit-identical scores are the gate**: provenance recording must not
  perturb ranking. Pin with the #1719-style exact-equality test (the
  pre/post refactor pattern this codebase has used twice).
- Surfaces: search/scout/gather/task per-chunk JSON (the chunk-JSON
  convention: skip-when-default, agents opt into reading it). Text surface
  omits it entirely — provenance is for machine consumers.
- `note_boost` provenance doubles as an audit-mode complement: an agent can
  see a note influenced ranking *without* turning notes off — softer than
  audit-mode's blanket exclusion, usable per-query.
- Cost: a few small allocations per result at fusion; no extra store reads.
  Token cost on JSON output is the real price — skip-when-default keeps
  unboosted dense-only results at zero overhead, and `--no-rank-signals`
  (or a tokens-budget interaction) caps it for tight-budget calls.

### Gate criteria

- Exact-equality pin: scores and order bit-identical with recording on/off.
- A fixture query where a known note boost fires → `rank_signals` carries
  `note_boost`; same query under audit-mode → no note signal (and the pin
  asserts audit-mode behavior unchanged).
- Parity test: CLI==daemon `rank_signals` identical.
- Recall gate trivially unaffected (no scoring change by construction, but
  run it anyway — the cheap insurance rule).

### Non-goals

Not a replacement for `cqs trace` (full per-query routing narrative —
classifier decision, α resolution, candidate pool sizes — stays a separate
debugging command). No natural-language explanations; signal names + values
only.

---

## 5. Correction: `cqs review --base` already exists

The campaign's fable reviewer worked around "review only diffs uncommitted
changes" with raw git plumbing — but `cqs review --base <ref>` shipped long
ago (clap surface verified live, with ref-validation hardening in
`run_git_diff`). The gap is documentation: the code-reviewer agent def and
/check-my-work skill don't mention `--base`, so agents reviewing committed
branches don't reach for it. Fix is two doc lines, fold into the next
agent-def touch. No code change.

---

## Sequencing

§1 and §2 ship together (one PR: schema v30 + extractor tags + dead
verdicts) — §2's `low-confidence-live` tier is the immediate consumer of
§1's column, and one migration beats two. §4 is independent of the others
and gated only by the bit-identical pin — it can ship in parallel with
§1/§2 (the ScoreSignal seam makes it a contained change). §5 rides any docs
PR. §3 gets its own implementation plan after the rest land (the overlay's
shadow semantics deserve a fable review pass at design time, not just at PR
time), and lands search-only behind the flag first.

Family-wide acceptance: after all four, revise the agent defs' "treat
results as hints" guidance to enumerate exactly what may now be acted on
without re-reading — the deleted clauses are the program's scoreboard.
