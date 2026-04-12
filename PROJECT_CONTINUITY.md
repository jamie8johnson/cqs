# Project Continuity

## Right Now

**SPLADE-Code 0.6B re-eval run to completion with persisted SpladeIndex. Flag-driven SPLADE is −0.6pp R@1 net — selective routing is now mandatory. (2026-04-11 14:15 CDT)**

### SPLADE-Code 0.6B re-eval result (2026-04-11, 165q v2 eval, threshold 1.6)

| Config | R@1 | R@5 | R@20 | N |
|--------|-----|-----|------|---|
| BGE-large | 42.4% | 67.9% | 85.5% | 165 |
| BGE-large + SPLADE-Code 0.6B | 41.8% | 66.1% | 86.1% | 165 |

**Flag-driven SPLADE on every query: −0.6pp R@1 net, reverses the 2026-04-09 +1.2pp headline.**

Per-category deltas (same-corpus baseline vs +SPLADE):
- **cross_language +10pp** (30 → 40%, N=10) — only category where SPLADE pays off, same direction as prior +20pp
- conceptual_search −3.7pp (22.2 → 18.5%, N=27)
- multi_step −4.6pp (36.4 → 31.8%, N=22)
- identifier_lookup, behavioral, negation, structural, type_filtered: unchanged

R@5 damage is bigger: cross_language +20pp, conceptual −7.4pp, type_filtered −6.2pp, negation −5.6pp. SPLADE displaces good dense hits at positions 2-5 on categories where lexical expansion isn't the missing signal.

**Conclusion**: Selective SPLADE routing (roadmap CPU lane) is now required, not optional. Route `CrossLanguage` → `DenseWithSplade`, leave every other category on dense. Predicted outcome: cross_language +10pp stays, conceptual/multi_step noise disappears, total 41.8% → ~43.0% (net **+1.2pp** vs always-on, **+0.6pp** vs baseline).

Research writeup: `~/training-data/research/sparse.md` § SPLADE-Code 0.6B Eval Re-run (FLAG-DRIVEN IS NET LOSS).

### Session unblock chain (three layered blockers removed)

1. **`PRAGMA integrity_check(1)` on every `Store::open`** — 85s per CLI invocation on 1.1 GB DB over WSL `/mnt/c`. Every `cqs search` paid it, eval harness was unusable. Shipped in #893: skip on read-only opens, quick_check on write opens. **86s → 6.9s per query.**
2. **`run_ablation.py` passed query as first positional** — single-token queries parsed as unknown subcommands. Shipped in #894: `cqs --json -n 20 -- <query>` form, `CQS_EVAL_TIMEOUT_SECS` env override, per-query timeout handling.
3. **SpladeIndex rebuilt from SQLite on every CLI invocation** — ~45s at 7.58M rows. Shipped in this PR: persist-alongside-HNSW pattern with generation counter and blake3 body checksum. 46.8s cold → 9.7s warm per SPLADE query.

Combined: full 2×165 ablation matrix now runs in ~55 min instead of the 4+ h the naive implementation would have taken.

### Session PRs (all merged)
- **#893** fix: integrity check skip on read-only opens (86 s → 6.9 s per query)
- **#894** fix: eval harness query separator + per-query timeout handling
- **#895** perf: persist SpladeIndex on disk (45 s → 9.7 s per SPLADE query)
- **#896** docs: tighten OpenRCT2 dual-trail spec — remove off-ramps, merit defense, time estimates

### Next (not yet in progress)
1. **Selective SPLADE routing** — CPU lane item, now required after the re-eval finding. `classify_query` → `DenseWithSplade` only for `CrossLanguage`. Predicted +1.2pp R@1 vs always-on.
2. **`PRAGMA quick_check` on write opens is still 40 s** — opt-in via `CQS_INTEGRITY_CHECK=1` rather than the current opt-out. Corruption detection on a rebuildable index doesn't justify the per-write cost. Tracked in the CPU lane.

## Open Issues
- #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v18 (embedding_base column for dual HNSW) + `metadata.splade_generation` counter added with the persistence work in #895
- Tests: 1345 lib pass (+6 from #895 SPLADE persistence)
- Adaptive retrieval Phases 1–5 implemented
- Three on-disk indexes per project: `index.hnsw.*` (enriched) + `index_base.hnsw.*` (base) + `splade.index.bin` (sparse inverted, new in #895)
- SPLADE-Code 0.6B model at `~/training-data/splade-code-naver/onnx/`. Set `CQS_SPLADE_MODEL` env var to use it. Vocab probe verifies tokenizer/model match at construction time. **Use `CQS_SPLADE_THRESHOLD=1.6`** — the default 0.01 activates ~21k tokens/chunk and blows up the DB.
- Env vars: `CQS_DISABLE_BASE_INDEX`, `CQS_SPLADE_MODEL`, `CQS_SPLADE_THRESHOLD`, `CQS_SPLADE_MAX_SEQ`, `CQS_SPLADE_BATCH`, `CQS_SPLADE_RESET_EVERY`, `CQS_TYPE_BOOST`, `CQS_SKIP_INTEGRITY_CHECK`, `CQS_EVAL_TIMEOUT_SECS`
