# Project Continuity

## Right Now

**Pivoted from LoRA to SQ-8 + SQ-10 (2026-03-19).** Three LoRA experiments all regressed. E5-base-v2 is near-optimal with our NL pipeline. Next: improve descriptions (SQ-8) and reranking (SQ-10).

### LoRA results (see ~/training-data/RESEARCH_LOG.md)
| Experiment | R@1 | MRR | Verdict |
|-----------|-----|-----|---------|
| Baseline (E5-base-v2) | 89.1% | 0.934 | — |
| v1: commit msgs, 3ep | 70.9% | 0.800 | Regression |
| v2: commit msgs, 1ep/5e-6 | 69.1% | 0.798 | Regression |
| v3: docstrings+CSN, 1ep | 72.7% | 0.829 | Regression (best of 3) |

### Next session priorities
1. **SQ-8:** LLM doc generation (`--improve-docs`) — richer descriptions
2. **SQ-10:** Fine-tune code reranker on CodeSearchNet (1.7M pairs ready)
3. **Reranking default for `--json`** — agents get best results automatically

### Infrastructure preserved
- Training env: conda cqs-train (PyTorch 2.10, A6000 48GB)
- Training data: ~/training-data/ (186k commit triplets, 7.5k docstring pairs, 1.7M CodeSearchNet)
- Training repos: ~/training-repos/ (19 repos, 12 languages)
- HuggingFace: jamie8johnson, hf CLI authenticated
- CoIR benchmark: installed in cqs-train env
- Research log: ~/training-data/RESEARCH_LOG.md

### Done this session (2026-03-18/19)
- Full audit: 88 findings, 12 PRs merged (#614-#625)
- v1.1.0 released
- SQ-9: notes simplification + 768-dim
- P3 deferred + P4 refactors + file splits
- `cqs train-data` command
- 3 LoRA experiments (all regressed → pivoted)
- CSS parser @media print fix
- 19 training repos cloned, CodeSearchNet downloaded
- Research log started

## Parked

- **SQ-7 LoRA:** revisit if we switch base model (SQ-3)
- **SQ-3:** Code-specific base model (CodeBERT, UniXcoder, Nomic-embed-code)
- **Post-index name matching** — fuzzy cross-doc references

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00
- cuVS PR #1840 (CAGRA serialize): open
- Audit cuVS + ort: planned

## Architecture

- Version: 1.1.0
- Schema: v15 (768-dim)
- Embeddings: 768-dim E5-base-v2 (base model, no fine-tuning)
- Tests: ~1734
- File structure: search/ (3), embedder/ (2), cli/enrichment.rs, cli/args.rs, train_data/ (6), test_helpers.rs
