# Project Continuity

## Right Now

**v8-keydac training ~80% (2026-03-25). Major eval finding: LoRA degrades hard eval R@1.**

### Active
- **v8-keydac training** on A6000 (~10500/13170 steps, ~2h remaining)
- **Cross-GPU eval finding** (Exp 16): LoRA models score WORSE than base on hard eval (RTX 4000 median-of-3). Base 90.9%, v5 85.5%, v7 83.6%, v7b 81.8%. Prior A6000 "all at 89.1%" is suspect.

### After training completes
1. Export ONNX — automatic via `--export-onnx`
2. Run v8 hard eval on RTX 4000 (3x median)
3. Run v8 enriched hard eval (with contrastive summaries)
4. **Re-run full model matrix on A6000** — verify if "all 89.1%" was artifact
5. Run CoIR (9 tasks) on v8
6. Update paper v0.4 with complete cross-GPU comparison
7. If v8 improved on CSN/CosQA: publish model, release v1.4.3

### Session accomplishments (2026-03-25)
1. Cross-GPU eval: discovered LoRA degrades hard eval R@1 (Exp 16)
2. Paper revised to v0.3 (cross-GPU methodology, corrected specialization trade-off)
3. v7b added to eval harness + measured
4. Verified RTX 4000 uses CUDA for eval (CUDA_VISIBLE_DEVICES=1)

### Previous session (2026-03-24)
1-11: See git log for PRs #667-#674

## Parked
- A6000 → Blackwell upgrade consideration
- KD-LoRA distillation — after v8 eval
- Paper submission — after v8 results + A6000 re-verification

## Open Issues
- #665: RM-23 enrichment_pass ~105MB memory
- #666: DS-17/DS-18 GC transaction windows
- #389: CAGRA memory retention (upstream cuVS)
- #255: Pre-built reference packages
- #106: ort pre-release RC
- #63: paste crate warning

## Architecture
- Version: 1.4.2 (released)
- Current model: LoRA v7 (0.707 CSN, 49.19 CoIR, 83.6% hard eval R@1 on RTX 4000)
- Training: v8-keydac in progress (443k KeyDAC-augmented)
- Enriched pipeline: 92.7% R@1, 100% R@5 (contrastive summaries compensate for LoRA degradation)
- Tests: 1395 lib + 34 adversarial
- GPUs: A6000 (device 0, training), RTX 4000 (device 1, eval)
- Paper: ~/training-data/paper/draft.md (v0.3)
