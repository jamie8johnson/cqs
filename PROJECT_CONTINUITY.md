# Project Continuity

## Right Now

**v1.19.0 shipped. SPLADE training at 26%. CPU lane: 6-item dev sprint. (2026-04-07 CDT)**

### GPU Lane: SPLADE fine-tuning (RUNNING)
- PID 182066, A6000 100%, step ~3,200/12,250 (26%), ETA ~midnight CDT
- Checkpoints every 500 steps, tensorboard at `~/training-data/splade-code-v1/tb_logs`
- Resume: `cd ~/training-data && python3 train_splade.py --resume`
- Config: LoRA r=16, lr=2e-5, batch=32, reg_weight=5e-4, 2 epochs

### CPU Lane: 6-item sprint (in order)
1. **`--include-type`/`--exclude-type` rename** — rename `--chunk-type` to `--include-type`, add `--exclude-type`. Update CLI, batch, skills, agents, all .md files, install binary. Branch: `feat/search-filter-rename`. IN PROGRESS.
2. **Java/C# test detection** — `@Test`/`[Test]`/`[Fact]` attributes → Test via post_process. Same pattern as Rust `#[test]`.
3. **Java/C# endpoint detection** — `@GetMapping`/`[HttpGet]` annotations → Endpoint. Same pattern as Python Flask.
4. **Audit weak chunk type tests** — scan 34 languages with post_process for tests passing due to silent fix. Mechanical grep + verify.
5. **Refactor batch `--rrf` opt-in** — rename `semantic_only` to `rrf: bool`, wire as `--rrf` flag. Dead code cleanup.
6. **Expand eval to 300q** — generator auto-handles identifier/structural/type-filtered. Need ~150 hand-curated behavioral/conceptual/negation/multi-step/cross-language.

### What still needs to happen (after sprint)
- [ ] Evaluate code-trained SPLADE (when training completes)
- [ ] ONNX export + integration test of trained SPLADE
- [ ] Code-trained reranker experiment
- [ ] Release v1.20.0

### This session (8 PRs merged, 2 releases)
- PR #831-837 all merged. v1.18.0 + v1.19.0 released.
- Full ablation: BGE-large × E5-LoRA × SPLADE × reranker × LLM summaries
- Best config: BGE-large + LLM summaries, no SPLADE, no reranker

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
- Capture lists unified: `ChunkType::CAPTURE_NAMES` (one source of truth)
- BGE-large + LLM summaries = best production config
- Cosine-only search (RRF disabled, SPLADE null, reranker negative)
- Store dim check prevents cross-model embedding contamination
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db
- Eval: v2 harness (112q), fixture (296q), noise (143q)
- Query logging: batch mode → ~/.cache/cqs/query_log.jsonl
