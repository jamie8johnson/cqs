# Project Continuity

## Right Now

**Two-lane parallel work session. v1.18.0 shipped. (2026-04-07 CDT)**

### GPU Lane: SPLADE fine-tuning (RUNNING)
- PID 182066, A6000 100% utilization, 48.4/49.1 GB VRAM
- 12,250 steps, ~2.2s/step, ETA ~7.5h from start
- Checkpoints every 500 steps, tensorboard at `~/training-data/splade-code-v1/tb_logs`
- Log: `~/training-data/splade_train.log`
- Script: `~/training-data/train_splade.py`
- Config: LoRA r=16, lr=2e-5, batch=32, reg_weight=5e-4, 2 epochs

### CPU Lane: dev work
1. ~~**Unify capture name lists**~~ — DONE. `ChunkType::CAPTURE_NAMES` replaces three lists. PR #836.
2. ~~**Phase 2 chunk types**~~ — DONE. JS/TS describe/it/test, Python Flask endpoints. PR #836.
3. **Expand eval to 300q** — next. Generator handles auto categories, need ~150 hand-curated.

### What still needs to happen
- [ ] Expand eval to 300 queries
- [ ] Evaluate code-trained SPLADE (when training completes ~7.5h)
- [ ] ONNX export + integration test of trained SPLADE
- [ ] Code-trained reranker experiment (after SPLADE eval)
- [ ] Release v1.19.0 (capture unification + Phase 2 chunks + SPLADE if it works)

### This session so far
- PR #831: Embedding cache (merged)
- PR #832: V2 eval harness + query logging (merged)
- PR #833: 5 new chunk types (merged)
- PR #834: v1.18.0 release (merged)
- PR #835: Session artifacts + store dim fix (CI green)
- PR #836: Capture list unification + Phase 2 chunk types (CI pending)
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
- Three hardcoded capture name lists — unified in PR #836

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
