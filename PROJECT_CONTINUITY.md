# Project Continuity

## Right Now

**v1.16.0 released. PR #818 in CI (ConfigKey + eval fixes). Band mining running. (2026-04-05 CDT)**

### PR #818 — ConfigKey + eval cleanup
- New `ConfigKey` chunk type for JSON/TOML/YAML/INI (were `Property`, polluted code search)
- Code-only filter added to batch mode search (was unfiltered — agent bug)
- Code-only filter added to noise eval
- Eval renames: `test_fixture_eval_296q`, `test_noise_eval_143q`, `run_raw_eval.py`, `run_model_eval.sh`
- Dead eval scripts deleted, one source of truth per metric

### Band mining (Exp #1)
Running on A6000. Training at ~6% (was set back by GPU contention from aborted reindex). Mining model: original v9-200k, margin=0.05, band [20,50). Monitor: `tail -f ~/training-data/exp-band/experiment.log`

### Pending after training finishes
1. Reindex with `cqs index --force --llm-summaries` (killed earlier due to GPU contention)
2. Rerun real-code eval to measure ConfigKey + LLM summaries impact
3. Band mining eval results

### LLM summaries
4,813 summaries generated (~$0.87). Cached by content_hash. Need reindex to bake into embeddings (pending GPU).

### Session PRs (#810-818)
- #810-813: audit fixes + v1.15.2 release
- #814: session artifacts
- #815: language macro v2 (52 files → 2 + queries)
- #816: Dart (53rd language) + docs review + roadmap cleanup
- #817: v1.16.0 release
- #818: ConfigKey chunk type + eval fixes (in CI)

### Training results
- Margin sweep (Exp #2): null result. Default 0.05 confirmed.
- Band mining (Exp #1): in progress

## Parked
- Cross-project call graph — spec ready
- Embedding cache — spec ready
- Wiki system — spec ready (standalone design)
- SSD fine-tuning: band mining running, iterative self-distillation next
- Ladder logic (RLL) grammar
- hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- Reranker V2 experiments
- Refactor: replace hardcoded capture lists in chunk.rs/mod.rs with capture_name_to_chunk_type()

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.16.0, Languages: 53 + L5X/L5K, Commands: 54+, Tests: ~2330
- Best model: BGE-large FT (91.6% R@1 fixture)
- Real-code eval: 50% R@1, 73% R@5 (BGE-large, 100q, pre-summaries)
- CI: rust-cache, ~16m test
- 21 chunk types (added ConfigKey for data formats)
- Language macro v2: `languages.rs` + `queries/*.scm`
- 10th audit: 103/103 fixed
