# Project Continuity

## Right Now

**v1.5.0 released (2026-03-25). Exp 18 (Stack + call-graph filter) in progress.**

### v9-mini Pipeline (Exp 18)
- **Pivot**: training data from CSN → The Stack (full repos with call graphs)
- **Novel signals**: call-graph false-negative filtering + synthetic multi-style queries
- **Status**: 1,350 repos selected (150/lang × 9 langs), cloning in progress
- **Pipeline**: select → clone → `cqs index` → extract pairs → mine hard negs (with CG filter) → gen synthetic queries → assemble → train
- **Scripts**: `select_and_clone_repos.py`, `index_stack_repos.py`, `gen_synthetic_queries.py`, `assemble_v9_mini.py`
- **Budget**: 100k samples, 4-6h training, ~$2 API for synthetic queries
- **Success bar**: hard eval ≥ 92.7% AND CSN ≥ 0.627 (must beat base on both)

### v1.5.0 Release
- Default model switched to base E5 (enrichment stack does the heavy lifting)
- v8 CoIR: 43.14 overall (below base 49.47 — KeyDAC was net negative)
- CI fix: hermetic env vars in llm_config test
- ORT download timeout on CI (transient, retried)

## Open Issues
- #665, #666, #389, #255, #106, #63

## Architecture
- Version: 1.5.0
- Current shipping model: base E5 (intfloat/e5-base-v2, 92.7% hard eval, 0.627 CSN)
- Best hard eval: v8-keydac (92.7% R@1, 0.652 CSN) = base E5
- Best CSN: v7 (0.707)
- Full-pipeline: 96.3% R@1 (v8 + HyDE + contrastive summaries)
- Paper: ~/training-data/paper/draft.md (v0.3)
- Training repo: github.com/jamie8johnson/cqs-training
