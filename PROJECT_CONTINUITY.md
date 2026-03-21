# Project Continuity

## Right Now

**Discriminating descriptions proven (+16pp over raw code). Ready to ship (2026-03-20).**

### What to ship
1. **v3 LoRA** as default embedding model (15 min training, +4.4pp CSN)
2. **Discriminating prompt** for SQ-6 summaries (change `build_prompt` in `llm.rs`)
3. **Fix `run_hard_eval.py` parser** — regex misses ~30 functions vs tree-sitter

### Description Experiment Results

| Config | R@1 | MRR | vs Raw |
|--------|-----|-----|--------|
| Raw code | 47.3% | 0.673 | — |
| Generic descriptions | 60.0% | 0.726 | +12.7pp |
| Discriminating | 63.6% | 0.763 | +16.3pp |
| Contrastive (top-5 neighbors) | 65.5% | 0.769 | +18.2pp |

Ship discriminating (simple, cheap). Contrastive is future optimization.

### Complete LoRA Results

| Config | CSN | CosQA | Training |
|--------|-----|-------|----------|
| Base E5 | 0.627 | 0.329 | — |
| **v3 (50k+docs/1ep)** | **0.671** | **0.334** | **15 min** |
| 200k+docs/1ep | 0.680 | 0.353 | 40 min |
| Rank 32 | 0.682 | 0.351 | 40 min (flat) |

### Key decisions
- E5 stays as base model (CodeSage fails NL queries)
- v3 LoRA: best value for effort
- Discriminating prompt: biggest bang for buck of any experiment
- Hard eval deprioritized (measures E5 perturbation, not production quality)
- Rank not the bottleneck — data is

## Parked
- v1.1.0 release — after LoRA + prompt ship
- Contrastive descriptions (two-pass, future optimization)
- Language-balanced training data from popular repos
- Full 10-task CoIR for paper
- 1.7M LoRA run (killed — diminishing returns)

## Architecture
- Version: 1.1.0, Schema: v16
- Embeddings: 768-dim E5-base-v2 + signatures (SQ-11)
- LLM: summaries (SQ-6), doc comments (SQ-8), hyde (SQ-12)
- Tests: 1265 lib pass
