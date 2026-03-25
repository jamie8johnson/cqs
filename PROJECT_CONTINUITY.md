# Project Continuity

## Right Now

**v8-keydac fully evaluated (2026-03-25). Running CoIR 9-task.**

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
- Ship v7 (best CSN) or v8 (best hard eval) or keep base (matches v8 on hard eval)?
- v8's value is CSN +2.5pp over base with no precision loss. v7's value is CSN +8pp but -11pp precision.
- v9 plan: synthetic queries + curriculum scheduling — could combine v7's recall with v8's precision

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
- Current shipping model: LoRA v7 (0.707 CSN, 81.8% hard eval)
- Best hard eval: v8-keydac (92.7% R@1, 0.652 CSN) = base E5
- Best CSN: v7 (0.707)
- Full-pipeline: 96.3% R@1 (v8 + HyDE + contrastive summaries)
- Paper: ~/training-data/paper/draft.md (v0.3)
- Training repo: github.com/jamie8johnson/cqs-training
