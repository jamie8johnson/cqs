# Project Continuity

## Right Now

**Language macro v2 in progress. Margin sweep complete. (2026-04-05 CDT)**

### Active work
- Language codegen: 52 defs in `languages.rs`, 106 `.scm` files, macro switched. Tests compiling (clean build after artifact cleanup). Branch: `feat/language-macro-v2`
- Next steps: verify tests pass → delete old `.rs` files → move tests → commit → PR

### Margin sweep results (GIST margin, E5-base, 200K CG-filtered)
| Margin | R@1 | MRR |
|--------|-----|-----|
| 0.01 | 69.1% | 0.785 |
| **0.03** | **72.7%** | **0.811** |
| 0.05 | 70.9% | 0.792 |
| 0.08 | 69.1% | 0.797 |
| 0.10 | 69.1% | 0.793 |
Peak at 0.03 — +1.8pp over baseline. Pipeline eval pending.

### Band mining experiment
Script ready: `~/training-data/run_band_mining.sh`. Uses margin-0.03 model + band [20,50) + CG filter. A6000 free.

### Session artifacts
- PR #814 (session artifacts) — in CI
- Results log updated: `~/training-data/RESULTS.md`
- ROADMAP updated with Exp 28 margin sweep

### Training results (BGE-large FT)
91.6% R@1 fixture, 50% R@1 real (100q), 57.5 CoIR. Published: jamie8johnson/bge-large-v1.5-code-search

## Parked
- Cross-project call graph — spec ready
- Embedding cache — spec ready
- Wiki system — spec ready (standalone design)
- SSD fine-tuning: band mining prepped, iterative self-distillation next
- Ladder logic (RLL) grammar
- Dart, hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Reranker V2 experiments

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste — advisory resolved, monitoring)

## Architecture
- Version: 1.15.2, Languages: 52 + L5X/L5K, Commands: 54+, Tests: 2351
- Best model: BGE-large FT (91.6% R@1, 57.5 CoIR)
- CI: rust-cache, ~16m test
- CommandContext with lazy reranker + embedder + open_readwrite
- Commands in 7 subdirectories, JSON schema typed Serialize
- 10th audit: 103/103 fixed, 0 remaining
- Language macro v2: consolidated `languages.rs` + `queries/*.scm` (in progress)
