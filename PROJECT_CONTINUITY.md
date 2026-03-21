# Project Continuity

## Right Now

**Hard negative mining running, audit fixes ready to commit (2026-03-21 15:38 CDT).**

### Running
- Hard negative mining: 1.7M CSN pairs, ~3% done, ETA ~5:30 PM CDT. Output: `~/training-data/csn_hard_negs.jsonl`
- After mining: train v7 with `train_lora.py --data csn_hard_negs.jsonl`, eval on hard eval + full 10-task CoIR

### Ready to commit (~55 dirty files)
All audit fixes from 6 agents:
- P1 (16): EX-13/14, RB-12/13, EH-17/18, PB-19/20, DOC-15-20
- P2 (6): AC-10, AC-14 (coverage→test_ratio), PERF-24, SEC-10, AD-27
- P3 (~22): tracing spans, code dedup, WSL comments, backoff, tests
- P4 (2): AD-28 (7 batch commands), PERF-28 (parallel call extraction)
- CI: eval job summary step
- Research log: Exp 13 CoIR, dimensions framework, publication assessment
- Haiku vs Sonnet comparison: identical R@1, model doesn't matter for summaries

### After commit
1. PR + merge
2. Create GitHub issues for unfixed items (CQ-16, CQ-18, RM-22, RM-26, RM-27, AD-24)
3. Re-run `--improve-all` (EX-13 fixed is_source_file to use language registry)
4. Re-index, re-run hard eval to verify metrics

### Key findings this session
- **v5 > v3**: shipped to HF (+1.2pp CSN, +1.4pp CosQA)
- **LoRA is a specialization trade-off**: full CoIR 48.67 (#8) vs base 50.90 (#7)
- **Hard negatives target semantic depth**: orthogonal to task breadth and text distribution
- **Summaries are gap-fillers**: help undocumented code, hurt documented code. Model doesn't matter.
- **8 quality dimensions**: semantic depth, task breadth, text distribution, abstraction level, structural awareness, negative space, granularity, cross-lingual transfer
- **Agent users**: cqs users are always AI agents. Precise/technical queries. Semantic depth matters most.

## Parked
- Train v7 on hard negatives (waiting for mining)
- Summary augmentation for Dimension 4 (abstraction level)
- Expand training languages (Rust/C++/TS)
- Language-specific LoRA adapters
- Paper draft
- GitHub issues for unfixed audit items

## Architecture
- Version: 1.3.0
- Schema: v16
- Embeddings: 768-dim E5-base-v2 LoRA v5 (166k/1ep)
- Metrics: 92.7% R@1, 0.965 NDCG@10 (hard eval, DocFirst)
- CoIR: 48.67 avg (9 tasks), CSN 0.683, CosQA 0.348
- Tests: 1290 lib pass (with gpu-index)
