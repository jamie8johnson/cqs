# Project Continuity

## Right Now

**Full 1.7M CSN + docs / 1ep training (2026-03-20).** Background task: `b4ijaon3b`. ~5.5 hours.

### Rank sweep result: flat
Rank 32 = rank 16 within noise. Bottleneck is data, not model capacity.

### Complete Results Matrix (all rank 16, 1ep unless noted)

| Config | CSN NDCG@10 | CosQA NDCG@10 | Notes |
|--------|------------|---------------|-------|
| Base E5 | 0.627 | 0.329 | — |
| 10k+docs | 0.671 | 0.327 | |
| 50k+docs (v3) | 0.671 | 0.334 | |
| 75k+docs | 0.675 | 0.341 | |
| 200k+docs | 0.680 | 0.353 | Best at 200k |
| 200k rank 32 | 0.682 | 0.351 | Rank doesn't help |
| 200k no docs (v5) | 0.683 | 0.348 | Docs help CosQA |
| 200k 3ep no docs (v4) | 0.695 | 0.304 | More epochs kills CosQA |
| 73k mixed (v6) | 0.644 | 0.332 | Mixed data dilutes signal |
| **1.7M+docs/1ep** | **TRAINING** | | **~5.5 hrs** |

### After training
1. Eval on CSN + CosQA
2. If good: try 2ep, then 3ep
3. Ship best model
4. Discriminating descriptions experiment ($0.10)
5. Custom training data from popular repos

### Key learnings
- Hard eval penalizes any training — not useful for production decisions
- CSN + CosQA are the metrics that matter
- More data monotonically improves real metrics
- Rank 16 is sufficient — data is the bottleneck
- Docstrings anchor generalization (help CosQA)
- CSN data is clean (filtering removed nothing)
- CodeSage-large-v2 fails NL queries (20% R@1)

## Parked

- v1.1.0 release — after LoRA ships
- Full 10-task CoIR — for paper
- Custom training data from popular repos
- Discriminating LLM descriptions

## Architecture

- Version: 1.1.0, Schema: v16
- Embeddings: 768-dim E5-base-v2 + signatures (SQ-11)
- LLM: summaries (SQ-6), doc comments (SQ-8), hyde (SQ-12)
- Tests: 1265 lib pass
