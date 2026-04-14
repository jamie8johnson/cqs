# Project Continuity

## Right Now

**5-bug audit + re-sweep + SPLADE-always-on fix in flight. (2026-04-13 EOD)**

### Big session findings

The v1.23.0 SPLADE alpha sweep was measured against a broken pipeline. Two bugs:
1. **Hash iteration non-determinism** in search_hybrid (HashSet) + SpladeIndex (HashMap) → ±5pp noise per category per run
2. **SPLADE disabled at α=1.0** → categories with α=1.0 lost the candidate-expansion benefit entirely

Plus 3 smaller bugs (RCA-3/-4/-5 in the audit). All fixed in PR #942.

Re-swept on deterministic pipeline (21 points, 0.0 → 1.0 by 0.05):

- Global optimum uniform α=1.0: **45.3% R@1**
- Per-category oracle: **49.8% R@1** (+4.5pp)
- Pre-session production baseline: 41.5% R@1

Real per-category optima:
- identifier 1.0 (FTS5, alpha inert)
- structural **0.9** (+14.8pp vs 1.0) — was 0.7 in old defaults (miscalibrated 3.7pp)
- conceptual **0.95** (+13.9pp vs 1.0)
- type_filtered 1.0
- behavioral **0.05** (+4.6pp vs 1.0)
- multi_step / negation / cross_language: 1.0

Updated `resolve_splade_alpha()` with real values.

### Open puzzle

Deploying the new per-category defaults gave 43.8%, not the oracle's 49.8%. 6pp gap. Per-category routing apparently has between-query state interaction that uniform sweeps don't expose. Needs investigation next.

### Also fixed this session
- **Dirty flag self-heal** (just written, not yet pushed): daemon startup now verifies HNSW checksums before respecting the dirty flag. If files pass checksum, clear the flag and proceed. Stops the "reindex interrupted → forever stale" problem that bit us repeatedly today.
- **Daemon env var logging**: startup prints CQS_* env vars so it's obvious which config the daemon is running.

### Meta-lesson

Two days of "per-category alpha tuning" was measuring noise. One audit pass + one fix flipped the entire eval narrative. When eval results drift 3-5pp between identical configurations, suspect the measurement apparatus before the feature.

## PR status
- #939 merged (v1.24.0: CAGRA filtering, daemon stability)
- #940 merged (docs cleanup)
- #941 merged (Windows cfg guard)
- #942 open (determinism + SPLADE-always-on + dirty-flag self-heal)

## Next session priorities
1. **Investigate 6pp oracle gap** on per-category routing — between-query state leak
2. **Eval expansion** — N=21 cross_language, N=24 type_filtered are too noisy. Grow each small category to N≥40.
3. **v1.25.0 release** after #942 merges — headline: determinism fixes + real per-category optima
4. **`cqs history` / author-weighted search / auto-notes** — agent continuity features on the roadmap

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63

## Architecture (post-#942)
- Version: 1.24.0, Schema: v20
- Deterministic search results across process invocations (hash iteration + SPLADE + SQL ordering fixes)
- SPLADE always enabled when available (candidate expansion decoupled from fusion weight)
- HNSW dirty flag self-heals via checksum verification on startup
- Per-category SPLADE alpha defaults updated to real optima
- cuVS 26.4 (libcuvs 26.04, conda, CUDA 13), patched with search_with_filter (upstream rapidsai/cuvs#2019)
- LLM summary coverage: 78% of code chunks (6,275 summaries)
