# Project Continuity

## Right Now

**v9-200k training 67% (ETA ~19:12 CDT). All PRs merged. (2026-03-27 18:27 CDT)**

### Active
- **v9-200k training**: 3953/5938 steps, GPU 100%. Monitor: `~/training-data/monitor_v9_200k.log`
  - Auto-eval runs raw + pipeline when training finishes
  - Output: `~/training-data/e5-code-search-lora-v9-200k/`
  - Next: v9-200k-hn (`bash run_v9_200k.sh --with-hard-negs`)
  - Then: v9-200k-1.5ep (`bash run_v9_200k.sh --with-hard-negs --epochs 1.5`)

### All merged today
- #701: 7th audit (85/95 fixed)
- #702: Roadmap (hnswlib-rs, datasets)
- #703: v1.8.0 release
- #704: Init dim fix
- #705: Metric corrections, CQS_ONNX_DIR, convenience wrappers
- #706: Notes groom
- #707: v1.9.0 BGE-large default
- #712: Red team 8 fixes
- #713: Watch mode RT-DATA-7/8

### Pending
1. PR the /cqs-verify skill + CLAUDE.md changes (1 commit on main ahead of remote)
2. v9-200k eval (auto-running when training completes)
3. v9-200k-hn training (~7h)
4. v9-200k-1.5ep training (~10h)
5. Compare all, update RESULTS.md
6. Publish HF datasets
7. Paper v0.6

## Parked
- Dart language support
- hnswlib-rs migration
- BGE-large LoRA

## Open Issues
- #389, #255, #106, #63 (upstream)
- #694-697, #700 (audit P4)
- #711 RT-RES-9 (diff impact cap)

## Architecture
- Version: 1.9.0
- Default: BGE-large 1024-dim
- ModelConfig::default_model() single source of truth
- /cqs-verify skill for session start verification
- Tests: 1491
- Metrics: 94.5% R@1 / 0.966 MRR (BGE-large pipeline)
