# Project Continuity

## Right Now

**v7 CoIR complete (2026-03-22). Mixed result: +2.4pp CSN, -5.5pp hard eval. Specialization trade-off partially resolved.**

### v7 Results (complete)
- Training: 200k subsample, GIST + Matryoshka + hard negs, 1ep, 6h53m on A6000
- **CoIR overall: 49.19** (v5: 48.67, base: 49.48) — +0.52pp vs v5, only -0.29pp vs base
- CoIR CSN: **0.707** (v5: 0.683, base: 0.627) — +2.4pp vs v5, best ever
- CoIR CCR: 0.508 (v5: 0.490, base: 0.569) — partially recovered (+1.8pp vs v5)
- v7 wins 6/9 CoIR tasks, loses 3
- Hard eval R@1: **81.8%** (v5: 85.5%, base: 87.3%) — worse on adversarial pairs
- **Key insight:** GIST + hard negs fix generalist degradation on realistic benchmarks but not adversarial confusable-function pairs

### Hard eval now supports local LoRA models
- Modified `tests/model_eval.rs`: `EvalEmbedder` resolves local paths, `local_lora_models()` auto-discovers v5/v7

### Decision: ship v7 or stay on v5?
- For cqs product (NL→code only): v7 is strictly better (0.707 vs 0.683 CSN)
- For generalist: v7 nearly matches base (49.19 vs 49.48)
- Hard eval regression means confusable functions (sorting variants, validators) get worse
- v5 is safer; v7 is better for the primary use case

### Still possible: v7b balanced training
- 46k/lang × 9 = 414k total — may further improve by fixing language imbalance
- Language imbalance (82% in 3 langs) likely still dragging down results

## Session accomplishments
- v1.3.0 released + 75 audit fixes (PRs #640-653)
- Full 10-task CoIR controlled comparison: base 49.48, v5 48.67, v7 49.19
- 9-language training data pipeline (extract → filter → mine)
- Literature survey + paper draft v0.1
- Novel ideas: call-graph false neg filtering, test-derived queries
- All scripts quality-reviewed, backed up to github.com/jamie8johnson/cqs-training
- v7 trained + evaluated — CoIR improved, hard eval degraded
- Hard eval supports local LoRA models

## Parked
- v7b balanced (46k/lang × 9 = 414k) — if imbalance is the remaining issue
- Synthetic query augmentation, structural metadata — v8
- Call-graph enriched training data — after balanced training proves concept
- Language-specific LoRA adapters — if balanced training also fails
- Paper revision — after shipping decision
- Rebuild cqs binary + reindex + re-run --improve-all

## Architecture
- Version: 1.3.1, Schema: v16
- Current model: LoRA v7 (200k 9-lang, GIST+Matryoshka, 0.707 CSN, 49.19 CoIR)
- Hard eval: supports local LoRA models (tests/model_eval.rs)
- Metrics: 92.7% R@1, 0.965 NDCG@10 (hard eval, DocFirst, with v5)
- Tests: 1290 lib pass
- Telemetry: CQS_TELEMETRY=1
