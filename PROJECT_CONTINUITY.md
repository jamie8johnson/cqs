# Project Continuity

## Right Now

**Eval harness + embedding cache shipped. Query logging live. (2026-04-06 CDT)**

### Branch: `feat/eval-harness` → PR #832 (CI pending)
- V2 eval harness: 48-query seed set, 8 categories, bootstrap CIs, per-query JSONL, markdown reports
- Batch query logging: search/gather/scout/onboard/where/task → `~/.cache/cqs/query_log.jsonl`
- Reranker ablation: ms-marco-MiniLM net negative across all passage formats and α values
- Ground truth validated against live 12k-chunk index

### PR #831 merged: embedding cache
- `EmbeddingCache` (SQLite, keyed by content_hash + model_fingerprint)
- Model fingerprint (blake3 of ONNX file)
- Pipeline integration (cache read → store check → embed → cache write → evict)
- `cqs cache stats/clear/prune` CLI commands
- Zero `clippy::too_many_arguments` suppressions (3 context structs)

### Eval baseline (BGE-large, 32 train queries, live index)
- R@1: 56.2%, R@5: 84.4%, R@20: 100%
- Identifier: strong. Behavioral: medium. Conceptual/negation/structural: weak.
- Expand to 300 queries for statistically defensible deltas (±2.5pp CI at N=300)

### Reranker findings
- ms-marco-MiniLM-L-6-v2 degrades R@1 by -15pp (NL passages) to -49pp (raw code)
- Score interpolation (α * emb + (1-α) * rerank) doesn't help — reranker signal is noise
- Need code-trained cross-encoder to make the reranker layer useful

### What still needs to happen
- Merge #832 after CI green
- Expand query set from 48 to 300 (iterative curation)
- Release v1.18.0 (cache + eval harness + query logging)
- Code-trained reranker experiment (when eval is at 300q)

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
- Version: 1.17.0 (1.18.0 pending with cache + eval)
- Languages: 53 + L5X/L5K, Tests: ~2351
- 22 chunk types
- BGE-large production model at 90.9% pipeline R@1 (296q fixture eval)
- Cosine-only search (RRF disabled)
- HNSW traversal-time filtering for chunk_type/language
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db
- Eval: v2 harness (48q seed, evals/), fixture eval (296q), noise eval (143q)
- Query logging: batch mode → ~/.cache/cqs/query_log.jsonl
