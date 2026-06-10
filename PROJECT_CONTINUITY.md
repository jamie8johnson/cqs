# Project Continuity

## Right Now

**v1.42.0 RELEASED — 2026-06-09.** Tagged, crates.io published, GitHub release binaries built, local binary + daemon on 1.42.0, target dir cleaned (166 GiB). Schema v28 (canonical_hash). Toolchain 1.96.0 local + CI.

**cuvs upstream contribution fleet — COMPLETE, all 5 PRs submitted, awaiting maintainer review (2026-06-09/10):**
- **PRs open on rapidsai/cuvs:** #2229 IVF-SQ (cqs #1678), #2230 refine (cqs #1684), #2231 brute_force serialize (cqs #1682), #2234 scalar quantizer (cqs #1683), #2235 tiered index (cqs #1685 — the upstream half of "kill the periodic HNSW rebuild"). All agent-implemented in isolated worktrees (~/cuvs-wt-*), code-reviewed to maintainer standards, GPU-tested single-threaded.
- **All CodeRabbit bot feedback resolved** (~9 follow-up commits, never force-pushed): unique temp filenames (#2229/#2231), full top-k ordering assert (#2230), i8 dtype guards on transform/inverse + guard-specific test asserts + non-panicking Drops (#2234), backend-aware SearchParams enum + safe bitset filter API (#2235 — fixed a Critical soundness hole: raw cuvsFilter in a safe fn). Quantile-range nitpick declined on-record (matches C + sibling conventions). Committed PR-body artifact removed from #2231's tree.
- **Gate:** copy-pr-bot vetting blocks NVIDIA CI on all five until a maintainer approves. Expect months (prior PR #2019: ~2 months).
- **Issues filed upstream with recommended fixes:** rapidsai/cuvs#2232 (ManagedTensor borrows host ndarray shape storage — dangling pointer at Drop if host array dies first; fix: owned Box<[i64]> dims), #2233 (parallel cargo test SIGSEGVs on GPU tests; fix: harness-level serialization + Resources thread-safety audit). Both offer follow-up PRs.
- **Maintenance rule for our PRs:** overlapping lib.rs/bindings.rs edits — merge main into branches as siblings land, NEVER rebase after review starts (their guideline). Version-pin dance for local testing: sed workspace 26.8.0→26.6.0, test against conda libcuvs 26.06, revert before commit.
- **Monthly cloud routine `cuvs-prs-follow`** (trig_013tdB4kKRZBeFX2UQjk1A9g, 3rd of month 14:17 UTC): enumerates all our open cuvs PRs live, follow-only (no posting — user posts nudges manually), next run Jul 3. Validated by a manual smoke run (it correctly reported pre-submission state and surfaced the C-API-prerequisite timeline unprompted).

**CAMPAIGN CLOSED 2026-06-10: command-core unification.** Eight PRs in one overnight run: #1688 (phase 0, cap parity), #1689 (phase 1, graph cores), #1694 (2a, query_core + typed search output), #1695 (2b-search, daemon through SearchCtx, ChunkOutput deleted), #1696 (2b-io, 7 io cores), #1697 (phase 3, infra/index), #1698 (phase 4, review/train + _with_posture deleted), #1699 (docs truth sweep, 6 lies fixed + repo description). Net: ~40 commands have one surface-agnostic core each with typed Deserialize Args / Serialize Output (MCP-tool-shaped), daemon dispatchers are thin adapters, parity tests pin CLI==daemon, zero retrieval-semantics change (eval gates held all night). Deferred work: the plan's 11-entry post-campaign ledger (`docs/plans/2026-06-10-command-core-unification.md`) + issues #1690-#1693. Audit findings closed along the way: queue item 1, CQ-V1.40-1/2/3/4/5/6/9, API-V1.40-1, EXT-V1.40-1, RM-V1.40-1, TC-HAP-V1.40-4 + parity-test backfill. Next from the queue: #1693 (test-concurrency policy), then #1690/#1691 per triage ordering.

**Queue work in flight: #1692 reuse-chain unification — PR #1701 in CI.** One shared `cli/pipeline/reuse.rs::resolve_reuse` replaces the ~95-line cache chains duplicated between `prepare_for_embedding` and the watch incremental reindex; index-based ReuseSplit preserves each caller's ownership model; reuse decisions verified line-by-line unchanged. Strict eval gate held (paired before/after on the same index: R@5/R@20 identical, R@1 ±1 tie-break noise). Forensic note: `prepare_for_embedding_store_cache_hit_skips_to_embed` had been FAILING under #[ignore] since #1677 (empty canonical_hash → NULL bind → IS NOT NULL filter) — reviewer independently reproduced the baseline failure via stash before accepting the repair. Watch-path store-read errors now degrade to re-embed (best-effort, matches bulk) instead of aborting.

**cuvs PR maintenance (2026-06-10):** three CodeRabbit rounds on #2235 all resolved (backend-aware SearchParams enum + safe bitset filter + positive IVF-Flat smoke test, 8/8 green); #2229/#2231 temp-filename fixes; #2230 full top-k ordering assert; #2234 i8 dtype guards + guard-specific asserts + non-panicking Drops, quantile nitpick declined on-record. All five PRs now gated only on copy-pr-bot maintainer vetting.

**Standing automation:** fix loop (session cron efc9e985, 5h — merges green PRs, continues campaign, else audit queue / #1680 / #1350 / #1351); tears loop (session cron 16f0275a, 2h); monthly cloud routine cuvs-prs-follow (next Jul 3).

**Operational notes from today:** daemon CLI client has no socket read timeout — request racing a daemon restart blocks forever (hung cli_envelope_test 40min; in notes.toml + audit queue). Never restart cqs-watch mid-test-suite. WSL git push now works directly (credential helper wired into cqs + cuvs clones; gh still via PowerShell). Telemetry reset 23:03 (archived window: impact 71% = pre-edit hook, search 4.4%, kind-fallback invisible until OB-V1.40-2). cuvs Rust gotcha: keep host ndarray alive while any index built from it lives (see #2232).

### Earlier today — comment hygiene + dependabot drain

Two arcs, both closed:

- **All-source comment cleanup (#1673, merged).** 231 of 264 `.rs` files: removed ~1,500+ changelog/provenance refs from comments (audit IDs, PR citations, "previously/pre-fix" narration), rewrote present-tense. Executed by 10 parallel opus agents with disjoint file ownership; integration mechanically verified the diff was comment-only (trailing-comment edits on byte-identical code allowed) and reverted 17 string-literal leaks before commit. One real doc fix: `lib.rs` `EMBEDDING_DIM` doc said 1024/BGE-large, is 768/EmbeddingGemma. Same PR brought ROADMAP.md to v1.41.0 currency (v1.40.0 → Previous, SNR Phases 4-6 flipped to shipped, Open Issues re-audited — 30 closed rows pruned, DS-V1.40-1/DS-V1.40-7 deferred P1s now tracked).
- **Dependabot drain (#1668–#1672, all merged).** Rust 1.96 stable's new `manual_option_zip` clippy lint broke CI on every open PR; fixed the two sites on main (#1672), `@dependabot rebase`d all four dep PRs, merged: uuid 1.23.2, log 0.4.31, serial_test 3.5.0, tree-sitter-swift 0.7.3. Two CI flakes encountered and cleared on rerun: HuggingFace 429 model download, `tc31_save_and_load_with_dim_1024` HNSW nearest-neighbor assert (noted in notes.toml — will bite again).

State: zero open PRs, main green at c396dd8a, binary 1.41.0+main installed, daemon restarted with reconcile queued (comment churn → content-hash changes → background re-embed). Telemetry kind-fallback measurement window still accumulating since the 2026-05-08 reset.

### Previous: v1.40.0 cycle wrap-up — 2026-05-08/09

24-PR session shipped v1.40.0 to crates.io (SNR Phases 1-4 + Polymorphic Routing Phase 1 + Tier 2a/2b cqs-dead reductions). Then ran the **post-v1.40.0 16-category audit**: 150 raw findings → 78 triaged → 9 closeout PRs (#1626–#1633) closing **21 of 23 P1 entries** (~91%). 2 P1s deferred with rationale (DS-V1.40-1 daemon cache invalidation — symptom rare; DS-V1.40-7 sentiment CHECK constraint — single-user discrete-value compliance reliable). Audit cycle's diminishing-returns curve confirms project maturity: 14th full audit, no architectural-correctness P1s remain. Net read: cqs is at the "polished and shipping value" stage; remaining work is steady-state plus telemetry-driven (Phase 2 polymorphic routing) and a potential MCP interface.

**Phase 1 polymorphic routing — 60/60 dispatch points complete.** Every function-or-type-specialized command (`impact`, `callers`, `callees`, `test-map`, `trace`, `deps`) consults `cqs::kind::classify_hits` against an exact-name lookup before its happy-path query, on both CLI-direct (#1612, #1616, #1617, #1618) and daemon-path (#1620). Const/Type/Module/Ambiguous kinds get a kind-labeled fallback shape (`{kind, fallback_from, name, definitions, note}`) instead of misrouted-to-empty results. Verified live: `cqs callers HANDLING_ADVICE` → const fallback; `cqs test-map ImpactOptions` → ambiguous fallback (struct + impl).

**cqs dead false-positive reduction.** Three tier closes in this session: Tier 1 (PR #1612 et al — Function with `kind` label) trimmed `~114 → 80 → 52`. Tier 2a (PR #1621 + #1622) added `field_initializer` + `type_cast_expression` patterns to `rust.calls.scm` — closed all 66 false positives in `src/language/languages.rs` plus the 14 `post_process_*` casts (`52 → 38`). Tier 2b partial (PR #1623) added a content-scan filter in `dead_code.rs` for macro invocations — closed 3 of 5 macro false positives (`38 → 35`). Remaining 2 macros (`for_each_logged_batch_cmd`, `gen_log_query_dispatch`) require a chunker change to include doc comments / file-level statements, deferred as architectural.

**Telemetry reset twice today.** First reset at 08:27 UTC archived 4506 events for the post-SNR-Phase-1-3 baseline. Second reset at 21:46 UTC (`telemetry_20260508_214618.jsonl`, 441 events) gives a clean post-Phase-1-routing + post-Tier-2b counter starting now. The 13h between-reset window was dominated by autopilot smoke (search rate 4.6%, but `impact` 35-call spike was me exercising every kind cell, not agent behavior). Real signal needs 1-2 weeks of agent-driven coding sessions before the kind-fallback hypothesis can be tested against the 79% → 6% search-rate decline that motivated v1.40.

**Headline shipped:**
- SNR restoration Phases 1-4 (#1601, #1602, #1604, #1609, #1613) — CLI direct defaults to bare JSON payload; `CQS_OUTPUT_FORMAT=v1` consumer-migration hedge; `CQS_ULTRASECURITY=1` adversarial override on every surface.
- Polymorphic routing Phase 1 — lib plumbing (#1610) + 30 CLI-direct cells (#1612, #1616, #1617, #1618) + 30 daemon-path cells (#1620). 6 commands × 5 kinds × 2 surfaces = 60 dispatch points.
- v3.v2 eval fixture refresh (#1607) — agg R@K +6.4 / +2.7 / +3.2 pp, above v1.36-snapshot.
- v1.40.0 release (#1614) — tag pushed, crates.io published, GitHub Release auto-fires.
- cqs dead false positives reduced ~114 → 35: #1621 (struct-field-assignment edges, -52), #1622 (type-cast edges, -14), #1623 (macro content-scan, -3 of 5).
- env_var_docs hardening (#1606), gitignore housekeeping (#1603), telemetry reset.

**Audit cycle (post-v1.40.0) — 9 PRs closing 21 of 23 P1s:**
- #1626 — Cluster E lying-docs sweep (8 DOC + 1 OB doc-drift). ROADMAP / SNR doc / polymorphic-routing doc / SECURITY / CHANGELOG / Cargo.toml / README all flipped status from "ready" / "always-on" / "46.3" → "shipped" / "opt-in" / "52.7".
- #1627 — Cluster A Tier 2b correctness + 6 tests. `filter_invoked_macros` switched LIKE → GLOB (case-sensitive, no `_` wildcard collision); Rust-only language guard; `id != ?2` self-exclusion for recursive macros.
- #1628 — misc P1 batch (RB + DS + SEC, 5 findings). `TestMapArgs::depth` clap range bound; `classify_hits` `.expect` → `unwrap_or(Kind::Other)`; `cmd_telemetry_reset` atomic rename + `atomic_replace`; `regenerate_v3_test.py` atomic write; `redact_userinfo` RFC-3986 authority bounding.
- #1629 — Cluster B Posture/OutputFormat env-var hygiene (11 findings). `OnceLock` cache + truthy aliases (`1`/`true`/`on`/`yes` case-insensitive) + `tracing::warn!` on unrecognized + `tracing::info!` first-read + 12 pure-parser unit tests replacing env-mutating ones.
- #1630 — SEC-V1.40-1 V2Bare worktree_stale signal restored. Object payloads splice `_meta` in-place; array/scalar payloads emit stderr warning.
- #1631 — DS-V1.40-3 `restore_from_backup` deletes live sidecars first. Closes the "Frankenstein WAL replays against pre-migrate main" silent-corruption window.
- #1632 — Cluster H DoS amplifier closure. `chunk_to_definition_value` shared helper enforces `KIND_FALLBACK_MAX_DEFINITIONS=100` + `KIND_FALLBACK_MAX_CONTENT_BYTES=2048` with UTF-8-safe truncation. Both CLI-direct and daemon paths now share the cap.
- #1633 — AC sweep (3 BFS/parser correctness P1s). `bfs_shortest_path` predecessor `String → Option<String>` (handles anonymous mid-chain nodes); Tier 2a `arguments` patterns anchored to first child via `.` predicate (no more phantom `→ count` edges keeping unrelated globals alive); `bfs_expand` depth-min lifted out of score-gated branch.

**Deferred from this audit cycle (2 P1s, with rationale):**
- DS-V1.40-1 (daemon Store cache invalidation) — symptom rare; caches reload on 100ms staleness check. Cluster D bundling deferred to v1.41 cycle.
- DS-V1.40-7 (sentiment CHECK constraint) — schema-migration cost > benefit for single-user discrete-value compliance per CLAUDE.md memory.

**Audit mode disabled** at end of cycle (was on for ~12h). Notes are back in search/read for normal operation.

### Shipped 2026-05-08 session — 24 PRs

| PR | Title | Notes |
|---|---|---|
| #1601 | feat(json): Posture enum + _with_posture emission helpers (SNR Phase 1) | additive plumbing |
| #1602 | feat(json): per-result skip-when-default + posture-gated force-emit (SNR Phase 2) | ~30% smaller per result in friendly mode |
| #1603 | chore(gitignore): re-ignore tools/screw-mcp/ + add .screw-tape/ cache | screwtape folder un-tracked |
| #1604 | feat(json): slim batch/daemon envelope under Friendly (SNR Phase 3) | ~70 KB saved per 1000-line fixture batch |
| #1605 | docs(roadmap): SNR Phases 1-3 shipped; 4-6 deferred | mid-session status |
| #1606 | fix(tests): env-var-docs substring → token match + pre-commit step | items 3+4 |
| #1607 | chore(eval): refresh v3.v2 fixture line numbers — agg R@K +6.4/+2.7/+3.2pp | item 6 |
| #1608 | docs(tears): autopilot session 2026-05-08 — 7 PRs + telemetry reset | early session capture |
| #1609 | feat(json): CQS_OUTPUT_FORMAT=v2 opt-in for bare-payload (SNR Phase 4 plumbing) | opt-in landed |
| #1610 | feat(kind): Kind detection lib module + Store::lookup_by_name (Polymorphic routing Phase 1 plumbing) | enums/helpers/SQL building blocks |
| #1611 | docs(tears): autopilot session final tally — 10 PRs + plumbing | second tears capture |
| #1612 | feat(impact): const fallback — kind-labeled definitions instead of empty | first cell of (command × kind) matrix |
| #1613 | feat(json): flip default to bare payload on CLI direct (SNR Phase 4 proper) | **breaking change** — default cqs --json shape |
| #1614 | chore: Release v1.40.0 | tag pushed, crates.io published, GitHub Release auto-fires |
| #1615 | docs(tears, roadmap): v1.40.0 cut + Phase 1 polymorphic routing status | post-release sync |
| #1616 | feat(impact): complete kind-mismatch matrix — Type, Module, Ambiguous cells | impact 5/5 cells |
| #1617 | feat(callers, callees): kind-mismatch matrix | callers + callees 10/10 cells |
| #1618 | feat(test-map, trace, deps): kind-mismatch matrix completes Phase 1 (CLI-direct) | last 15/15 cells, 30/30 CLI-direct |
| #1619 | docs(tears, roadmap): Phase 1 polymorphic routing complete (30/30 cells) | mid-session tears |
| #1620 | feat(batch dispatch): polymorphic-routing kind-fallback for daemon path | 30/30 daemon-path cells; closes the deferred surface from #1618 |
| #1621 | fix(parser): Rust struct-field-assignment edges (#1573 Tier 2a) | -52 cqs-dead false positives (66 → 14 in `src/language/languages.rs`) |
| #1622 | fix(parser): Rust struct-field type-cast edges (#1573 Tier 2a follow-up) | -14 (closes the remaining `post_process_*` casts to 0) |
| #1623 | fix(dead_code): macro content-scan filter (#1573 Tier 2b partial) | -3 of 5 macro false positives via `WHERE content LIKE '%<name>!%'` |
| #1624 | docs(tears, roadmap): final 23-PR session summary | meta-PR closing the session |

Plus: `cqs telemetry --reset` ran twice — first at 08:27 UTC (archived 4506 events to `telemetry_20260508_082716.jsonl`), again at 21:46 UTC (archived 441 events to `telemetry_20260508_214618.jsonl`). Triage comment posted on #1459 marking sub-items 4, 7, 8 already done in v1.36→v1.38 cycles. Installed binary refreshed multiple times during session (after #1604, after v1.40.0 tag, after Phase 1 CLI completion, after #1620 daemon-path sweep, after #1623 Tier 2b filter).

### Eval-baseline snapshot post-session

v3.v2 refreshed (PR #1607). Default slot (EmbeddingGemma-300m + per-cat α + Unknown=0.80):

| Split | R@1 | R@5 | R@20 |
|---|---:|---:|---:|
| test (n=109) | 49.5% | 72.5% | 84.4% |
| dev (n=109) | 56.0% | 82.6% | 94.5% |
| **aggregate** | **~52.7%** | **~77.5%** | **~89.4%** |

Δ vs pre-refresh aggregate (46.3 / 74.8 / 86.2): **+6.4 / +2.7 / +3.2 pp**. Brings agg R@K above the v1.36-snapshot range (50.9 / 76.2 / 88.6). Pure fixture re-anchoring — no retrieval-side change.

### Cumulative cqs-dead reduction (today's session)

```
                              Pre-Tier-1   Pre-Tier-2a   Post-base (#1621)   Post-cast (#1622)   Post-2b (#1623)
total cqs-dead entries:       ~114         ~80           52                  38                  35
src/language/languages.rs:    ~66          66            14                  0                   0
post_process_* (casts):       14           14            14                  0                   0
macro chunks flagged:          5            5             5                  5                   2
```

The remaining 35 includes 2 macros (`for_each_logged_batch_cmd`, `gen_log_query_dispatch`) flagged because their only invocation `for_each_logged_batch_cmd!(gen_log_query_dispatch);` lives at file scope (line 606 of `src/cli/batch/commands.rs`) — outside any chunk's byte range, so the LIKE-content scan in #1623 can't see it. Closing fully requires one of: (a) chunker change to include doc comments / file-level statements in chunk content, (b) file-system scan during macro-invocation check, (c) "module preamble" file-level chunk type. All three are architectural changes — deferred.

### What's left for Phase 1 polymorphic routing

**Phase 1 is complete.** All 60 dispatch points (6 commands × 5 kinds × 2 surfaces) ship the kind-fallback shape. Phase 2 (`cqs about` unified entry) is contingent — only ship if Phase 1 telemetry shows agents still bouncing between commands. Real-world telemetry needs 1-2 weeks of agent-driven usage post-v1.40.0 before that decision can be made.

### Earlier this day (v1.39.2 cycle, before autopilot)

The v1.39.2 cycle closed earlier today; section below captures it. PR #1593 inverted `_meta.handling_advice` to opt-in via `CQS_ULTRASECURITY=1` — addressed the alarm-shaped piece of the agent-friction equation. The autopilot session above shipped the response-size and routing-Phase-1 pieces; v1.40.0 bundles all of it.

---

**v1.39.2 shipped** — 2026-05-08, follow-on patch to v1.39.1 covering two threads that emerged from the same exploratory loop. Three PRs land for it: #1584 (cliff fix, already in v1.39.1), #1588 (α retune), #1589 (orphan GC). Plus #1582 (reranker closeout docs) and #1586 (tears) merged en route. Issue #1587 closes with #1589.

**Loop arc** (single session, started from "is the k*2 floor too high?"):

1. **Probed `CQS_HNSW_EF_SEARCH`** → byte-identical R@K across [0, 8000] because hnsw_rs internally floors ef to k (`let ef = ef_arg.max(knbn)` at hnsw.rs:1450). Outer ef knob below k is silently no-op.
2. **Pivoted to `CQS_SEARCH_CANDIDATE_FLOOR` sweep** → sharp cliff at floor=442 in dense-only (R@5 0.66 → 0.17). Traced to CAGRA's `itopk_max(14181) = floor(log2(14181) · 32) = 441`. CAGRA returned empty Vec for `k > 441`, with a comment claiming "caller falls back to HNSW" — true for `search_filtered_with_index`, FALSE for `search_hybrid` (SPLADE-fusion path used by every production query).
3. **Shipped v1.39.1** with `VectorIndex::max_k() -> Option<usize>` trait method + `cap_k_to_backend` dispatch helper. CAGRA reports its cap; both dispatch sites trim before calling the backend. R@5 0.5963 → 0.7156 restored at default floor=500 on hybrid path; dense-only no longer cliffs at floor>441.
4. **Refreshed LLM summaries** on the gemma slot to validate the post-cliff stack with current content. `cqs index --llm-summaries` ran 1,169-item Anthropic batch in ~23 min. Per-chunk coverage 60.25% → 68.70% (8,582 → 9,757 chunks covered; remaining 31.3% structurally ineligible — too short / types / non-summarizable per `collect_eligible_chunks`).
5. **Honest coverage measurement caught a silent drift**: `llm_summary_count` was 13,231 rows for 13,175 distinct chunk hashes (100.43% — 56 stale rows from prior reindexes). Filed #1587 (orphan GC).
6. **Paired α sweep** on v3.v2 218q post-refresh → most categories already at the test+dev joint optimum, but `identifier_lookup` was the one strong cross-fixture signal: dev R@5 0.8889 → 1.0000 at α=0.85 (n=18, plateau α=0.80..0.90). Shipped as #1588 (α retune 1.00 → 0.85). Test+dev paired sweep on 8 categories caught what a single-fixture sweep would have missed. Other categories' "wins" on one fixture but not the other are below the noise floor (n=8-14, single-query swings dominate).
7. **Implemented #1587 GC** in #1589: `Store::prune_orphaned_llm_summaries() -> Result<u64>` runs a single `DELETE FROM llm_summaries WHERE content_hash NOT IN (SELECT content_hash FROM chunks)`, auto-fires at end of `cqs index` after the final pending-summaries flush. Opt-out via `--no-prune-summaries` for cross-slot summary copy by content_hash. Plus `Store::llm_summary_chunk_coverage()` + `cqs stats --json` exposes `llm_summary_chunks_covered` + `llm_summary_chunk_coverage_pct` alongside the row-count `llm_summary_count` (kept for backward compat).

**Empirical impact (v3.v2 218q paired snapshot 2026-05-08, post-v1.39.2 stack):**

| Metric | Test (n=109) | Dev (n=109) | Aggregate (n=218) |
|---|---:|---:|---:|
| R@1 | 40.4% | 52.3% | 46.3% |
| R@5 | 71.6% | **78.0%** | 74.8% |
| R@20 | 81.7% | 90.8% | 86.2% |

Per-category R@5: identifier_lookup **91.7%** (was 86.7% pre-retune), multi_step 92.9%, negation 81.8%, type_filtered 69.2%, behavioral 65.6%, cross_language 63.6%, structural 62.5%, conceptual 56.0%.

Numbers are below the 2026-05-03 capture (50.9% / 76.2% / 88.6% agg) because the corpus drifted ~30% since then and the v3.v2 fixture matches by `(file, name, line_start)` strict — line shifts from audit-cycle PRs silently turn fixture hits into misses. Refreshing the v3.v2 line numbers would lift agg R@K back into the v1.36-snapshot range without any retrieval-side change. Fix bundle (cliff + α retune + GC) is a strict improvement on the current corpus state.

**Live GC verification** (gemma slot, 14,207 chunks): pre-prune 14,400 rows / 9,751 chunks covered (68.64%) → post-prune **9,317 rows / 9,750 chunks covered (68.63%)**. **5,083 orphans deleted in one pass.** Coverage % unchanged (correct — pruning orphans doesn't drop any live data). The auto-fire now keeps the table honest after every reindex; `cqs stats --json llm_summary_chunk_coverage_pct` is the metric to watch going forward.

**Three pacing lessons** from this loop:

1. **Recall-floor bumps need paired-eval sanity** (carry-over from v1.39.1). #1583's floor=500 immediately exceeded CAGRA's `itopk_max=441` on our own corpus; the formula was correct, the per-backend interaction wasn't on the radar.
2. **Honest-count metrics matter**. The 13,231-row `llm_summary_count` looked fine at 92.9% of total_chunks but the per-chunk reality was 60.25%. Once the gap was visible, #1587 was filed in 5 minutes. Lesson: any "X is N% covered" metric should be derived from the asymmetry it claims to measure (chunks-covered, not summary-rows), not the proxy that's easier to query.
3. **Test+dev paired sweeps for category-level retunes**. The v3.v2 single-fixture sweep flagged 5 categories as candidates; only `identifier_lookup` survived the cross-fixture check. Single-fixture R@5 wins at n=8-14 are below the noise floor — paired agreement is the cheap robustness check that costs nothing extra to compute.

---

**v1.39.1 shipped** — 2026-05-07. Patch release on crates.io. One PR (#1584) plus #1582 (reranker closeout docs) and #1586 (tears) en route. CAGRA itopk_max cliff in the SPLADE-fusion path; see v1.39.2 loop above for the trail end-to-end.

---

**v1.39.0 shipped** — 2026-05-07. Minor release. 88 commits since v1.38.0 across three threads:
- v1.38.0-cohort audit follow-ups (#1487–#1511)
- post-v1.38 audit cycle of 154 findings catalogued in PR #1515 — ~64 closed across ~33 cluster PRs (#1514–#1570)
- post-cycle hardening of the watch/reindex path (#1572, #1575, #1577)

**Headline operator-visible changes** (v1.39.0):
- Daemon stops SIGFPE'ing on EmbeddingGemma reindex (#1577 TRT-incompatibility blocklist; root cause #1576 upstream). Pre-fix observed 4 daemon crashes/day.
- Cross-project commands (`trace`, `callers`, `deps`, `impact`, `test_map`) work again on slot-migrated projects since #1105 (#1564 fix).
- Atomic per-file reindex (#1575) — mid-batch crash leaves no asymmetric state between `function_calls` and chunks/FTS.
- `cqs dead` noise rate cut from ~80% to ~30% (#1572 — Property + doc-extension filters).
- Graph commands now reject `--limit 0` at parse time (#1569 LimitArg fan-out).

**Operational lesson from v1.39.0 release**: #1495's `cqs-macros` workspace split (which landed AFTER v1.38.0 was tagged) had `publish = false`, blocking `cargo publish -p cqs`. Fixed in #1579: dropped publish=false, filled in standard Cargo.toml metadata, then published cqs-macros 0.1.0 first followed by cqs 1.39.0. **Going forward both crates need version bumps coordinated whenever cqs-macros's surface changes.**

## Audit umbrellas — current state

- ✅ **#1463 (P4 design-level)** — ~64 of 154 findings closed across the v1.38 cycle. Truly remaining (all genuinely big or platform-blocked):
  - **API-V1.38-6** (top-level Cli flag → subcommand parity) — clap conflict on duplicate flag definitions; needs SearchArgs locals removed AND every search-wrapping handler rewritten to read from cli.*. Lib `cqs::scout()` doesn't accept filter knobs at all.
  - **DS-V1.38-4 deeper hazard** (HNSW half-renamed-set under a lock-then-rename window) — needs bundle-into-single-file refactor + migration path. Easy mitigation already shipped in #1570.
  - **PL-V1.38-2** (SPLADE Windows umask) — needs Windows test runner.
  - **TC-HAP-V1.38-3** (`enrichment_pass` itself untested) — needs real embedder load (~91 MB).
  - 12 P4 carry-overs all tracked separately (#1512 Windows daemon, #1461 Windows ACL, etc.).
- ⏳ **#1459 (P3 API design)** — 7 of 8 sub-items shipped. Item 2 (project/ref verb consolidation) remains; user investigation found ref + project are genuinely distinct primitives.
- ✅ **#1460, #1461, #1462** — closed in v1.38.0
- ✅ **#1366** (proc-macro CLI derive) — closed by #1495
- ✅ **#1452** (skip first-pass embed) — closed by #1497
- ✅ **#1453** (per-slot SPLADE α) — closed by #1472
- ✅ **#1458** (TC Happy 5 tests) — closed in v1.39.0 cycle

## Open issues (re-verified 2026-06-09)

All 15 open issues confirmed still open against GitHub — none stale.

| # | Status | Why open |
|---|---|---|
| #106 | tier-3 | ort 2.0-rc.12 stable release blocked on upstream pykeio |
| #255 | tier-3 | Pre-built reference packages — signing/registry design (infra, not code) |
| #717 | tier-3 | HNSW mmap — needs lib swap to hnswlib-rs (nightly-only) |
| #916 | tier-2 | SPLADE mmap — audit-deprioritized (59 MB transient) |
| #1043 | platform | Windows network drives — needs Windows test runner |
| #1139 | enhancement | structural_matchers shared library — partially landed (per-language data exists; cross-language sharing remains) |
| #1140 | enhancement | Embedder preset extras map — explicitly skipped per autopilot directive |
| #1350 | architecture | apply_scoring_pipeline hand-coded — P4-14 deferred |
| #1351 | architecture | HNSW DistCosine type-baked — needs persist migration |
| #1391 | enhancement | NVRTX (TensorRT-RTX) — blocked on ORT Linux platform gate |
| #1459 | umbrella | API design — 1 of 8 items remaining (project/ref verb consolidation) |
| #1463 | umbrella | P4 — see audit umbrella state above |
| #1512 | platform | Windows daemon named pipes — needs Windows runner |
| #1573 | new | cqs dead tier 2/3 false-positive sources (filed during v1.39.0 cycle) |
| #1576 | upstream | TensorRT 10 SIGFPE during ONNX engine compilation for Gemma — filed against NVIDIA |

## Recent release history (compressed)

- **v1.39.0** (2026-05-07) — 88-commit minor release. v1.38 audit cycle + post-cycle hardening (atomic reindex #1575, TRT blocklist #1577, cqs dead noise filter #1572). Schema unchanged at v27.
- **v1.38.0** (2026-05-06) — 13 audit-driven PRs closing #1460/#1461/#1462. Per-slot SPLADE α tables (#1472), TOML overlays for FTS synonyms + classifier vocab, `cqs serve` concurrent-request cap (#1477), daemon socket TOCTOU hardening (#1478). No schema bump.
- **v1.37.0** (2026-05-05) — v1.36.2 audit close-out (#1456): 120/163 findings addressed. Dim-scaled batch sizes (#1464). Promoted `cqs::limits` to `pub`. `RerankerMode::Llm` removed.
- **v1.36.2** (2026-05-04) — critical fix (#1451): long-running `cqs index` no longer crashes with SQLITE_BUSY when concurrent `cqs` invocations overlap. busy_timeout 5s → 30s.
- **v1.36.1** (2026-05-04) — qwen3-embedding-4b preset (#1441/#1442) — 7.4 GB FP16, 2560-dim, 4096 max-seq.
- **v1.36.0** (2026-05-03) — schema v25→v26. Per-category SPLADE α retuned for EmbeddingGemma + Unknown=0.80 catch-all hedge. Net agg lift R@5 +3.7pp. 13 audit-followup fixes including critical readonly-migration bug (#1413).
- **v1.35.0** (2026-05-02) — default embedder swap BGE-large → EmbeddingGemma-300m + tokenizer-truncation correctness fix (#1384) for fine-tuned BERT-family presets.
- **v1.34.0** (2026-05-02) — post-v1.33.0 audit close-out (24 fix PRs, 129 findings) + EmbeddingGemma preset.
- **v1.33.0** (2026-05-02) — eval-matcher drift fix (#1284), placeholder-cache 30s startup tax fix (#1288, CI 38min→6min), `bge-large-ft` LoRA preset.

## Schema state

- **v27** (post-#1497, v1.38.0+) — `chunks.needs_embedding INTEGER NOT NULL DEFAULT 0` plus partial index. Drives `--llm-summaries` skip-first-pass embed: chunks land with zero-vec sentinel + `needs_embedding=1`; HNSW build and search hide them until `enrichment_pass` clears the flag.
- v27 migration backfills `needs_embedding=1` for any pre-v27 row with `embedding_base IS NULL` so legacy chunks repopulate the base-HNSW on the next index pass.
- HNSW build, `Store::search_by_name`, `Store::search_fts_only` all filter `WHERE needs_embedding = 0`.

## Adding a top-level CLI command (post-#1495)

Declare the variant with `#[cqs_cmd(group = "a"|"b", batch = "cli"|"daemon"|"runtime")]` on `Commands` (definitions.rs), implement the handler in `commands/<area>/`, add a small `cmd_<snake>_dispatch` shim in `commands/dispatch_shims.rs`. The shim destructures the variant out of `&Commands` and forwards to the handler. Cfg-gated variants get `#[cfg(feature = "...")]` next to `#[cqs_cmd(...)]` and the derive forwards it to every emitted arm.

## Operational pitfalls (rolling forward)

- **Main is protected** — `git push` to main is rejected. Always create a branch + PR.
- **Always use `--body-file` for `gh pr create`** — never inline heredocs (PowerShell mangles + Claude Code captures the whole multiline as a permission entry, corrupting `settings.local.json`).
- **WSL git credential helper** — `git push` from `~/training-data` needs `git config --global credential.helper '/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe'`. Already configured globally for cqs.
- **Squash-merge + rebase trap** — when a PR is squash-merged and a follow-up branch was based off it, rebase fails. Cherry-pick onto fresh main.
- **Auto-merge disabled** — `gh pr merge --auto` returns "auto merge is not allowed". Watch CI manually + merge when green.
- **`cargo publish --features gpu-index` fails verification** — the workspace `[patch.crates-io]` cuvs-patched fork doesn't ship in the package. Use plain `cargo publish` (no features); gpu-index is feature-gated.
- **cqs-macros must publish first** — when bumping cqs that depends on cqs-macros, publish cqs-macros to crates.io first or `cargo publish -p cqs` errors with "no matching package named cqs-macros".
- **`cargo publish` 413 errors** = excludes missing. `evals/` etc. are in `Cargo.toml`'s `exclude` list.
- **`enumerate_files` returns relative paths** — joining with project root before `parse_file()` is mandatory; otherwise the parser resolves against cargo's CWD.
- **`type_edges` parser tracks signature-level uses only** — params, returns, fields. Not expression-level (`let x = T::new()`). Test assertions on "who uses type T?" must check signature users.
- **Daemon GPU "activity" is misleading** — ORT keeps the CUDA context warm; A6000 sits at P2/1800MHz/84W with 0 actual compute work. True idle (P8) requires stopping the daemon.
- **CI cqs test job runs ~6-12 min** post-#1288/#1302 (was 38 min). Fixed-interval `/loop` heartbeats > 60min should go to cloud schedule (`/schedule`).
- **HF preset tokenizers may ship `truncation: {max_length: 512}` baked into `tokenizer.json`** — affects bge-large-ft, v9-200k, coderank. Cqs windowing/counting must clone-and-disable truncation before counting tokens. See PR #1384. When adding a new preset, check `python -c "from tokenizers import Tokenizer; print(Tokenizer.from_file('tokenizer.json').get_truncation())"` first.
- **Triage-flip durability** (audit-cycle lesson): force-pushed rebases naively resolve triage-row conflicts using older agents' pre-flip snapshots. Mitigation: keep triage flips append-only OR move each PR's triage update into a separate narrow PR per cluster.

## Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions.
5. **Don't suggest ending a session.** 1M context, plenty of headroom, user works continuously.

## Eval baselines

Canonical slate: `evals/queries/v3_test.v2.json` (109q) + `evals/queries/v3_dev.v2.json` (109q). Both fixtures refreshed 2026-04-25 (PR #1109).

**Baseline (v3.v2 218q dual-judge, 2026-05-08 post-v1.39.1 cliff fix + LLM summaries refresh + identifier_lookup α retune):**

| Metric | Test | Dev | Agg |
|---|---:|---:|---:|
| R@1 | 40.4% | 52.3% | 46.3% |
| R@5 | 71.6% | 78.0% | 74.8% |
| R@20 | 81.7% | 90.8% | 86.2% |

Per-category R@5 (post-retune): identifier_lookup 91.7% (was 86.7% pre-retune; agg n=36), multi_step 92.9%, negation 81.8%, type_filtered 69.2%, behavioral_search 65.6%, cross_language 63.6%, structural_search 62.5%, conceptual_search 56.0%.

Numbers below the 2026-05-03 capture (44/55 R@1 → 40.4/52.3, 67.9/78.0 R@5 → 71.6/78.0) reflect: (a) corpus drift since 2026-05-03 (13,359 → 14,203 chunks; eval matches `(file, name, line_start)` strict so audit-cycle line shifts silently turn hits into misses — see feedback memory "Eval Line-Start Drift"); (b) the cliff fix and α retune are strict improvements on the current corpus state. Refreshing the v3.v2 fixture line numbers would lift agg R@K back into the v1.36-snapshot range without changing retrieval quality. v4 fixtures (1526/split, 14× v3 N) exist for any A/B that needs tighter noise floors.

**Strategic frontier candidates** (when redirected): wire USearch / SIMD brute-force as `IndexBackend` candidates (#1131 trait scaffolding already in); HyDE on v3 dev with index-time per-category routing (never properly tested at v3 N); knowledge-augmented retrieval (call/type graph as structured filter; multi_step queries weakest at 28-43% R@1); expand v3 → v4 fixture scale (1526q/split — current 109q is data-bound for per-category sweeps).

**Reranker V2 closed** — 2026-05-07 re-eval against post-v1.39.0 stack confirmed all four reranker variants (off-the-shelf MiniLM + 3 in-domain UniXcoder retrains) remain net-negative on v3.v2 (test R@5 -10 to -16pp, dev R@5 -16 to -26pp). Gap actually widened on dev as stage-1 strengthened. R@20 within 1-4pp of baseline across all four — gold is in the pool; every reranker demotes it. Bottleneck is fixture-size (109q × 30 candidates too thin for 125M cross-encoder) + stage-1 already strong; not a tunable knob. Future revisit gated on v4-scale labelled fixture OR a 5× bigger base (bge-reranker-large at ~3× latency). README now documents the regression at v1.39.0.
