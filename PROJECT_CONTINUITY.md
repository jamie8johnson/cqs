# Project Continuity

## Right Now

**v1.7.0 released. v9-200k dataset gap-filling pipeline running. (2026-03-26)**

### Active
- **Gap-filling pipeline**: Indexing 2,119 new repos (Ruby/TS/C++/PHP/Python) at 9/2119. Target: 22,222 pairs per language × 9 = 200K perfectly balanced.
  - Openclaw indexed: 49K TS pairs (gap filled for TypeScript)
  - Script: `~/training-data/fill_gaps.sh` (steps: select → filter → clone → index → merge → check → HF dataset prep)
  - Output: `v9_merged_pairs_v3.jsonl` after merge
- **Uncommitted on `release/v1.7.0` branch**: CONTRIBUTING.md "Adding a New Language" guide

### Session Accomplishments (2026-03-26)
1. v1.5.0, v1.6.0, v1.7.0 released
2. 6th audit: 82/82 findings fixed
3. v9-mini: 65.5% raw R@1, 89.1% enriched, 0.638 CSN
4. 8 models benchmarked — enrichment dominance (43.6pp)
5. Configurable models: 4-phase parallel execution → PR #690 → v1.7.0
6. Eval scripts: prefix-configurable, unified model registry
7. Workflow skills: /before-edit, /investigate, /check-my-work
8. CLAUDE.md restructured + telemetry archived
9. "Adding a New Language" guide with copy-paste template
10. HF dataset publishing in pipeline

### Pending
1. Push CONTRIBUTING.md guide to main
2. Gap-filling completes → assemble 200K → publish HF
3. Mine hard negatives on 200K
4. Train v9-200k
5. BGE-large eval via `CQS_EMBEDDING_MODEL=bge-large`
6. Paper v0.6

## Parked
- Dart language support (guide written)
- Curriculum scheduling (v9-full)
- Ship v9-mini as default (matches base enriched, better raw+CSN)

## Open Issues
- #389, #255, #106, #63 (all blocked on upstream)

## Architecture
- Version: 1.7.0
- Models: E5-base default, BGE-large preset, custom ONNX
- ModelConfig: CLI > env > config > default
- EMBEDDING_DIM: fully runtime
- Languages: 51 (guide for adding #52)
- Tests: 2025
