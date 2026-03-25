# Project Continuity

## Right Now

**Switched default model to base E5 (2026-03-25). v8 CoIR running. Research continues.**

### v8 Results
- Hard eval: **92.7% R@1** (3x identical, zero non-determinism) — matches base E5, first LoRA to not degrade
- Enriched hard eval: 92.7% R@1, 100% R@5
- Full-pipeline (with HyDE): **96.3% R@1**
- CSN: **0.652** (regression from v7's 0.707 — KeyDAC traded benchmark for precision)

### Authoritative A6000 Hard Eval Matrix (median of 3)
| Model | R@1 | CSN |
|-------|-----|-----|
| Base E5 | 92.7% | 0.627 |
| v5 (MNR) | 85.5% | 0.683 |
| v7 (GIST) | 81.8% | 0.707 |
| v7b (GIST) | 83.6% | 0.707 |
| **v8 (KeyDAC)** | **92.7%** | 0.652 |

Prior "all at 89.1%" was wrong. v8 is the only LoRA that preserves hard eval precision.

### Remaining evals
- Full 9-task CoIR for v8 — in progress

### What to decide
- **Ship v8** for production (best hard eval, zero non-determinism, 96.3% full-pipeline)
- **Report v7** for paper benchmarks (best CSN at 0.707)
- v8 CSN regression is language-specific: Python near-perfect (0.996) but PHP/JS/Java collapsed
- Root cause: unbalanced training data (41% Python, 6% JS) + KeyDAC favors English-like identifiers
- v9 plan (~300k): balanced oversampling + 78k harvested cqs pairs + synthetic queries + curriculum scheduling. More diversity, not volume. Target: CSN ≥ 0.70 AND hard eval ≥ 90%

### Session accomplishments (2026-03-25)
1. v8 training completed (19.5h, 443k KeyDAC pairs)
2. Authoritative A6000 matrix (debunked "all 89.1%")
3. HyDE predictions generated (4304 functions, $0.38)
4. 78k training pairs harvested (67k HyDE + 9k summaries + 1.4k docs)
5. Python scripts audited and fixed (11 scripts, error handling + argparse)
6. Stress eval script written
7. Literature sweep 2 (7 new strategies, HF Papers API)
8. Paper revised to v0.3

## Open Issues
- #665, #666, #389, #255, #106, #63

## Architecture
- Version: 1.4.2
- Current shipping model: base E5 (intfloat/e5-base-v2, 92.7% hard eval, 0.627 CSN)
- Best hard eval: v8-keydac (92.7% R@1, 0.652 CSN) = base E5
- Best CSN: v7 (0.707)
- Full-pipeline: 96.3% R@1 (v8 + HyDE + contrastive summaries)
- Paper: ~/training-data/paper/draft.md (v0.3)
- Training repo: github.com/jamie8johnson/cqs-training
