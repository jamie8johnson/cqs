# Project Continuity

## Right Now

**v1.9.0 released (BGE-large default). Starting v9-200k training pipeline. (2026-03-27)**

### Active
- **Training pipeline**: Hard negative mining on 200K dataset, then 3 ablation runs
  - v9-200k: 200K pairs, call-graph filter only, 1 epoch
  - v9-200k-hn: 200K pairs, call-graph + FAISS hard negatives, 1 epoch
  - v9-200k-3ep: 200K pairs, call-graph + FAISS, 3 epochs
  - Each ~80 min on A6000, one variable changed per run
- **200K dataset ready**: `~/training-data/cqs-code-search-200k.jsonl` (22,222 × 9)
- **Eval infrastructure**: `CQS_ONNX_DIR` enables pipeline eval for LoRA models

### Pending
1. Hard negative mining on 200K dataset
2. Train v9-200k / v9-200k-hn / v9-200k-3ep
3. Eval all three → pick best 110M model
4. Publish HF datasets (200K/500K/1M)
5. Paper v0.6

## Parked
- Dart language support (guide written)
- hnswlib-rs migration (audited, fork path documented)
- BGE-large LoRA (deferred, focusing on 110M improvement)

## Open Issues
- #389, #255, #106, #63 (blocked on upstream)
- #694-697, #700 (audit P4)

## Architecture
- Version: 1.9.0
- Default model: BGE-large-en-v1.5 (1024-dim)
- ModelConfig::default_model() → single source of truth
- E5-base available as preset (CQS_EMBEDDING_MODEL=e5-base)
- CQS_ONNX_DIR for local model loading
- Tests: 1491
- Metrics: 94.5% R@1 / 0.966 MRR (BGE-large pipeline)
