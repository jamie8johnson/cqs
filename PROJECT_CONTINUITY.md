# Project Continuity

## Right Now

**v9-200k training running (~7h). Red team fixes committed. (2026-03-27 16:40 CDT)**

### Active
- **v9-200k training**: Running detached on A6000. PID check: `ps aux | grep train_lora`
  - Log: `~/training-data/v9-200k_20260327_163510.log`
  - Output: `~/training-data/e5-code-search-lora-v9-200k/`
  - Config: 200K pairs, GIST+Matryoshka, 1 epoch, batch 32, lr 2e-5, LoRA r=16
  - After: run raw + pipeline eval, then start v9-200k-hn
  - Script: `~/training-data/run_v9_200k.sh` (supports `--with-hard-negs`, `--epochs`)
- **Red team fixes**: 8/23 findings fixed (committed, needs PR). 4 issues created (#708-711).
- **Uncommitted on main**: red team fixes + roadmap + notes groom

### Next steps (in order)
1. PR the red team fixes + groom notes + roadmap updates
2. Wait for v9-200k training to complete (~7h from 16:35 CDT → ~23:35)
3. Eval v9-200k (raw + pipeline)
4. Train v9-200k-hn: `bash run_v9_200k.sh --with-hard-negs`
5. Train v9-200k-1.5ep: `bash run_v9_200k.sh --with-hard-negs --epochs 1.5`
6. Compare all models, update results
7. Fix RT-DATA-7/8 (watch mode high findings)
8. Publish HF datasets

## Parked
- Dart language support
- hnswlib-rs migration (audited, fork path documented)
- BGE-large LoRA (deferred, focusing on 110M)

## Open Issues
- #389, #255, #106, #63 (upstream)
- #694-697, #700 (audit P4)
- #708 RT-DATA-7, #709 RT-DATA-8 (watch mode high)
- #710 RT-RES-1, #711 RT-RES-9 (performance caps — RT-RES-1 fixed inline)

## Architecture
- Version: 1.9.0
- Default: BGE-large 1024-dim, E5-base as preset
- ModelConfig::default_model() single source of truth
- Tests: 1491
- Metrics: 94.5% R@1 / 0.966 MRR (BGE-large pipeline)
