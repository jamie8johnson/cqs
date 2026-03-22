# Project Continuity

## Right Now

**v7 trained and evaluated (2026-03-22). Result: degraded. Planning v7b balanced.**

### v7 Results (complete)
- Training: 200k subsample, GIST + Matryoshka + hard negs, 1ep, 6h53m on A6000
- Hard eval R@1: **81.8%** (v5: 85.5%, base: 87.3%) — worse than both
- Hard eval MRR: 0.875 (v5: 0.908, base: 0.927)
- CoIR Go NDCG@10: 0.785 (v5: 0.793, base: 0.780) — roughly flat
- CoIR full 10-task: **running**
- Language imbalance likely cause: PHP/Java/Python at 82%, Stack langs at 9%

### Hard eval now supports local LoRA models
- Modified `tests/model_eval.rs`: `EvalEmbedder` resolves local paths, `local_lora_models()` auto-discovers v5/v7

### Next: v7b balanced training
- 46k/lang × 9 = 414k total (Ruby limits the floor)
- Tests whether equal language representation fixes degradation
- No new data needed — subsample from existing `combined_9lang_hard_negs.jsonl`
- If balanced also fails: try language-specific LoRA adapters (LoRACode approach)

## Session accomplishments
- v1.3.0 released + 75 audit fixes (PRs #640-652)
- Full 10-task CoIR controlled comparison: base 49.47, v5 48.67
- 9-language training data pipeline (extract → filter → mine)
- Literature survey + paper draft v0.1
- Novel ideas: call-graph false neg filtering, test-derived queries
- All scripts quality-reviewed, backed up to github.com/jamie8johnson/cqs-training
- v7 trained + evaluated (degraded — language imbalance)
- Hard eval supports local LoRA models

## Parked
- Synthetic query augmentation, structural metadata — v8
- Call-graph enriched training data — after balanced training proves concept
- Language-specific LoRA adapters — if balanced training also fails
- Paper revision — after v7b results
- Rebuild cqs binary + reindex + re-run --improve-all

## Architecture
- Version: 1.3.0, Schema: v16
- Current model: LoRA v5 (166k/1ep, 0.683 CSN, 48.67 CoIR) — still best
- v7 (200k/GIST+Matryoshka): R@1 81.8% — degraded, not shipping
- Hard eval: supports local LoRA models (tests/model_eval.rs)
- Metrics: 92.7% R@1, 0.965 NDCG@10 (hard eval, DocFirst)
- Tests: 1290 lib pass
- Telemetry: CQS_TELEMETRY=1
