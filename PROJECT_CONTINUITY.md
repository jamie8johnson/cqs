# Project Continuity

## Right Now

**v1.11.0 released. PR #722 awaiting CI (6 issues + markdown split). Session winding down. (2026-03-29 17:00 CDT)**

### What shipped this session
- v1.11.0: 8th audit (80/88 fixed) + 6 new commands + query expansion
- PRs merged: #715 (audit), #720 (commands), #721 (CONTRIBUTING checklist)
- crates.io published, GitHub release built, binary installed
- 19 OpenClaw contributions (9 PRs, 9 issues, 1 comment — all awaiting maintainer review)
- Paper v0.6 (thesis: training signal quality > model capacity)
- Pre-Edit hook live (`.claude/hooks/pre-edit-context.py`)
- Audit skill improved (prompt gen + review steps 8-9, P4 trivials fixed inline)

### PR #722 awaiting CI
Branch: `fix/open-issues-batch`. Closes 6 issues:
- #711 RT-RES-9: diff impact capped at 500 functions
- #695 EX-32: export-model auto-detect dim from config.json
- #694 EX-30: BatchProvider::is_valid_batch_id moved to trait
- #697 SEC-22: cargo audit config for transitive advisories
- #718 CQ-38: parser/markdown.rs split into 4-file directory
- #716 PERF-45: EMBED_BATCH_SIZE restored to 64 with debug logging

CI passed. PR #722 merged. Local failure was 3 concurrent test runs competing for GPU/DB.

### Training — 89.1% basin confirmed (5 data points)
| Variant | Change | Pipeline R@1 |
|---------|--------|-------------|
| v9-200k | baseline | **94.5%** |
| v9-500k | 2.5× more data | 89.1% |
| v9-200k-hn | + FAISS hard negatives | 89.1% |
| v9-200k-1.5ep | 1.5× more epochs | 89.1% |
| contrastive-B | 25% contrastive queries | 89.1% |

Breaking the ceiling requires fundamentally different training pairs (test-derived queries, type-aware negatives), not format variations.

### Active training
- v9-175k: training in progress on A6000 (~2h remaining, started 18:20 CDT)
- CSN eval for contrastive-B: **done — 0.689** (best LoRA CSN ever, +7.4pp over v9-200k)
- 225K dataset assembled (25K/lang from 500K pool), ready to train after 175K
- Contrastive prefix is a CSN optimization technique: +7.4pp CSN but same -5.4pp pipeline

### Next session
1. Check 175K results → if 94.5% run 225K, if 89.1% peak is at exactly 200K
2. Rebuild binary (main has #722 fixes beyond v1.11.0 tag)
3. Re-run full eval matrix on current code (synonym expansion changed FTS behavior)
4. Release v1.12.0 after eval re-verification
5. Paper v0.7 with basin finding (5+ data points) + data size sweep results

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1
- Blackwell GPU upgrade
- Publish 500K/1M datasets to HF (waiting for training experiments to settle)

## Open Issues
- #717 RM-40 (HNSW fully in RAM, no mmap)
- #389 (upstream cuVS CAGRA memory)
- #255, #106, #63 (upstream deps)
- #694, #695, #697, #711, #716, #718 closed by #722

## Architecture
- Version: 1.11.0, BGE-large default (1024-dim)
- v9-200k LoRA: 94.5% pipeline, 70.9% raw (110M = 335M on pipeline)
- Commands: 50+ (including brief, affected, neighbors, doctor --fix, train-pairs)
- Query expansion: 31 synonym mappings (auth→authentication, etc.)
- parser/markdown.rs split into markdown/ directory (4 files, 3 context structs)
- EMBED_BATCH_SIZE: 64 (restored from 32, with debug logging)
- Pre-Edit hook: auto-injects module context for .rs files
- Tests: ~1540
- OpenClaw: 19 contributions (all awaiting review)
