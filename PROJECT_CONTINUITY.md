# Project Continuity

## Right Now

**v1.18.0 released. Full ablation complete. Best config confirmed. (2026-04-07 CDT)**

### Released this session
- PR #831: Embedding cache (SQLite, model fingerprint, `cqs cache` CLI)
- PR #832: V2 eval harness + batch query logging (112 queries, 8 categories, bootstrap CIs)
- PR #833: 5 new chunk types (Test, Variable, Endpoint, Service, StoredProc — 27 total)
- PR #834: v1.18.0 release

### Key findings
- ms-marco-MiniLM-L-6-v2 reranker is net negative for code search (-15pp to -49pp)
- Eval baseline: R@1=68%, R@5=87%, R@20=100% on 75 train queries against 13k-chunk live index
- Identifier lookup: 100% R@1. Conceptual/multi-step: 25% R@1.
- New chunk types added ~1,200 chunks to index (13,290 vs 12,085)
- Test chunk type is additive to existing heuristic test detection (find_test_chunks)

### Design decisions
- Embedding cache: SQLite, keyed by (content_hash, model_fingerprint), LRA eviction
- Test is_callable (call graph + test discovery). Variable is_code not callable. Service is_code not callable.
- Endpoint has capture name registered but no .scm queries yet (Phase 2: framework-specific)
- Three hardcoded capture lists (chunk.rs, mod.rs, define_chunk_types!) all updated — unification still needed

### Ablation results (v2 eval, 75 train queries, BGE-large)
- **Best config: BGE-large + LLM summaries, no SPLADE, no reranker**
- Summaries: +25pp behavioral, +25pp multi_step, -25pp negation, +1.3pp overall
- SPLADE: 0pp across all configs. Off-the-shelf model doesn't know code.
- Reranker: -15pp to -49pp. ms-marco-MiniLM not code-trained.
- Store dim check fix written (model switching bug), not yet released

### What still needs to happen
- Expand eval to 300 queries
- Phase 2 chunk types: Flask/Express endpoint detection, JS describe/it/test blocks
- Fine-tune SPLADE on code pairs (200k training data ready)
- Code-trained reranker experiment
- Unify the three capture name lists into single source of truth
- Release store dim check fix (model switching)

## Parked
- Wiki system — spec + plan ready
- Cross-project call graph — spec ready
- Ladder logic (RLL) grammar
- hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Paper v0.7
- SPLADE: shipped but +2pp is inside noise floor, need 300q eval to confirm

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.18.0, Languages: 53 + L5X/L5K, Tests: ~2360
- 27 chunk types (Test, Variable, Endpoint, Service, StoredProc added this session)
- BGE-large production model at 90.9% pipeline R@1 (296q fixture eval)
- Cosine-only search (RRF disabled)
- HNSW traversal-time filtering for chunk_type/language
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db
- Eval: v2 harness (112q, evals/), fixture eval (296q), noise eval (143q)
- Query logging: batch mode → ~/.cache/cqs/query_log.jsonl
- 13,290 chunks, 432 files indexed
