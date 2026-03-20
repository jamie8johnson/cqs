# Project Continuity

## Right Now

**LoRA v4 training in progress (2026-03-20).** 200k CodeSearchNet pairs, 3 epochs, 5e-6 LR. Background task ID: `bx12j7xea`. ETA ~2 hours from ~10:30pm. Output at `~/training-data/e5-code-search-lora-v4/`.

### When training completes
1. Run CoIR CSN: `cd ~/training-data && python run_coir.py --model lora-v4 --task codesearchnet` (need to add v4 to run_coir.py — path is `~/training-data/e5-code-search-lora-v4/merged_model`)
2. Run CoIR cosqa transfer test: `--model lora-v4 --task cosqa`
3. Compare v3 vs v4 — expect significant improvement (4x data, 3x epochs)
4. Update research log with v4 results
5. Consider running reranker configs on CoIR

### CoIR Results So Far

| Config | CSN Avg NDCG@10 | cosqa NDCG@10 |
|--------|----------------|---------------|
| Base E5-base-v2 | 0.627 | 0.329 |
| E5 + NL enrichment | 0.626 (flat) | — |
| **E5 + LoRA v3** | **0.671 (+4.3pp)** | **0.334 (+0.5pp)** |
| E5 + LoRA v4 | TRAINING... | — |

**Key finding:** LoRA that regressed -16pp on hard eval gives +4.3pp on CoIR. Eval regime hypothesis confirmed.

### Done this session (2026-03-20)
- PR #628: atomic_write race condition fix
- PR #629: SQ-10 reranker eval harness + configurable model
- PR #630: Weight sweep (Exp 6) + SQ-11 type-aware embeddings (Exp 7, first positive)
- PR #631: SQ-12 index-time HyDE query predictions
- Hard eval Exp 8: hyde on balanced corpus (mixed result: TS +0.087, R@1 -5.4pp)
- Stress eval: RRF + call graph enrichment + hyde (Rust MRR still 0.037)
- CoIR benchmark: base E5, LoRA v3, NL enrichment, cosqa transfer test
- Research log updated through CoIR results + eval regime hypothesis

### Files at ~/training-data/
- `run_coir.py` — CoIR benchmark adapter script
- `coir-results/e5-base-v2/` — base E5 results (CSN + cosqa)
- `coir-results/e5-lora-v3/` — LoRA v3 results (CSN + cosqa)
- `coir-results/e5-nl-enriched/` — NL enrichment results (CSN)
- `RESEARCH_LOG.md` — full experiment log (8 experiments + CoIR)

## Parked

- **SQ-7 LoRA on hard eval:** 3 experiments all regressed. Accepted trade-off — LoRA is for CoIR, not hard eval.
- **SQ-10 Reranking on CoIR:** Untested. May help as top-K reranker on CoIR tasks.
- **SQ-3:** Code-specific base model
- **Post-index name matching** — fuzzy cross-doc references
- **v1.1.0 release** — needs docs review, dimension refs, CHANGELOG
- **CoIR full 10-task run** — need all tasks for leaderboard comparison (currently only CSN + cosqa)
- **Paper writing** — eval regime hypothesis is the core story

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00
- cuVS PR #1840 (CAGRA serialize): open
- Audit cuVS + ort: planned

## Architecture

- Version: 1.1.0
- Schema: v16 (composite PK on llm_summaries — supports summary, doc_comment, hyde purposes)
- Embeddings: 768-dim E5-base-v2 + type-aware signatures (SQ-11)
- LLM enrichment: summaries (SQ-6), doc comments (SQ-8), hyde predictions (SQ-12)
- Tests: 1265 lib pass (with gpu-index)
- Training env: conda cqs-train, A6000 48GB
- Training data: ~/training-data/ (CodeSearchNet 1.7M, docstring 7.5k, LoRA v1-v4, reranker)
- Research log: ~/training-data/RESEARCH_LOG.md
- CoIR results: ~/training-data/coir-results/
- HuggingFace: jamie8johnson/code-reranker-v1 (ONNX)
