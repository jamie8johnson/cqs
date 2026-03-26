# Project Continuity

## Right Now

**v9-mini results in. Assembling balanced v9-200k dataset. BGE-large plan ready. (2026-03-26)**

### v9-mini Results (Exp 18)
- Raw R@1: **65.5%** (best ever, +16.4pp over base 49.1%)
- Enriched R@1: **89.1%** (matches base — both 89.1% on this GPU, 92.7% was within noise)
- CSN: **0.638** (PASS, +0.011 over base 0.627)
- **Call-graph filter + Stack data worked** — first LoRA that improves raw embedding without degrading enriched eval
- The 92.7% figure was GPU non-determinism — base E5 also shows 89.1% on re-run

### Active
- **v9-200k dataset assembly**: Cloning 1,500 repos for C++/TS/Ruby balance gaps. Need ~52K more pairs to reach 22K/lang for all 9 languages.
- **BGE-large configurable models plan**: 4 revisions, 3 reviews, ready to execute after v9-200k.

### Pending
1. Clone + index C++/TS/Ruby gap repos
2. Merge new pairs into v9_merged_pairs.jsonl
3. Re-mine hard negatives on 200K set
4. Assemble balanced 200K (22K/lang × 9)
5. Train v9-200k
6. Execute BGE-large plan (8 tasks)
7. v1.7.0 release

## Parked
- Paper v0.6 (needs v9-200k + BGE-large results)
- Curriculum scheduling (v9-full, if v9-200k shows scale helps)

## Open Issues
- #389, #255, #106, #63 (all blocked on upstream)

## Architecture
- Version: 1.6.0
- Model: E5-base-v2 (768-dim). v9-mini LoRA trained (65.5% raw, 89.1% enriched, 0.638 CSN).
- Enrichment: contributes ~40pp (raw ~49-65% → enriched ~89%)
- Languages: 51 (28 with FieldStyle)
- Tests: ~1993
- Training: ~/training-data (github.com/jamie8johnson/cqs-training)
- Conda: transformers 5.3.0, torch 2.11.0, faiss-gpu 1.13.2
