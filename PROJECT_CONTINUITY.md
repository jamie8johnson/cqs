# Project Continuity

## Right Now

**v8-keydac training ~77% complete (2026-03-25). ETA ~9:20 AM CDT. All PRs merged.**

### Active
- **v8-keydac training** on A6000 (~10200/13170 steps, ~3h remaining)
  - Data: `augmented_200k_keydac.jsonl` (443k pairs = 200k original + 243k KeyDAC augmented)
  - Config: 1 epoch, batch 32, GIST + Matryoshka, LoRA r=16
  - Log: `~/training-data/train_v8_keydac.log`
- **RTX 4000** (8GB) available for inference/eval (CUDA_VISIBLE_DEVICES=1)

### After training completes
1. Export ONNX (opset-11 template) — automatic via `--export-onnx`
2. Run hard eval 3x median on RTX 4000 — compare v8 vs v7 vs base
3. Run enriched hard eval (with contrastive summaries)
4. Run CoIR (9 tasks) — compare CSN, CosQA transfer
5. If improved: publish model to HF, bump cqs default, release v1.4.3
6. Update research log with Exp 16 results, update paper draft v0.3

### Session accomplishments (2026-03-24)
1. v1.4.0 audit: 74 findings, 70 fixed (PR #667)
2. v1.4.1 released: audit fixes (PR #668)
3. Contrastive summaries: 92.7% R@1 full-pipeline (PR #669)
4. v1.4.2 released: contrastive + adversarial tests (PRs #670, #671)
5. Enriched hard eval: 92.7% R@1, 100% R@5 (PR #672)
6. KeyDAC augmentation script + 443k training data (PR #672)
7. Housekeeping: CI Node.js 24, groomed notes 145→90, architecture docs (PRs #673, #674)
8. Paper draft v0.2 revised (~/training-data/paper/draft.md)
9. v8 training kicked off on A6000
10. RTX 4000 verified for parallel eval (CUDA_VISIBLE_DEVICES=1)
11. ORT CUDA non-determinism discovered (2-4pp R@1 variance per run)

### Next experiments
1. **KD-LoRA distillation** — CodeSage-large → E5-base. ~12h on A6000. After v8 eval.
2. **Paper submission** — with v8 results

## Parked
- Verified HF eval results — needs CoIR benchmark registration
- A6000 → Blackwell upgrade — when RTX PRO 6000 available (~$3500 net after A6000 eBay)

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
- GPUs: A6000 48GB (training, device 0), RTX 4000 8GB (eval, device 1)
- Paper: ~/training-data/paper/draft.md (v0.2)
- Training repo: github.com/jamie8johnson/cqs-training (private)
