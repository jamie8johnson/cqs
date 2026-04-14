# Project Continuity

## Right Now

**Watch-reindex contamination found. All session alpha data is compromised. Re-sweep needed. (2026-04-14 10:45 CDT)**

### The bug we just found

`run_ablation.py` was writing `evals/runs/*/results.json` inside the watched project dir. Watch mode treats `.json` files as code changes → reindexes → rebuilds HNSW → changes `index.db` mtime → next eval's first query invalidates BatchContext caches → fresh state for remaining queries.

Effect:
- Every eval run contaminated the state for the NEXT eval run
- Consecutive identical-config runs drifted by up to ±15pp on small categories
- The "6pp oracle gap" was entirely this artifact
- **The entire 21-point alpha sweep from yesterday is corrupted** — each cell was measured on a progressively-mutated index

Fix (branch `fix/eval-output-location`): write to `~/.cache/cqs/evals/` instead.

Verified: two identical back-to-back runs give **bit-exact** results (structural R@1 = 59.3% both runs).

### The three determinism landmines, summarized
1. **Hash iteration randomness** (fixed in PR #942)
2. **SPLADE disabled at α=1.0** (fixed in PR #942)
3. **Eval output triggers watch-reindex** (fixing now)

All three corrupted measurements in different ways. With all three fixed, we should finally have a reproducible eval.

### What this means

- v1.24.0 defaults shipped per-category alphas tuned on corrupted measurements
- The "real optima" from the re-sweep (structural 0.9, conceptual 0.95, behavioral 0.05) are likely also wrong — they were measured under the same contamination
- Need full re-sweep with the fix in place

### Next session priorities

1. Merge the eval-output-location fix
2. Re-sweep all 21 alphas on truly clean infrastructure
3. Compare: actually-correct per-category optima vs today's (corrupted) guesses
4. Decide real defaults, update `resolve_splade_alpha()`, release v1.25.0

### Residual puzzles

- SPLADE encoder on GPU (CUDA ONNX) may have residual non-determinism in the sparse vector output. Minor compared to everything else; verify post re-sweep.
- cross_language and negation categories drifted 1 query between repeat same-daemon runs yesterday. May be the ONNX issue above.

## PR status
- #939, #940, #941, #942 all merged (v1.24.0)
- `fix/eval-output-location` — about to PR

## Architecture
- Version: 1.24.0, Schema: v20
- Deterministic search path (PR #942)
- SPLADE always-on, alpha controls fusion weight only
- Per-category defaults: identifier 1.0, structural 0.9, conceptual 0.95, type_filtered 1.0, behavioral 0.05, rest 1.0 — ALL UNCERTAIN pending re-sweep on fixed infrastructure
- HNSW dirty flag self-heals via checksum verification
- cuVS 26.4 + patched with search_with_filter (upstream rapidsai/cuvs#2019)
- Eval results now write to `~/.cache/cqs/evals/` (outside watched project dir)

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63
