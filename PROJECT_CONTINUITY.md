# Project Continuity

## Right Now

**Phase 5 + summary expansion shipped. Routing null at N=27. SPLADE-Code 0.6B re-eval blocked on encoding perf. (2026-04-10 19:00 CDT)**

### This session shipped (10 PRs to main)
- #876 Phase 5 dual embeddings + DenseBase routing
- #877 CQS_DISABLE_BASE_INDEX env var (eval A/B)
- #878 summary eligibility expanded from is_callable() → is_code()
- #879 roadmap updates (selective SPLADE routing tracked)
- #880 bypass test coverage
- #881 CQS_SPLADE_MODEL env var + vocab-mismatch probe
- #882 CQS_TYPE_BOOST env var (sweep infra)
- #883 evals/run_sweep.py harness
- #884 SPLADE vocab probe accepts benign lm_head padding
- #885 routing fix: conceptual back to enriched (later showed as null)
- #886 real batched encode_batch (5-10x speedup ceiling)
- #887 (open) CQS_SPLADE_BATCH env var + adaptive halving

### Eval matrix findings (50% coverage, BGE-large)
- Phase 5 dual-routing: **null at N=27 per category** — all category swings within ±1 query
- Total R@1: 43.0% with or without routing (within noise)
- The "−3.7pp on conceptual from routing" earlier in the day was a misread (one query out of 27 = 3.7pp)
- The historical research finding "summaries hurt conceptual −15pp" was for a different corpus shape

### Pending blockers
- **SPLADE-Code 0.6B encoding** (task #16) — encoder leaks GPU memory (7.4 → 30GB over 1h) with no progress regardless of batch size. Likely needs ORT arena reset / IO bindings / per-batch session refresh. Until fixed, the SPLADE-Code 0.6B re-eval (the only experiment with above-noise predicted effect) is blocked.

### What's next
1. Debug the SPLADE encoding GPU memory leak (task #16) — biggest unblock
2. Once unblocked: re-run the 4-cell matrix with proper SPLADE-Code 0.6B
3. Decide what ships in v1.23.0 based on real SPLADE-Code 0.6B numbers
4. Larger eval set (165q is too small to discriminate ±3pp effects)

## Open Issues
- #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v18 (embedding_base column for dual HNSW)
- Tests: 1330 lib pass
- Adaptive retrieval Phases 1-5 implemented
- Two HNSW indexes per project: enriched (`index.hnsw.*`) + base (`index_base.hnsw.*`)
- SPLADE-Code 0.6B model files at `~/training-data/splade-code-naver/onnx/`
  - Set `CQS_SPLADE_MODEL` env var to use it (vocab probe verifies tokenizer/model match)
  - Encoding currently broken (task #16)
