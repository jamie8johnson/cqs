# Project Continuity

## Right Now

**v1.20.0 released. Full audit complete. Clean slate. (2026-04-08 CDT)**

### What happened this session
- v1.18.0: Embedding cache, 5 chunk types, eval harness, query logging
- v1.19.0: --include-type/--exclude-type, Java/C# detection, capture list unification
- v1.20.0: 14-category audit (71 findings, 69 fixed), Elm, SPLADE training (null)
- SPLADE code fine-tuning: completed, null result. Failure mode analyzed (weak reg, wrong vocab, wrong negatives). v2-v4 experiments planned.
- Full ablation: BGE-large + LLM summaries = best config. SPLADE and reranker both null.
- 265-query eval set across 8 categories, ground truth validated
- All branches cleaned up (local + remote)
- GitHub repo description updated

### Tracked issues
- #843: PF-5 wire SPLADE encode_batch (blocked on SPLADE quality)
- #844: AD-19 rename SearchFilter::chunk_types to include_types

### What's next
- SPLADE v2: token-overlap hard negative mining → reg sweep → CodeBERT vocab
- Code-trained reranker (Reranker V2)
- Paper v0.7
- Cross-project call graph
- New chunk types: Extern, Namespace, Middleware, Solidity modifier, Rust impl, CSS @rules
- Remaining 6 audit categories run but findings not yet triaged into P1-P4

## Parked
- Wiki system — spec revised (agent-first)
- Paper v0.7

## Open Issues
- #843 (PF-5 SPLADE batch), #844 (AD-19 API rename)
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.20.0, Languages: 54, Tests: ~2375, Chunk types: 25
- BGE-large + LLM summaries = best production config
- SPLADE null (off-the-shelf + code-trained). Reranker null (ms-marco-MiniLM).
- Eval: v2 (265q), fixture (296q), noise (143q)
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db
- 14-category audit complete, 69/71 findings fixed
