# Project Continuity

## Right Now

**Phase 1 agents running (4/7 done). PR #738 open. (2026-03-31 18:53 CDT)**

Branch: `fix/v1.13-audit-p2-batch2`

### Phase 1 status (configurable constants)
| Agent | Finding | File | Status |
|-------|---------|------|--------|
| A1 | SHL-6 HNSW params | hnsw/mod.rs | Running |
| A2 | SHL-12 embed batch | cli/pipeline.rs | Running |
| A3 | SHL-2 reranker max_length | reranker.rs | Done |
| A4 | SHL-8 gather BFS cap | gather.rs | Done |
| A5 | SHL-9 impact BFS cap | impact/bfs.rs | Running |
| A6 | SHL-11 rayon threads | reference.rs + project.rs | Done |
| A7 | SHL-15 query cache | embedder/mod.rs | Done |

### After Phase 1 completes
1. Build check all 7 changes together
2. Quality audit: tracing, error handling, robustness (invalid env values)
3. Commit + push to PR #738
4. Phase 2: tests (TC-2, TC-4) + nits (PB-2, SEC-5, EH-1)
5. Phase 3: shared serialization (CQ-1/3/5)

### PR #738 contents so far
- PERF-6: finalize_results remove() vs clone()
- PERF-2: batch FTS upsert (22K→batched)
- RM-5: contrastive neighbor buffer reuse
- SEC-3: ONNX_DIR symlink containment
- CQS_MAX_SEQ_LENGTH + CQS_EMBEDDING_DIM env overrides
- `cqs reconstruct <file>` command (source from index)
- Phase 1 changes incoming (7 configurable constants)

### Session totals
- v1.13.0 released, 9 PRs merged (#728-737)
- IEC 61131-3 (52nd language)
- Paper v0.9
- 132 audit findings, 47 fixed + Phase 1 in progress
- 153GB disk freed
- `cqs reconstruct` new command
- Coordinated 3-phase plan in docs/plans/

### OpenClaw — 7 PRs, 6 issues

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1 (P&ID → PLC function block mapping)
- Openclaw variant for PLC process control
- Blackwell GPU upgrade
- Publish 500K/1M datasets to HF
- Re-eval GTE-Qwen2 + nomic with correct windowing
- BGE-large CoIR run

## Open Issues (cqs)
- #717 RM-40 (HNSW fully in RAM, no mmap)
- #389 (upstream cuVS CAGRA memory)
- #255, #106, #63 (upstream deps)

## Architecture
- Version: 1.13.0
- Languages: 52
- Presets: BGE-large (default, 1024d), E5-base (768d), v9-200k (768d)
- Env overrides: CQS_MAX_SEQ_LENGTH, CQS_EMBEDDING_DIM, CQS_MAX_CONTRASTIVE_CHUNKS, CQS_HNSW_M/EF_CONSTRUCTION/EF_SEARCH (Phase 1), CQS_EMBED_BATCH_SIZE, CQS_GATHER_MAX_NODES, CQS_IMPACT_MAX_NODES, CQS_RAYON_THREADS, CQS_QUERY_CACHE_SIZE, CQS_RERANKER_MAX_LENGTH
- Commands: 51+ (added reconstruct)
- Tests: ~1540
- Hooks: Pre-Edit (module context), Pre-Bash (git commit → cqs review)
