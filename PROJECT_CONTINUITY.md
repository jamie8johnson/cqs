# Project Continuity

## Right Now

**Adaptive retrieval Phase 1 done. SPLADE-Code exported. Pausing. (2026-04-09 17:45 CDT)**

### Branch: `feat/adaptive-retrieval`

One commit: `40ad770 feat: Phase 1 — QueryClassifier for adaptive retrieval`
- `src/search/router.rs` — classifier with 9 categories, 3 confidence levels, 4 strategies
- 28 tests (13 happy + 15 adversarial), all passing
- NOT pushed yet — local only

### Also pending
- **PR #868**: `fix/eval-script-and-docs` — eval script --config flag fix. Needs merge.
- **SPLADE-Code 0.6B ONNX**: exported to `~/training-data/splade-code-naver/onnx/` (2.3GB). Verified with ORT. Ready to eval but needs Phase 3 (pre-pooled output support in SpladeEncoder) first — output is (1, 151936) not (1, seq_len, 30522).

### What to do next (in order)
1. Push `feat/adaptive-retrieval` branch
2. Merge PR #868
3. Implement Phase 2: wire classifier into cmd_query (before embedding), add --strategy flag, type boost
4. Implement Phase 3: SpladeEncoder pre-pooled output detection (enables SPLADE-Code eval)
5. Implement Phase 4: telemetry extension
6. Eval SPLADE-Code 0.6B with fixed eval script: `CQS_SPLADE_MODEL=~/training-data/splade-code-naver/onnx python3 evals/run_ablation.py --config bge-large --config bge-large+splade`
7. Phase 5: dual embeddings (v2, schema migration) — only if Phase 2 eval shows routing helps

### Key decisions this session
- SPLADE v2: **NULL** (0.0pp). 110M BERT with SpladeLoss is dead. v3/v4 cancelled.
- SPLADE-Code paper: 600M with KL distillation works. Our failures were capacity + training objective.
- Adaptive retrieval v1: +2-4pp from mechanism routing alone (no schema change)
- Adaptive retrieval v2: +5-10pp from dual embeddings (schema migration v17→v18)
- Summaries are index-time not search-time — critical constraint for routing design
- Paper 2508.21038 proves single-vector limits — justifies dual embeddings theoretically
- Pre-edit impact hook works — fires on every Edit, shows caller count
- Monitor tool available — use for streaming background processes
- Ultraplan = remote Opus session for deep planning (keyword trigger, not slash command)

### Key files
- Classifier: `src/search/router.rs` (Phase 1 complete)
- Plan: `docs/plans/adaptive-retrieval.md` (all 6 phases)
- SPLADE-Code ONNX: `~/training-data/splade-code-naver/onnx/model.onnx` + `.data`
- SPLADE-Code export script: `~/training-data/export_splade_code.py`
- SPLADE v2 eval results: `/tmp/eval_v2.log`
- Claude Code source: `/mnt/c/Projects/collection-claude-code-source-code-main/`

## Parked
- Graph visualization (`cqs serve`) — spec at docs/plans/graph-visualization.md
- Wiki system, Paper v1.0, Phase 6 explainable search
- Phase 5 dual embeddings — after v1 eval proves routing works

## Open Issues
- #856 (PB-5 atexit UB)
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.21.0, Languages: 54, Tests: ~2468 (28 new router tests), Chunk types: 29
- BGE-large + LLM summaries = best production config
- SPLADE: v1 null, v2 null (110M BERT). SPLADE-Code 0.6B exported, pending eval.
- Adaptive retrieval: Phase 1 complete (classifier), Phases 2-5 pending
- Cross-project: 5 commands wired (callers, callees, impact, trace, test-map)
- Write serializer mutex, telemetry file-presence, pre-edit impact hook
