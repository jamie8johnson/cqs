# Project Continuity

## Right Now

**SPLADE spec + plan written. RRF disabled, summary preservation fixed. 14 PRs today. (2026-04-06 CDT)**

### Specs & plans ready
- SPLADE sparse-dense hybrid: spec + 8-task plan at `docs/superpowers/specs/2026-04-06-splade-*` and `docs/superpowers/plans/2026-04-06-splade-*`
- Wiki system: spec + 8-task plan at `docs/superpowers/specs/2026-04-04-wiki-*` and `docs/superpowers/plans/2026-04-04-wiki-*`

### PRs merged this session (#810-827)
- #810-813: audit 103/103, v1.15.2 release
- #814: session artifacts
- #815: language macro v2 (52 files → 2 + queries)
- #816: Dart (53rd language), docs, roadmap cleanup
- #817: v1.16.0 release
- #818: ConfigKey chunk type, batch mode filter, eval cleanup
- #819: Impl chunk type for Haskell instances
- #820: Preserve LLM summaries across --force (ATTACH method)
- #826: HNSW traversal-time filtering for --chunk-type and --lang
- #827: Disable RRF in batch mode + robust summary preservation (read-before-rename)

### Key findings
- RRF degrades search 17pp vs cosine-only (74% vs 91.2%). Disabled in batch mode.
- ConfigKey: JSON/TOML/YAML/INI keys polluted code search. Fixed.
- Batch mode was missing code-only filter. Fixed.
- `--force` reindex silently regenerated LLM summaries (~$0.87/run). Fixed: read summaries before rename.
- E5-base training ceiling confirmed at ~81%. BGE-large at 91.2% is production model.
- Three training experiments (margin sweep, band mining, iterative distillation) all null.

### Re-baselined eval numbers (2026-04-06)
| Model | Pipeline R@1 (296q) |
|-------|---------------------|
| BGE-large FT | 91.9% |
| BGE-large | 91.2% |
| v9-200k | 81.4% |

### Uncommitted on main
- CLAUDE.md, PROJECT_CONTINUITY.md, ROADMAP.md, docs/notes.toml — tears
- docs/superpowers/specs/2026-04-06-splade-sparse-dense-hybrid-design.md — spec
- docs/superpowers/plans/2026-04-06-splade-sparse-dense-hybrid.md — plan
- docs/superpowers/specs/2026-04-04-ssd-fine-tuning-roadmap.md — updated with results
- docs/superpowers/plans/2026-04-04-wiki-system.md — revised plan

## Parked
- Wiki system — spec + plan ready
- Cross-project call graph — spec ready
- Embedding cache — spec ready
- Ladder logic (RLL) grammar
- hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Reranker V2
- Paper v0.7

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.16.0 (1.16.1 pending), Languages: 53 + L5X/L5K, Tests: ~2330
- 22 chunk types (ConfigKey, Impl added this session)
- BGE-large production model at 91.2% pipeline R@1 (re-baselined)
- Cosine-only search (RRF disabled — 17pp worse)
- HNSW traversal-time filtering for chunk_type/language
- LLM summaries: preserved across --force via read-before-rename
- Eval: test_fixture_eval_296q, test_noise_eval_143q, run_raw_eval.py
