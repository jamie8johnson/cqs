# Project Continuity

## Right Now

**Two-lane parallel work session. v1.18.0 shipped. (2026-04-07 CDT)**

### GPU Lane: SPLADE fine-tuning
- Training naver/splade-cocondenser-ensembledistil on 200k code pairs
- Goal: code-aware token expansion ("sort" → {quicksort, heapsort, merge_sort})
- Off-the-shelf SPLADE confirmed null (0pp) in v2 eval ablation
- Requirements: observable (wandb), robust (checkpointing), resumable
- Status: research agent dispatched, awaiting findings

### CPU Lane: dev work (in order)
1. **Unify capture name lists** — three lists → one. Branch: `refactor/unify-capture-lists`. Quick cleanup.
2. **Phase 2 chunk types** — Flask/Express endpoint detection, JS describe/it/test blocks. Framework-specific tree-sitter queries.
3. **Expand eval to 300q** — generator handles identifier/structural/type-filtered auto. Need ~150 hand-curated behavioral/conceptual/negation/multi-step/cross-language queries.

### This session so far
- PR #831: Embedding cache (merged)
- PR #832: V2 eval harness + query logging (merged)
- PR #833: 5 new chunk types (merged)
- PR #834: v1.18.0 release (merged)
- PR #835: Session artifacts + store dim fix (open, CI pending)
- Full ablation: BGE-large × E5-LoRA × SPLADE × reranker × LLM summaries
- Best config confirmed: BGE-large + LLM summaries, no SPLADE, no reranker

### Ablation results (v2 eval, 75 train queries)
| Config | R@1 | R@5 |
|--------|-----|-----|
| BGE-large (baseline) | 68.0% | 86.7% |
| + LLM summaries | 69.3% | 85.3% |
| + SPLADE | 68.0% | 86.7% |
| + summaries + SPLADE | 68.0% | 84.0% |
| E5-LoRA v9-200k | 54.7% | 76.0% |
| ms-marco reranker | -15pp to -49pp | — |

### Bugs found this session
- Store `get_embeddings_by_hashes` not model-aware — dim check fix in PR #835
- Three hardcoded capture name lists — unification in progress

## Parked
- Wiki system — spec revised (agent-first), parked for review
- Cross-project call graph — spec ready
- Code-trained reranker — after SPLADE and eval expansion
- Ladder logic (RLL) grammar
- hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Paper v0.7

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.18.0, Languages: 53 + L5X/L5K, Tests: ~2360
- 27 chunk types (Test, Variable, Endpoint, Service, StoredProc added this session)
- BGE-large + LLM summaries = best production config
- Cosine-only search (RRF disabled, SPLADE null, reranker negative)
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db (2 models, 81 MB)
- Eval: v2 harness (112q, evals/), fixture (296q), noise (143q)
- Query logging: batch mode → ~/.cache/cqs/query_log.jsonl
- 10,948 chunks, 432 files indexed (BGE-large 1024-dim)
