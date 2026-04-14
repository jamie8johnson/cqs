# Project Continuity

## Right Now

**v1.25.0 staged. Classifier is the bottleneck, not alphas. (2026-04-14 ~12:35 CDT)**

### Where we landed today

Three PRs on top of v1.24.0:

1. **#943 merged** — `run_ablation.py` now writes eval results to `~/.cache/cqs/evals/` instead of `evals/runs/*/results.json`. Fixes the watch-reindex contamination that corrupted every prior alpha measurement.
2. **Clean 21-point alpha re-sweep** — first truly deterministic sweep. Back-to-back runs bit-exact.
3. **New per-category defaults** in `resolve_splade_alpha()` — structural 0.60 (was 0.9, wrong), conceptual 0.85 (was 0.95), identifier 0.90 (was 1.0), behavioral 0.05 (confirmed), rest 1.0 (confirmed).
4. **Router fix** — dropped `query.contains("how does")` from `is_behavioral_query`. That pattern caught 100% of multi_step eval queries and routed them to α=0.05. Now multi_step falls to MultiStep/Unknown (both α=1.0). +0.7pp overall.

### Numbers

- Best uniform α from clean sweep: **α=0.95 → 44.9%** (not α=1.0 — that was a corruption artifact)
- Per-category oracle ceiling: **49.4%** (131/265)
- Deployed per-category routing after fixes: **44.9%** — ties uniform α=0.95
- The 4.5pp oracle gap is **entirely classifier accuracy**, not alpha choice

### The classifier is the bottleneck

Confusion matrix (eval label vs `classify_query()` output):

| eval_label | N | correctly classified |
|---|---|---|
| negation | 29 | 100% |
| identifier | 50 | 84% |
| structural | 27 | 19% |
| type_filtered | 24 | 4% |
| behavioral | 44 | 5% |
| conceptual | 36 | 3% |
| cross_language | 21 | 0% |
| multi_step | 34 | 0% → fixed today via "how does" removal |

Structural/conceptual/behavioral detectors rely on narrow phrase and word lists that miss most natural-language queries. Those queries fall to Unknown → α=1.0. type_filtered queries starting with "struct"/"enum"/"trait" hit the Structural rule first. Cross-language detection requires explicit language names.

Classifier investigation is the next high-value CPU-lane item. Added to ROADMAP.

### Next session priorities

1. Ship v1.25.0: commit router changes, new defaults, bump version, changelog, release.
2. Classifier accuracy investigation — expand rule set / learned classifier / LLM-first-query-cached. Worth +4.5pp if done well.
3. Eval expansion: grow small categories (N=21 cross_language, N=24 type_filtered) to N≥40.
4. Rename `v2_300q.json` to actual count (265).

### Residual puzzles

- Identifier dropped 1 query (98% → 96%) and structural dropped 1 query (51.9% → 48.1%) between v1 and v2 eval today, with only the `is_behavioral_query` change between them. Likely SPLADE ONNX GPU non-determinism on the sparse vector output — the previously-noted residual.

## PR status
- #939, #940, #941, #942, #943 all merged
- Router + defaults changes: uncommitted on local main (needs branch + PR for v1.25.0)

## Architecture
- Version: 1.24.0 (1.25.0 staged), Schema: v20
- Deterministic search path (PR #942) + deterministic eval pipeline (PR #943)
- SPLADE always-on, alpha controls fusion weight only
- Per-category defaults (staged for v1.25.0): identifier 0.90, structural 0.60, conceptual 0.85, type_filtered 1.0, behavioral 0.05, rest 1.0
- HNSW dirty flag self-heals via checksum verification
- cuVS 26.4 + patched with search_with_filter (upstream rapidsai/cuvs#2019)
- Eval results write to `~/.cache/cqs/evals/` (outside watched project dir)

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63
