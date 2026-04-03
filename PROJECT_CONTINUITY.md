# Project Continuity

## Right Now

**Refactoring wave planned. PR #763 in CI. v1.15.0 released. (2026-04-03 16:43 CDT)**

### Next: Refactoring wave (waiting on PR #763)

**Wave 1 — 4 parallel agents in worktrees (no file overlap):**
1. `cli/mod.rs` split (1161 lines) → `cli/store.rs`, `cli/signal.rs`, `cli/files.rs`
2. `pipeline.rs` split (1303 lines) → `cli/pipeline/parse.rs`, `embed.rs`, `upsert.rs`, `enrich.rs`
3. `store/helpers.rs` split (1222 lines) → by responsibility
4. Telemetry subcommand list → derive from clap `Commands` enum instead of hardcoded strings

**Wave 2 — after wave 1 merges:**
5. Reranker session → lazy field in CommandContext (overlaps cli/mod.rs)
6. Batch/CLI handler unification → shared handlers, different formatters (biggest, ~2000 lines saved)

### BGE-large fine-tuning — complete

| Eval | FT | Baseline | v9-200k |
|------|-----|---------|---------|
| Fixture R@1 (296q) | **91.6%** | 90.9% | 90.5% |
| Raw R@1 (55q) | **66.2%** | 61.8% | **70.9%** |
| Real R@1 (100q) | **50.0%** | **50.0%** | 26.0% |
| Real R@5 (100q) | **73.0%** | 72.0% | 51.0% |
| CoIR (19-subtask) | **57.5** | 55.7 | 52.7 |
| CoIR CSN | **0.779** | 0.721 | 0.615 |

Published: [jamie8johnson/bge-large-v1.5-code-search](https://huggingface.co/jamie8johnson/bge-large-v1.5-code-search)
v9-200k model card also updated with corrected numbers.

### Data integrity fixes this session
- Real eval script couldn't read expanded queries (schema mismatch) — fixed, all models re-run
- CoIR "Overall" used mixed averaging (19-subtask for BGE, 9-task for E5 models) — corrected all
- v9-200k collapsed from 49% to 26% R@1 on post-restructure codebase
- RESULTS.md, research_log.md, paper draft all corrected

### Uncommitted changes
- `CLAUDE.md` — preamble simplified to "Remain calm. There is no rush."
- `scripts/run_real_eval.py` — fixed expanded eval format
- `PROJECT_CONTINUITY.md`, `ROADMAP.md` — updated with results

### This session summary
- BGE-large training completed (12.75h), ONNX exported, all evals run
- 10 PRs merged (#753-762), v1.15.0 released
- Model published to HuggingFace
- Cleaned 50 stale remote branches, removed cu11 pip packages

## Parked
- Dart, hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB) — fits current board (Z590/i9-11900K, PCIe 4.0 x16, Seasonic GX-1300 has 12VHPWR)
- Publish datasets to HF
- Ladder logic (RLL) tree-sitter grammar
- Batch/CLI handler unification (wave 2, after splits land)
- v9-200k deep analysis (5 experiments in ROADMAP + research_log)
- Crates.io publish blocked by tree-sitter-structured-text git dep
- Consider FT BGE-large as new default model

## Open Issues
- #717, #389, #255, #106, #63

## Architecture
- Version: 1.15.0, Languages: 52 + L5X/L5K, Commands: 54+, Tests: ~2196
- Best model: BGE-large FT (91.6% R@1, 57.5 CoIR)
- Production default: BGE-large baseline (identical on real code)
- Published models: jamie8johnson/bge-large-v1.5-code-search, jamie8johnson/e5-base-v2-code-search
- 6 custom agents, CommandContext struct, 7 command subdirectories
- Schema v16
