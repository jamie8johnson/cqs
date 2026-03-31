# Project Continuity

## Right Now

**Expanded eval complete. Paper v0.8. Full 9-model matrix on 296 queries. (2026-03-31 08:15 CDT)**

### Session deliverables
- Fixed FTS5 synonym expansion bug (OR groups need explicit AND)
- Expanded eval: 55→296 queries, 5→7 languages (added Java + PHP)
- Full 9-model eval matrix on expanded eval — all models completed
- Paper revised to v0.8 — all 55-query references replaced with 296-query results
- Results log + research log updated with Exp 27
- ROADMAP updated with BGE-large fine-tuning experiment
- PR #731 merged, binary rebuilt

### Key numbers (296 queries, 7 langs, Config A cosine-only)
| Model | R@1 | MRR |
|-------|-----|-----|
| BGE-large (335M) | **90.9%** | 0.9493 |
| v9-200k (110M) | **90.5%** | 0.9482 |
| Basin (6 variants) | 81–82% | ~0.90 |
| E5-base (110M) | 75.3% | 0.8688 |

### Key finding
55-query eval exaggerated model differences (94.5% vs 89.1% = 3-query gap). Expanded eval shows BGE-large and v9-200k are virtually tied. Basin is at 81-82% (was 89.1%). RRF hurts at scale (74.7% vs 90.9%).

### Next
1. Fine-tune BGE-large on 200K CG-filtered data (~3-4h on A6000)
2. Ship v9-200k as LoRA preset in cqs
3. Resume repo indexing (88/2332 done)
4. Release v1.13.0 with expanded eval

### OpenClaw — 7 PRs, 6 issues
Tracker: `docs/openclaw-contributions.md`. Consolidated from 12→7 PRs.

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1 (P&ID → PLC function block mapping)
- IEC 61131-3 language support
- Openclaw variant for PLC process control (long horizon)
- Blackwell GPU upgrade
- Publish 500K/1M datasets to HF
- Type-aware negative mining (7 basin points suggest diminishing returns)
- Imbalanced 200K experiment (lower priority post per-query analysis)

## Open Issues (cqs)
- #717 RM-40 (HNSW fully in RAM, no mmap)
- #389 (upstream cuVS CAGRA memory)
- #255, #106, #63 (upstream deps)

## Architecture
- Version: 1.12.0
- v9-200k LoRA: 90.5% pipeline (296q), 70.9% raw — published to HF
- BGE-large: 90.9% pipeline (296q), off-the-shelf
- Expanded eval: 296 queries, 7 languages
- Commands: 50+
- Tests: ~1540
