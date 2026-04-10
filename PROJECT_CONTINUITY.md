# Project Continuity

## Right Now

**Phase 5 dual embeddings draft PR #876 up. Eval running. (2026-04-10 11:01 CDT)**

### Recent wins
- v1.21.0 + v1.22.0 released
- v1.22.0: adaptive retrieval v1 (classifier, routing, SPLADE pre-pooled, telemetry)
- SPLADE v2: NULL. SPLADE-Code 0.6B (Naver): **+1.2pp R@1, +20pp cross-language**
- Full audit cleared, chunk types in 19 languages, dependabots merged
- PR #874 merged (eval script field name + --config flag fix)
- PR #875 merged (cuvs pinned to =26.2 — CUDA 13 incompatibility with 26.4)

### Phase 5 status (PR #876, draft)
- Schema v17→v18 done, embedding_base column wired through pipeline
- Dual HNSW build (`index_base.hnsw.*`) and DenseBase router strategy live
- 14 new tests, 1316 lib tests pass total
- Smoke test: behavioral/conceptual queries route to base index, structural stays on enriched
- Eval running (~165 q × ~12s/q ≈ 33min); waiting on results
- Hook drive-by: replaced verbose pre-edit-context.py with focused pre-edit-impact.py (function-targeted, no jq dep)

### What's next
1. Eval results → flip PR #876 from draft → merge
2. Paper polish with SPLADE-Code + dual embedding results
3. Phase 6: explainable search (depends on SPLADE-Code integration)

## Open Issues
- #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0, Tests: 1316 lib (+5 Phase 5), Chunk types: 29
- Adaptive retrieval Phases 1-5 implemented (5 in flight on PR #876)
- Schema: v18 (embedding_base column for dual HNSW indexes)
- SPLADE-Code 0.6B: +1.2pp R@1, +20pp cross-language
- Two HNSW indexes per project: enriched (`index.hnsw.*`) + base (`index_base.hnsw.*`)
