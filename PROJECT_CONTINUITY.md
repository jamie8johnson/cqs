# Project Continuity

## Right Now

**SQ-10: Code reranker — completed, negative result (2026-03-19).**

### Results
- Fine-tuned cross-encoder/ms-marco-MiniLM-L-6-v2 on 50k CodeSearchNet + 7.5k docstring pairs
- Trained 3 epochs, ONNX exported, uploaded to jamie8johnson/code-reranker-v1
- **Both web-trained and code-trained rerankers hurt performance**
  - Web-trained: R@1 89.1% → 78.2% (-10.9pp)
  - Code-trained: R@1 89.1% → 9.1% (catastrophic)
- Decision: Abandon reranking. E5-base-v2 embedding-only is near-optimal.

### Done this session
- SQ-8 merged (PR #627) — --improve-docs LLM doc comment generation
- SQ-10 training + eval complete — negative result, reranking hurts
- Fixed CI: atomic_write race condition in rewriter tests (PR #628)
- CQS_RERANKER_MODEL env var added to reranker.rs (on sq10 branch)
- Reranker eval harness added to model_eval.rs (on sq10 branch)
- Research log updated with Experiments 4 and 5

### SQ-10 branch (sq10-code-reranker)
- Rust integration + eval committed but NOT pushed/merged
- Eval harness is useful for future experiments even though reranking failed
- Consider merging just the eval harness + configurable model without making reranking default

## Parked

- **SQ-7 LoRA:** revisit if we switch base model (SQ-3). 3 experiments all regressed.
- **SQ-10 Reranking:** V2 could try hard negatives (BM25/embedding top-k) or larger model (L-12). Low priority.
- **SQ-3:** Code-specific base model
- **Post-index name matching** — fuzzy cross-doc references

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00
- cuVS PR #1840 (CAGRA serialize): open
- Audit cuVS + ort: planned

## Architecture

- Version: 1.1.0
- Schema: v16 (composite PK on llm_summaries)
- Embeddings: 768-dim E5-base-v2 (no fine-tuning — proven near-optimal)
- Tests: ~1270 (lib, with gpu-index)
- Training env: conda cqs-train, A6000 48GB
- Training data: ~/training-data/ (CodeSearchNet, docstring pairs, reranker model)
- Research log: ~/training-data/RESEARCH_LOG.md (5 experiments tracked)
