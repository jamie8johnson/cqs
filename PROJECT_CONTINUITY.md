# Project Continuity

## Right Now

**Session closing 2026-04-16. Alpha sweep + classifier audit complete. Small real win (+1.8pp R@1) shipped; most of the sweep didn't transfer.**

Branch: `chore/post-v1.26.0-tears-vllm-infra`. PR #1010 open, ready to merge after final push. CI last passed clippy+fmt+msrv; test job running.

### Final measurements on v3 test (109 queries, stable across 3 trials)

| Config | R@1 | R@5 | R@20 |
|---|---|---|---|
| v1.26.0 alphas | 40.4% | 64.2% | 80.7% |
| **v1.26.0 + xlang=0.10 (shipped)** | **42.2%** | 64.2% | 78.9% |
| Full v3-swept alphas | 41.3% | 63.3% | 78.9% |

**Shipping only the cross_language α change (1.00 → 0.10).** +1.8pp R@1 over v1.26.0. Small R@20 regression (−1.8pp) is the trade-off.

### What shipped in PR #1010

1. **cqs bug fixes:**
   - RefCell panic in `batch/mod.rs` (try_borrow_mut + deferred retry)
   - Reranker `token_type_ids` zeroed → populate from tokenizer encoding
   - Reranker local-path support in `CQS_RERANKER_MODEL`
2. **Clippy 1.95 compliance** (sort_by_key, checked_div).
3. **One alpha change**: cross_language 1.00 → 0.10.
4. **Centroid classifier infrastructure** (disabled by default, `CQS_CENTROID_CLASSIFIER=1` to enable). Infra includes alpha floor wiring and centroid file at `~/.local/share/cqs/classifier_centroids.v1.json`.
5. **Classifier audit** as integration test (`tests/classifier_audit.rs`).
6. **Eval pipeline** (14 scripts under `evals/`): telemetry mining, chunk-driven generation, pool building, dual-judge validation, consensus merge, alpha sweep, reranker training, centroid training, diagnose, heartbeat.
7. **v3 dataset artifacts** checked in: `v3_all.json`, `v3_train/dev/test.json`, `v3_consensus.json`, `v3_pools.json`, `v3_alpha_sweep.json`, `v3_validated_*.json`.

### What didn't ship (and why)

| Attempt | Why not |
|---|---|
| Centroid classifier (runtime) | −4.6pp R@1; wrong-alpha cost asymmetric |
| Logistic regression (skipped) | Breakeven simulation proved max accuracy still net-negative for Unknown→category routing |
| Reranker V2 | −5.5pp R@1 even fine-tuned. Needs 200k Gemma-labeled pairs + code-pretrained base + RRF fusion to be net-positive. Deferred. |
| Full v3 alpha sweep | Only cross_language transferred through the router; rest was masked by strategy routing |
| Classifier rule fixes (negation idiom guards) | Eliminated 2 misfires but no R@1 change within noise |

### Classifier audit findings (v3 dev)

Rule-based `classify_query` is **38.5% accurate** on v3 dev. 49.5% of queries land in Unknown (rule doesn't fire), 13.8% fire wrong. Per-category health:

- **Perfect**: negation (100%), cross_language (100% precision when fires)
- **Broken**: multi_step (0% correct on 14 queries — "AND" conjunctions get caught by structural first), conceptual_search (0% correct on 12 queries — abstract-noun patterns don't match v3 phrasings)
- **Low precision**: type_filtered (17% — fires, but misclassifies most queries to structural/conceptual)

Fix value is bounded: queries that fall to Unknown get α=1.0, which is often what they want anyway (per breakeven simulation). Fixing classifier misfires gives at best 1-3pp R@1 — not worth brittle pattern additions.

### Session total findings

Five experiments, one shipped improvement:
1. Centroid classifier ❌ (−4.6pp dev R@1)
2. Reranker V2 ❌ (−5.5pp dev R@1)
3. Logistic regression — skipped after breakeven simulation
4. Full v3 alpha sweep ❌ (masked by strategy routing)
5. **Cross-language α 1.00 → 0.10** ✓ (+1.8pp test R@1)

Plus 2 cqs bugs found and fixed as byproducts (RefCell panic, token_type_ids).

### Lessons

- **Simulate end metric before building** (already in memory from this session).
- **Don't trust wins that bypass production path.** The forced-α sweep showed +13.8pp R@1 dev; through the full router only +0.9-1.8pp transferred. Strategy routing absorbs most of what alpha would give.
- **Alpha tuning upper bound is closed.** We measured it (~48% forced-α, ~42% router). Real headroom is behind classifier accuracy, which is itself blocked by the breakeven constraint on Unknown queries.
- **R@1 gains from here require representation work**, not more tuning of the current stack. HyDE, reranker V2 at scale, embedder switch.

## Architecture state

- **Version:** v1.26.0 + post-release fixes (will ship as v1.26.1 or folded into v1.27.0)
- **Local binary:** v3 alphas (v1.26.0 + xlang=0.10) + classifier intact + bug fixes installed
- **Index:** 14,917 chunks, 100% SPLADE coverage
- **Production R@1 baseline on v3 test:** 42.2%
- **Open PRs:** #1010 (session work)
- **Open issues:** 18 (0 tier-1)
- **cqs-watch daemon:** running patched binary

## Operational pitfalls added this session

- **Measurement stability on v3 test:** 3-trial variance is ±1 query (~1pp). Always repeat measurements; single-trial readings can be off by 3-5pp (the "45.0%" that turned out to be 42.2%). The v1.26.0 "baseline" is actually 40.4 ± 0.9pp, not the 44.0% I cited early on.
- **Forced-α measurements mislead.** Bypassing strategy routing inflates measured deltas by 5-10pp vs production. Always validate wins through the full router before shipping.
- **Rust 1.95 adds clippy lints** (unnecessary_sort_by, manual_checked_div) that trigger `-D warnings` failures. Keep toolchain in sync with CI.
- **Rebuilds invalidate daemon state.** After `cp cqs ~/.cargo/bin/` always `systemctl --user restart cqs-watch` or accept that the daemon is running the old binary.

## What's parked

- **HyDE for structural/behavioral queries** — per old data +14pp structural, +12pp type_filtered. Attacks representation. Needs fresh v3 eval. Most promising next lever.
- **Reranker V2 at scale** — Gemma pipeline already built; needs code-pretrained base + RRF fusion to be net-positive.
- **Embedder switch BGE → E5 v9-200k** — v2 measurements said ties-on-R@1 with 1/3 dim. Needs v3 re-measurement.
- **Classifier rule expansion for multi_step/conceptual/type_filtered** — low-risk pattern additions, low expected R@1 impact (+1-3pp ceiling). Worth doing with a larger eval set, not at current N=109.
