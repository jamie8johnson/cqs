# Project Continuity

## Right Now

**v1.15.0 released. BGE-large training ~59%. Clean repo. (2026-04-02 21:30 CDT)**

### BGE-large LoRA training (detached)
Service: `bge-training`. Step 3500/5938 (58.9%). Train loss 0.165, eval loss 0.078. ~8.6s/step. ETA ~03:15 CDT Apr 3.
Fixed `PeftResumableTrainer` in `train_lora.py` for sentence-transformers 5.3.0 + peft 0.18.1 checkpoint resume bug.
Output: `~/training-data/bge-large-lora-v1/`

**After training completes:**
1. Check `ls ~/training-data/bge-large-lora-v1/onnx/model.onnx`
2. Copy tokenizer: `cp ~/training-data/bge-large-lora-v1/merged/tokenizer.json ~/training-data/bge-large-lora-v1/onnx/`
3. Test: `CQS_ONNX_DIR=~/training-data/bge-large-lora-v1/onnx CQS_EMBEDDING_DIM=1024 cqs index --force`
4. Run 296q fixture eval + 187q real eval
5. Compare to BGE-large baseline (90.9% fixture, 48% real, 55.71 CoIR)

### This session (9 PRs, v1.15.0 released)
- #753: 6 custom agents (.claude/agents/), all tested
- #754: `cqs telemetry` dashboard
- #755: L5X parser (Rockwell PLC exports)
- #756: CLAUDE.md "Remain calm. There is no rush." + "When Stuck" section
- #757: CommandContext refactor (32 handlers)
- #758: L5K parser (legacy Rockwell format)
- #759: Docs audit (stale R@1 numbers, missing refs)
- #760: Commands subdirectory restructure (46 files → 7 subdirectories)
- #761: Release v1.15.0

### Key decisions this session
- "Remain calm. There is no rush." — based on Anthropic emotion concepts research showing desperate vector drives hacky solutions
- Three-strike rule for failed fixes — stop, reassess, dispatch agent
- CommandContext pattern for shared CLI state
- Commands grouped by theme (search/graph/review/index/io/infra/train) for agent navigability
- L5X/L5K use regex extraction → ST tree-sitter, not XML injection (CDATA per-line prevents set_included_ranges)

### Crates.io publish blocked
`tree-sitter-structured-text` is a git dependency without version — blocks `cargo publish`. Need to publish that crate first or pin a version. Existing issue, not new.

## Parked
- Dart, hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB) — fits current board (Z590, PCIe 4.0 x16, Seasonic GX-1300 has 12VHPWR cable)
- Publish datasets to HF
- Ladder logic (RLL) tree-sitter grammar (~50-80 lines, textual DSL in L5X CDATA)
- cli/mod.rs split (1161 lines), pipeline.rs split, store/helpers.rs split
- Batch/CLI handler unification
- v9-200k deep analysis (5 experiments in ROADMAP + research_log)

## Open Issues
- #717, #389, #255, #106, #63

## Architecture
- Version: 1.15.0, Languages: 52 + L5X/L5K, Commands: 54+, Tests: ~2196
- 6 custom agents in .claude/agents/
- `cqs telemetry` command
- CommandContext struct in dispatch
- Commands in 7 subdirectories (search/graph/review/index/io/infra/train)
- Schema v16, HNSW 11,398 vectors
- Binary rebuilt and installed at v1.15.0
