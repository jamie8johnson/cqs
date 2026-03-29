# Project Continuity

## Right Now

**8th audit: P1+P2 all fixed, PR #715 open (CI green). P3 fixer running. Training complete. (2026-03-29)**

### Branch: `audit/v1.9.0-plus`

### Audit v1.9.0+ — 88 findings, 14 categories
- **P1 (12): ALL FIXED.** PR #715.
- **P2 (15): ALL FIXED.** Including 4 large refactors (WatchState struct, N+1 UPDATE, O(N) contrastive, watch tests).
- **P3 (39): Fixer agent running.** Prompts generated.
- **P4 (25): 22 fixable (prompts generated), 3 hard (issues only: PERF-45, RM-40, CQ-38).**
- Audit skill updated: steps 8-9 (prompt gen + review). P4 trivials fixed inline.
- Pre-Edit hook installed: `.claude/hooks/pre-edit-context.py` auto-injects module context for .rs files.

### Training — v9-200k-1.5ep COMPLETE
**Result: 1.5 epochs regresses pipeline by 5.4pp (94.5% → 89.1%). Raw flat (70.9%).**
Third confirmation: 200K × 1 epoch × CG-filter-only is the recipe. More training doesn't help.

| Model | Pipeline R@1 | Raw R@1 | Raw MRR |
|-------|-------------|---------|---------|
| v9-200k (1ep) | 94.5% | 70.9% | 0.795 |
| v9-200k-1.5ep | 89.1% | 70.9% | 0.791 |

### OpenClaw — 19 contributions filed
Tracking: `docs/openclaw-contributions.md`. 9 PRs, 9 issues, 1 comment. Six Greptile 5/5.

### Uncommitted (on audit branch, beyond PR #715)
- P3 fixer agent actively modifying files (36 dirty files)
- CLAUDE.md workflow examples, ROADMAP agent adoption updates
- Pre-edit hook script + settings.local.json hook config
- math.rs test edit reverted

### Pending
1. P3 fixer completes → commit + push to PR #715
2. Apply fixable P4s (22 items, prompts ready)
3. File 3 hard P4 issues (PERF-45, RM-40, CQ-38)
4. Merge PR #715
5. Update RESULTS.md + research_log with 1.5ep results
6. Publish 500K/1M datasets to HF

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1
- Blackwell GPU upgrade

## Open Issues
- #389, #255, #106, #63 (upstream)
- #694-697, #700 (audit P4)
- #711 RT-RES-9

## Architecture
- Version: 1.9.0, BGE-large default (1024-dim)
- v9-200k LoRA: 94.5% pipeline, 70.9% raw (110M = 335M on pipeline)
- HF dataset: https://huggingface.co/datasets/jamie8johnson/cqs-code-search-200k
- OpenClaw PR: https://github.com/openclaw/openclaw/pull/56278
- Tests: 1491
