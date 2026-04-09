# Project Continuity

## Right Now

**Adaptive retrieval planned. SPLADE v2 eval running. PR #866 in CI. (2026-04-09 14:00 CDT)**

### What happened this session
- v1.21.0 released (PRs #850-865 merged). Cross-project, 29 chunk types, 14-category audit (40+ fixes), API renames, batch flags, write serializer mutex, path containment.
- SPLADE v2 training complete (199,998 token-overlap negatives, reg_weight 1e-3, 3h43m). ONNX exported. Eval ablation running now (~1h 40m so far, per-query CLI calls are slow).
- SPLADE-Code paper found (Naver Labs, arXiv 2603.22008) — 600M/8B code-specific SPLADE. Export script at ~/training-data/export_splade_code.py. May obsolete our v2/v3/v4 training.
- Agent adoption telemetry analysis: main conversation = search 60% + context 28%. Subagents use impact/callers/test-map. Pre-edit impact hook built (.claude/hooks/pre-edit-impact.sh). Telemetry reset for baseline.
- Adaptive retrieval spec complete: v1 mechanism routing (+2-4pp) + v2 dual embeddings (+10-15pp). 52 tests planned. Spec: docs/plans/adaptive-retrieval.md.
- Graph visualization spec (parked): axum + Cytoscape.js. Spec: docs/plans/graph-visualization.md.
- Research: paper v1.0 rewrite, research split to 7 files, HF model cards updated.
- Notes groomed: 145 → 133.

### Pending right now
- **SPLADE v2 eval**: `evals/run_ablation.py` running with `CQS_SPLADE_MODEL=~/training-data/splade-code-v2/onnx`. Per-query CLI subprocess calls. Output will go to `evals/runs/`. Check with `ps aux | grep run_ablation`.
- **PR #866**: docs/specs PR in CI. Merge when green, then start adaptive retrieval implementation.

### What to do next (in order)
1. Check SPLADE v2 eval results — if null, proceed to SPLADE-Code 0.6B export
2. Merge PR #866
3. Create branch `feat/adaptive-retrieval`
4. Implement Phase 1-5 of adaptive retrieval spec (docs/plans/adaptive-retrieval.md)
5. Export SPLADE-Code 0.6B (~/training-data/export_splade_code.py) when GPU free
6. Paper v1.0 polish with new results

### Key files
- Adaptive retrieval spec: `docs/plans/adaptive-retrieval.md`
- Graph viz spec: `docs/plans/graph-visualization.md`
- SPLADE v2 model: `~/training-data/splade-code-v2/onnx/`
- SPLADE-Code export script: `~/training-data/export_splade_code.py`
- Pre-edit hook: `.claude/hooks/pre-edit-impact.sh`
- Telemetry baseline: `.cqs/telemetry.jsonl` (reset 2026-04-09)

## Parked
- Graph visualization (`cqs serve`) — spec ready, parked
- Wiki system — spec revised (agent-first)
- Paper v1.0 — needs adaptive retrieval + SPLADE-Code results

## Open Issues
- #856 (PB-5 atexit UB)
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.21.0, Languages: 54, Tests: ~2440, Chunk types: 29
- BGE-large + LLM summaries = best production config
- SPLADE: v1 null (off-the-shelf + code-trained). v2 training complete, eval running. SPLADE-Code 0.6B export script ready.
- Eval: v2 (265q), fixture (296q), noise (143q)
- Cross-project: callers, callees, impact, trace, test-map wired
- Write serializer mutex on all store transactions (#853)
- Telemetry: file-presence activation (subagents captured)
- Pre-edit impact hook active
