# Project Continuity

## Right Now

**LoRA eval complete. Ready to commit/PR/merge, then queue v5 overnight (2026-03-20).**

### Key Finding: v4 Over-Specializes
- v4 (200k/3ep): CSN +6.8pp but cosqa **-2.5pp** (transfer regression)
- v3 (50k/1ep): CSN +4.3pp AND cosqa +0.5pp (helps both)
- **Production choice: v3.** Light LoRA that doesn't over-specialize.
- v4/v5 are for paper benchmarks only.

### CoIR Results (all configs)

| Config | CSN NDCG@10 | cosqa NDCG@10 | Production? |
|--------|------------|---------------|-------------|
| Base E5 | 0.627 | 0.329 | Current default |
| **v3 (50k/1ep)** | **0.671** | **0.334** | **Ship as default** |
| v4 (200k/3ep) | 0.695 | 0.304 | Paper only |

### Before v5 training
1. Commit + PR + merge docs/research updates
2. Queue v5: 1.7M CSN, 1 epoch, ~5.5 hrs overnight, checkpoint each epoch

### Done this session (2026-03-20)
- PRs #628-632 merged
- 8 hard eval experiments + stress eval
- CoIR: base, v3, v4 on CSN + cosqa transfer tests
- LoRA v4 trained (200k/3ep) — over-specializes on CSN
- Fixed run_coir.py output clobbering
- Research log fully updated

### Production Stack
1. Type-aware signatures (SQ-11) — shipped, free
2. Call graph enrichment (SQ-4) — shipped, free
3. LLM summaries (SQ-6) — shipped, optional
4. **LoRA v3 — ship as default** (light touch, helps everywhere)
5. Hyde predictions (SQ-12) — shipped, optional

## Parked

- **v1.1.0 release** — after LoRA ships
- **v5 training** — 1.7M/1ep overnight, for paper CSN numbers
- **Mixed LoRA** — train on CSN + cosqa + SO for production-quality generalist adapter
- **Full 10-task CoIR** — for leaderboard avg
- **Post-index name matching** — fuzzy cross-doc references

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00
- cuVS PR #1840 (CAGRA serialize): open

## Architecture

- Version: 1.1.0
- Schema: v16
- Embeddings: 768-dim E5-base-v2 + signatures (SQ-11). LoRA v3 shipping as default.
- LLM: summaries (SQ-6), doc comments (SQ-8), hyde (SQ-12)
- Tests: 1265 lib pass
- Training: ~/training-data/ (CSN 1.7M, LoRA v1-v4, CoIR results)
- Research log: ~/training-data/RESEARCH_LOG.md
