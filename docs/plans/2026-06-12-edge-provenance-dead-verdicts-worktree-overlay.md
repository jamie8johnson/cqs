# Design: Edge Provenance, Dead-Code Verdicts, Worktree Search Overlay

Status: PROPOSED (design only — nothing queued)
Origin: 2026-06-12 session. All three motivating gaps were hit live during the
v1.43.0 campaign by our own agents; this doc specs the fixes. A fourth
candidate (`cqs review --base`) turned out to already exist — see §4.

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
  which is correct for every pre-v30 row except serde/macro edges — those
  re-extract anyway on the next reindex (PARSER_VERSION is already 4), so no
  backfill logic is needed. Values: `call`, `serde_callback`,
  `macro_heuristic`, reserving `fn_pointer`, `dyn_dispatch`.
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

## 4. Correction: `cqs review --base` already exists

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
§1's column, and one migration beats two. §4 rides any docs PR. §3 gets its
own implementation plan after §1/§2 land (the overlay's shadow semantics
deserve a fable review pass at design time, not just at PR time), and lands
search-only behind the flag first.
