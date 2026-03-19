# Project Continuity

## Right Now

**SQ-7: LoRA training running on A6000 (2026-03-19).** 3 epochs, ~6 hours, finishes ~12:30am. Background task `b8xwgmkri`.

### Training details
- Data: 183,586 triplets from 19 repos (12 languages), ~/training-data/training_data.jsonl (1.5 GB)
- Model: E5-base-v2 + LoRA rank 16 (2.7M trainable / 112M total = 2.39%)
- Config: 3 epochs, batch 32, lr 2e-5, warmup 0.1, fp16
- Output: ~/training-data/e5-code-search-lora/ (adapter + merged model + ONNX)
- Script: ~/training-data/train_lora.py

### Eval baseline (hard eval)
| Model | R@1 | R@5 | MRR | NDCG@10 |
|-------|-----|-----|-----|---------|
| E5-base-v2 | 89.1% | 98.2% | 0.934 | 0.950 |
| jina-v2-base-code | 80.0% | 96.4% | 0.874 | 0.905 |

Per-language (E5-base-v2): Rust 0.955, Python 1.000, **TypeScript 0.758**, JavaScript 0.955, Go 1.000

### After training completes
1. Verify ONNX export in ~/training-data/e5-code-search-lora/onnx/
2. Run hard eval with fine-tuned model — compare R@1, MRR, per-language
3. If improvement: `hf upload jamie8johnson/e5-base-v2-code-search ./e5-code-search-lora/onnx/`
4. Update cqs model URL + hash, PR, release v1.1.1

### Also pending
- CSS parser fix committed but not pushed (css.rs:55 @media print panic)
- ROADMAP.md has SQ-7 updates (uncommitted)
- Cargo.toml description still says "Local ML" (fix in next release)

### Done this session
- Full 14-category audit: 88 findings fixed (PRs #614-#617)
- SQ-9: notes simplified, 769→768-dim, schema v15 (PR #620)
- P3 deferred + P4 refactors (PRs #621-#622)
- v1.1.0 released (PR #623)
- `cqs train-data` command (PR #624)
- Training data generated: 186k triplets, 19 repos, 12 languages
- LoRA training started

## Parked

- **SQ-3: Code-specific embedding model** — evaluate after LoRA results
- **SQ-8: LLM doc comment generation** — post-training
- **Post-index name matching** — fuzzy cross-doc references

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00
- cuVS PR #1840 (CAGRA serialize): open
- Audit cuVS + ort: planned

## Architecture

- Version: 1.1.0 (v1.1.1 pending after LoRA)
- Schema: v15 (768-dim)
- Embeddings: 768-dim E5-base-v2 (LoRA fine-tuning in progress)
- Tests: ~1734
- Training env: conda cqs-train, A6000 48GB, PyTorch 2.10
