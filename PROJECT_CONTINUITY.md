# Project Continuity

## Right Now

**v1.19.0 shipped. CPU sprint complete. SPLADE training at 47%. (2026-04-07 CDT)**

### GPU Lane: SPLADE fine-tuning (RUNNING)
- PID 182066, A6000 100%, step ~5,770/12,250 (47%), ETA ~midnight CDT
- Checkpoints every 500 steps, tensorboard at `~/training-data/splade-code-v1/tb_logs`
- Resume: `cd ~/training-data && python3 train_splade.py --resume`

### CPU Lane: sprint COMPLETE
All 6 items done, PRs merged:
1. ~~`--include-type`/`--exclude-type` rename~~ — PR #838
2. ~~Java/C# test detection~~ (`@Test`, `[Test]`, `[Fact]`) — PR #838
3. ~~Java/C# endpoint detection~~ (`@GetMapping`, `[HttpGet]`) — PR #838
4. ~~Audit weak chunk type tests~~ — deferred (separate PR)
5. ~~Batch `--rrf` opt-in~~ — PR #838
6. ~~Expand eval to 265q~~ — PR #838 + #839 (21 fixes, 28 removed)

### What still needs to happen
- [ ] Evaluate code-trained SPLADE (when training completes ~midnight)
- [ ] ONNX export + integration test of trained SPLADE
- [ ] Code-trained reranker experiment
- [ ] Release v1.20.0 (filter rename + Java/C# detection + 265q eval + SPLADE if it works)

### This session (14 PRs merged, 2 releases)
- PRs #821-839 all merged. v1.18.0 + v1.19.0 released.
- Full ablation: BGE-large × E5-LoRA × SPLADE × reranker × LLM summaries
- Best config: BGE-large + LLM summaries, no SPLADE, no reranker
- 265-query eval set across 8 categories, ground truth validated
- SPLADE fine-tuning running on A6000

### Ablation results (v2 eval, 75 train queries)
| Config | R@1 | R@5 |
|--------|-----|-----|
| BGE-large (baseline) | 68.0% | 86.7% |
| + LLM summaries | 69.3% | 85.3% |
| + SPLADE (off-the-shelf) | 68.0% | 86.7% |
| E5-LoRA v9-200k | 54.7% | 76.0% |

## Parked
- Wiki system — spec revised (agent-first), parked for review
- Cross-project call graph — spec ready
- Code-trained reranker — after SPLADE and eval expansion
- Ladder logic (RLL), hnswlib-rs, DXF, Openclaw PLC
- Blackwell RTX 6000, L5X files, Paper v0.7

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.19.0, Languages: 53 + L5X/L5K, Tests: ~2360, Chunk types: 27
- `--include-type`/`--exclude-type` (renamed from `--chunk-type`, alias kept)
- Java/C# test detection (`@Test`, `[Test]`, `[Fact]`) + endpoint detection (`@GetMapping`, `[HttpGet]`)
- Capture lists unified: `ChunkType::CAPTURE_NAMES`
- BGE-large + LLM summaries = best production config
- Cosine-only search (RRF disabled, SPLADE null, reranker negative)
- Store dim check prevents cross-model embedding contamination
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db
- Eval: v2 harness (265q), fixture (296q), noise (143q)
- Query logging: batch mode → ~/.cache/cqs/query_log.jsonl
