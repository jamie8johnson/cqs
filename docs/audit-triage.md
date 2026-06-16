# Audit Triage (v1.46.1)

Audit date: 2026-06-15
Source: `docs/audit-findings.md` (22 raw findings → 19 triaged rows after dedup/cluster).
Prior cycle: `docs/audit-triage-v1.42.0.md` (P1+P2 COMPLETE; this cycle deduped against it — no new finding re-states a closed v1.42/v1.40 item).

Tier-refinement notes (overrides vs the auditors' deterministic proposed_tier):
- **DOC-V1.46.1-1 / DOC-V1.46.1-2 promoted P3→P2.** Both are docs lies about live agent-facing config knobs (`CQS_CACHE_MAX_BYTES`, `CQS_CONVERT_WEBHELP_BYTES` — nonexistent vars an agent would set and get a silent no-op). The docs-lying-is-P1 rule treats a doc that promises behavior the code doesn't deliver as a correctness bug; at medium impact that lands P2. The fix is still mechanical (delete/replace a README row) → auto-fix.
- **CQ-V1.46.1-1 promoted P4→P3.** medium/medium is not "hard"; and it is the structural root behind OB-V1.46.1-1 and TC-ADV-V1.46.1-1 (same `parse_file_relationships*` test-only path) — the v1.46.0-class structural fault behind #1958/#1955. Fix is an architecture call (collapse vs delegate vs factor) → issue, not auto-fix.
- **EXT-V1.46.1-1 kept P3 but routed auto-fix on option (b) only** (drop the "extensible" docstring lie — itself a small docs-lying fix); the `[dead]` config-section alternative is the issue path.
- Findings with an existing GitHub issue (API-V1.46.1-1 → #1459, PB-V1.46.1-2 → #1512) stay **P4 (tracked)** — no NEW standalone slice large enough to break out, though the `include_types`/`type_impact` rename + `deny_unknown_fields` is a clean sub-slice if #1459 wants one.
- Route-by-fix-nature: the two high-impact RM/TC rows split — TC-HAP-V1.46.1-1 (add a test, mechanical+verifiable) is auto-fix even at P2; RM-V1.46.1-1 (panic-policy decision) is issue even at P2.

## Summary by Priority

| Priority | Count | Definition |
|----------|-------|------------|
| P1 | 0 | Easy + high impact (or lying docs) — fix first |
| P2 | 4 | Medium + high impact, or docs-lying — fix in batch |
| P3 | 11 | Easy + low/med impact — fix if time |
| P4 | 6 | Hard / low impact / already-tracked — issue or inline |

New v1.46.1 findings: 21 triaged rows (from 22 raw; 2 merged into 1, on issue #1459). Routes: auto-fix 14, issue 5, tracked 2, inline 0, drop 0.

## P1 — easy + high impact

(none this cycle)

## P2 — medium + high impact (or docs-lying)

| ID | Finding | Location | Route | Status |
|----|---------|----------|-------|--------|
| RM-V1.46.1-1 | Daemon dispatch `catch_unwind` is dead under `panic = "abort"` — a dispatch panic aborts the warmed daemon (drops ~600MB caches; remote restart/DoS primitive) | src/cli/watch/socket.rs:257-330 × Cargo.toml:355 | issue | open |
| TC-HAP-V1.46.1-1 | `apply_dead_overlay` Direction A (parent-dead → live resurrection) untested at every layer — high-visibility false-positive surface in dead/ci/review under overlay | src/store/calls/dead_code.rs:1022-1031 | auto-fix | open |
| DOC-V1.46.1-1 | README documents fictional `CQS_CACHE_MAX_BYTES` with inverted behavior; real knob is `CQS_CACHE_MAX_SIZE` (auto-evicts) — silent no-op for an agent bounding cache growth | README.md:938 | auto-fix | open |
| DOC-V1.46.1-2 | README documents nonexistent `CQS_CONVERT_WEBHELP_BYTES`; merged-output cap is a hardcoded 50 MB const — silent no-op | README.md:803 vs src/convert/webhelp.rs:118 | auto-fix | open |

## P3 — easy + low/med impact

| ID | Finding | Location | Route | Status |
|----|---------|----------|-------|--------|
| PERF-V1.46.1-1 | `embed_batch` deep-clones every chunk string per batch (`texts.to_vec()`); comment wrongly claims it's unavoidable — embed/index hot path | src/embedder/core.rs:1006-1011 | auto-fix | open |
| PERF-V1.46.1-2 | SPLADE `encode_batch` tokenizes serially in a `map()` loop, forgoing the tokenizer's rayon parallel batch — production index-build path | src/splade/mod.rs:799-808 | auto-fix | open |
| PERF-V1.46.1-3 | `CallGraph::edge_meta` allocates two `Arc<str>` per lookup in cross-project caller/callee rendering | src/store/helpers/types.rs:568-573 | auto-fix | open |
| DOC-V1.46.1-3 | README documents wrong default for `CQS_BUSY_TIMEOUT_MS` (5000); actual fallback is 30000 (6× under-estimate of the lock-wait window) | README.md:777 vs src/store/mod.rs:1075 | auto-fix | open |
| RM-V1.46.1-3 | Overlay `discover_delta` builds full records/masked_origins/parse_set BEFORE the size cap rejects — the DoS rail is post-hoc, doesn't bound peak memory of the function it guards | src/worktree_overlay.rs:809-868 | auto-fix | open |
| TC-HAP-V1.46.1-2 | `distinct_callees_from_origins` has no direct test for multi-origin / dedup / empty / path-normalization behavior (feeds Direction-B candidate set) | src/store/calls/query.rs:684 | auto-fix | open |
| TC-ADV-V1.46.1-1 | `parse_l5x_all` call-graph + type-ref extraction has zero test coverage, incl. over malformed/error-recovered ST | src/parser/l5x.rs:590-613 | auto-fix | open |
| OB-V1.46.1-1 | InvalidData (non-UTF8) silent-skip in `parse_file_relationships_with_candidates` diverges from the two sibling parse sites that warn (sweep straggler) | src/parser/calls.rs:1284-1285 | auto-fix | open |
| CQ-V1.46.1-1 | Test-only parallel call/type-extraction path (`parse_file_relationships_with_candidates`) diverges from the production index path — the #1958/#1955 structural fault class; two live divergences remain | src/parser/calls.rs:1258-1569 vs src/parser/mod.rs:478-568 | issue | open |
| API-V1.46.1-2 | `SearchArgs` duplicates the three overlay fields inline instead of flattening the shared `OverlayArgs` (copies already drifting) | src/cli/args.rs:257-284 vs :103-129 | auto-fix | open |
| EXT-V1.46.1-1 | `cqs dead` known-gap allowlists are compile-time consts despite a docstring claiming "extensible" — docstring lie (option b) + optional `[dead]` config (option a) | src/cli/commands/review/dead.rs:98,:110-113,:144-176 | auto-fix | open |

## P4 — hard / low impact / already-tracked

| ID | Finding | Location | Route | Status |
|----|---------|----------|-------|--------|
| RM-V1.46.1-2 | Daemon `in_flight` slot decremented post-call, not via RAII — leaks a slot on panic outside the inner catch_unwind (moot under abort, live the moment panic policy → unwind; live now in debug/test) | src/cli/watch/daemon.rs:246-247 | issue | open |
| API-V1.46.1-1 | Two parallel `*Args` layers (clap wire vs core) with no exhaustiveness guard; `include_types`/`type_impact` name-mismatch is the live deserialization symptom (merged: includes the systemic copy-function duplication) | src/cli/commands/graph/impact.rs:42 vs src/cli/args.rs:341 | tracked | open (#1459) |
| EH-V1.46.1-1 | Overlay `fingerprint()` collapses transient read errors to the deletion sentinel (ZERO32) — risks a stale-overlay cache hit, silent; needs design call (distinct sentinel vs return Result) | src/worktree_overlay.rs:978-981 | issue | open |
| PB-V1.46.1-1 | `platform_cfg_sweep_test` guards only the unsized-binding shape, not the dead-code-on-a-sibling-target shape that also broke v1.46.0 (the other 2 of 3 cross-build breaks) | tests/platform_cfg_sweep_test.rs | issue | open |
| PB-V1.46.1-2 | `is_wsl_drvfs_path` `cfg!(windows)` UNC branch is effectively dead — forward-slash literals vs backslash Windows paths; coarse-fs falls to ZERO on a native-Windows WSL-UNC share | src/config.rs:136-140 | tracked | open (#1512) |
| TC-HAP-V1.46.1-3 | Dart body-inclusive `canonical_hash` (the `extra`-node comment-stripping path, #1970) is untested — a regression silently changes Dart chunk ids on comment-only edits, defeating embedding-cache reuse (injectivity surface, #1947) | src/parser/chunk.rs:178-192 | auto-fix | open |

> Note: RM-V1.46.1-2 is listed P4 by impact-today (moot under release abort) but its fix is trivial (RAII guard) and it becomes live under any unwind build — it should land in the SAME PR as RM-V1.46.1-1 if that PR chooses option (a) unwind, since unwind makes the leak real. Treat as a rider on RM-V1.46.1-1's panic-policy decision.

## Overrides log (proposed_tier → assigned)

| ID | proposed | assigned | reason |
|----|----------|----------|--------|
| DOC-V1.46.1-1 | P3 | P2 | docs-lying about a live agent-facing config knob (silent no-op) |
| DOC-V1.46.1-2 | P3 | P2 | same — nonexistent override var, silent no-op |
| CQ-V1.46.1-1 | P4 | P3 | medium/medium ≠ hard; structural root of OB + TC-ADV rows; the #1958/#1955 fault class |
| (all others) | — | = | proposed_tier accepted |

## Dedup / cluster merges

- **API-V1.46.1-1** merges the auditors' two API rows — `include_types`/`type_impact` name divergence (the live symptom) AND the systemic "two parallel `*Args` layers, no exhaustiveness guard" (the root). Both carry existing_issue #1459; one tracked row.
- **OB-V1.46.1-1** + **CQ-V1.46.1-1** share the same root file (`parse_file_relationships_with_candidates`, the test-only parse path). Kept as two rows because the fixes differ (one-line warn vs path-collapse architecture) and route differently (auto-fix vs issue), but the warn should land in whichever PR touches that path. **TC-ADV-V1.46.1-1** also orbits this family (L5X ST extraction) — same structural neighborhood, independent fix.
- The three README env-var rows (cache / webhelp / busy-timeout) stay separate (distinct vars, distinct files/lines, distinct fixes) but land in ONE docs PR.

## Suggested fix clustering (for the implementation session)

- **Daemon panic-survival cluster (P2 + rider):** RM-V1.46.1-1 (decide panic policy) + RM-V1.46.1-2 (RAII slot guard — required if option (a) unwind is chosen). One issue, one PR.
- **Docs sweep (P2/P3, one PR):** DOC-V1.46.1-1, DOC-V1.46.1-2, DOC-V1.46.1-3, + EXT-V1.46.1-1's docstring-drop half.
- **Tokenizer hot-path perf (P3, one PR):** PERF-V1.46.1-1 (embed_batch) + PERF-V1.46.1-2 (SPLADE encode_batch) — both "use the batch tokenizer API, drop the per-item/per-body clone".
- **Dead-overlay test backfill (P2/P3, one PR):** TC-HAP-V1.46.1-1 (Direction A resurrection) + TC-HAP-V1.46.1-2 (distinct_callees_from_origins) — same fixture surface (worktree_overlay_build.rs / store_calls).
- **Parse-path consolidation (P3, issue):** CQ-V1.46.1-1 (collapse) carries OB-V1.46.1-1 (warn) as a rider; TC-ADV-V1.46.1-1 (L5X tests) can land alongside.
- **Cross-build guard (P4, issue):** PB-V1.46.1-1 — pairs with the project's `cargo check --target <sibling>` discipline; consider a scratch clippy job.

---

# Carried forward from v1.42.0 (still open)

⚠️ **Full carry-forward reconciliation against all PRs merged since v1.42.0 is the orchestrator's to confirm.** The v1.42.0 triage records P1+P2 COMPLETE (2026-06-11) and the great majority of its P3/P4/CF rows are marked ✅ across PRs #1738-#1799. Below are the survivors after spot-grepping the ambiguous ones against current main (2026-06-15); treat as a starting point, not an audited-complete list.

## CF-P2 — carried forward, P2-grade

| ID(s) | Finding | Location | Status |
|-------|---------|----------|--------|
| (none confirmed open) | All ten v1.42 CF-P2 rows show ✅ in the archived triage (PRs #1766/#1769/#1770/#1773/#1792/#1799); PB-V1.40-9 refuted (production runs WAL on v9fs daily). | — | orchestrator to confirm |

## CF-P3 — carried forward, P3-grade

| ID(s) | Finding | Location | Status |
|-------|---------|----------|--------|
| EH-V1.40-7 | `lookup_by_name` empty-string short-circuit undocumented — one doc line, fold into next query.rs touch (closed-with-decision in v1.42 triage; the doc line is cheap to land on any query.rs PR) | src/store/.../query.rs | CF-P3 (trivial rider) |

## Standing deferrals (decision, not tables)

Carried by explicit decision from the v1.42.0 triage — re-listed only for visibility:
- DS-V1.40-7 / EXT-V1.40-4 — sentiment CHECK constraint / Sentiment enum: schema-migration cost > benefit, single user. **Deferred.**
- P4 umbrellas still tracked on GitHub: **#1463** (4 remaining design items), **#1459** (project/ref verb consolidation — API-V1.46.1-1 attaches here), **#1512** (Windows daemon FS detection — PB-V1.46.1-2 attaches here), **#1573** (dead-code tiers 3a/3b/4: EXT-V1.40-3/7, PERF-V1.40-10).
- SHL-V1.40-3 (next perf cycle), SHL-V1.40-6 (#1453 successor) — deferred by cadence.
