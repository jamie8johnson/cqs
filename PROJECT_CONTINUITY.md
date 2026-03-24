# Project Continuity

## Right Now

**v8-keydac training in progress (2026-03-24). PR #672 pending CI. All prior work merged.**

### Active
- **v8-keydac training** running on A6000 (PID 255391). ETA ~14-21h. Log: `~/training-data/train_v8_keydac.log`
  - Data: `augmented_200k_keydac.jsonl` (443k pairs = 200k original + 243k KeyDAC augmented)
  - Config: 1 epoch, batch 32, GIST + Matryoshka, LoRA r=16
- **PR #672** pending: enriched hard eval + KeyDAC script + CI Node.js 24 migration
- **RTX 4000** (8GB) available for inference/eval while A6000 trains

### Session accomplishments (2026-03-24)
1. v1.4.0 audit: 74 findings, 70 fixed (PRs #667, #668)
2. v1.4.1 released: audit fixes
3. Contrastive summaries: 92.7% R@1 full-pipeline (PR #669)
4. v1.4.2 released: contrastive + adversarial tests (PRs #669, #670)
5. Adversarial test hardening: 34 tests across 5 areas (PR #670)
6. Enriched hard eval: 92.7% R@1, 100% R@5, 0.9624 NDCG@10 (PR #672)
7. KeyDAC augmentation script + data generation (PR #672)
8. v8 training kicked off

### After training completes
1. Export ONNX (opset-11 template)
2. Run hard eval (raw + enriched) — compare v8 vs v7 vs base
3. Run CoIR (9 tasks) — compare CSN, CosQA transfer
4. If improved: publish model, bump cqs default, release v1.4.3
5. Update research log with Exp 16 results

### Next experiments
1. **KD-LoRA distillation** — CodeSage-large → E5-base. ~12h. After v8 eval.
2. **Paper** — with contrastive summaries + KeyDAC results

## Parked
- Paper revision — after v8 eval
- Verified HF eval results — needs CoIR benchmark registration

## Open Issues
- #665: RM-23 enrichment_pass ~105MB memory (deferred)
- #666: DS-17/DS-18 GC transaction windows (informational)
- #389: CAGRA memory retention (blocked on upstream cuVS)
- #255: Pre-built reference packages (enhancement)
- #106: ort pre-release RC
- #63: paste crate warning (monitoring)

## Architecture
- Version: 1.4.2 (released, tagged, published to crates.io)
- Current model: LoRA v7 (200k 9-lang, GIST+Matryoshka, 0.707 CSN, 49.19 CoIR)
- Training: v8-keydac in progress (443k KeyDAC-augmented pairs)
- Full-pipeline: 92.7% R@1, 0.9478 NDCG@10 (contrastive summaries)
- Enriched hard eval: 92.7% R@1, 100% R@5, 0.9624 NDCG@10
- Tests: 1395 lib + 34 adversarial
- GPUs: A6000 48GB (training), RTX 4000 8GB (inference)
- 5th full audit (v0.5.3, v0.12.3, v0.19.2, v1.0.13, v1.4.0)
