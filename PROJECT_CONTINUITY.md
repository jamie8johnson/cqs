# Project Continuity

## Right Now

**v1.22.0 shipped + PR #874 merged. Next: Phase 5 dual embeddings. (2026-04-10 09:50 CDT)**

### Recent wins
- v1.21.0 + v1.22.0 released
- v1.22.0: adaptive retrieval v1 (classifier, routing, SPLADE pre-pooled, telemetry)
- SPLADE v2: NULL. SPLADE-Code 0.6B (Naver): **+1.2pp R@1, +20pp cross-language**
- Full audit cleared, chunk types in 19 languages, dependabots merged
- PR #874 merged (eval script field name + --config flag fix)

### What's next
1. Phase 5: dual embeddings (v2) — schema v17→v18, est +5-10pp
2. Paper polish with SPLADE-Code + adaptive retrieval results
3. Phase 6: explainable search (depends on SPLADE-Code integration)

## Open Issues
- #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0, Tests: ~2468, Chunk types: 29
- Adaptive retrieval Phases 1-4 shipped
- SPLADE-Code 0.6B: +1.2pp R@1, +20pp cross-language
