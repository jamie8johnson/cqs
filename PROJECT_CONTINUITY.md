# Project Continuity

## Right Now

**v1.17.0 released. SPLADE + HNSW filtering + ConfigKey + fixes. (2026-04-06 CDT)**

### v1.17.0 highlights
- SPLADE sparse-dense hybrid search (--splade, +2-5pp on real code)
- HNSW traversal-time filtering for --chunk-type and --lang
- ConfigKey + Impl chunk types (22 total)
- Batch RRF disabled (17pp worse than cosine)
- LLM summary preservation across --force
- CAGRA itopk_size capped at 512
- prune_missing path mismatch fix
- Schema v17 (sparse_vectors + enrichment_version)

### Corrected SPLADE results (both legs working)
| Eval | Cosine | SPLADE rerank | Delta |
|------|--------|---------------|-------|
| Fixture R@1 (296q) | 91.2% | 91.2% | 0pp |
| Function lookup R@1 (50q) | 40% | 42% | +2pp |
| Conceptual hit (40q) | 52% | 57% | +5pp |

### Next
- Embedding cache (spec updated, plan needed)
- Wiki system (spec + plan ready)
- Code-specific SPLADE fine-tuning (CodeBERT-based)

## Parked
- Wiki system — spec + plan ready
- Cross-project call graph — spec ready
- Embedding cache — spec updated, plan next
- Ladder logic (RLL) grammar
- New chunk types: Variable, Test, Handler, Route
- Code-specific SPLADE (CodeBERT-based)
- hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Reranker V2
- Paper v0.7

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.17.0, Languages: 53 + L5X/L5K, Tests: ~2345
- Schema: v17 (sparse_vectors + enrichment_version)
- 22 chunk types (ConfigKey, Impl)
- BGE-large production model at 91.2% pipeline R@1
- Cosine-only default, SPLADE opt-in via --splade
- HNSW traversal-time filtering
- CAGRA itopk_size capped at 512
- LLM summaries preserved across --force
