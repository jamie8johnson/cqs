# Project Continuity

## Right Now

**Eval infrastructure 4/6 done. Enrichment ablation complete. (2026-03-31 22:52 CDT)**

Branch: `feat/eval-diagnostics` (PR #740)

### Enrichment ablation results (major finding)
| Layer Skipped | R@1 | Delta |
|---------------|-----|-------|
| None (full) | 91.6% | — |
| doc | 84.8% | **-6.8pp** |
| filecontext | 87.5% | -4.1pp |
| signatures | 90.2% | -1.4pp |
| callgraph | 91.2% | -0.4pp |
| parent | 91.2% | -0.4pp |

Doc comments are the #1 enrichment layer. Call graph is nearly irrelevant.

### Eval infrastructure status
1. [x] Per-query diagnostics (CQS_EVAL_OUTPUT)
2. [x] Cross-run stability script
3. [x] Enrichment ablation (CQS_SKIP_ENRICHMENT)
4. [x] Difficulty tiers + weighted R@1
5. [ ] Multi-answer queries (also_accept) — next
6. [ ] Real codebase eval

### Session totals
- 12 PRs (#728-740), v1.13.0 released
- 132 audit findings, ~55 fixed, 3-phase agent plan executed
- IEC 61131-3 (52nd language), `cqs reconstruct`, 12 env var overrides
- Paper v0.9, enrichment ceiling confirmed (per-query v9 vs v5 diff)
- RRF off by default, embedder pre-warm, 5,445 lines doc bloat stripped
- Claude Code source analyzed (19K chunks)

### Next
1. Item 5: multi-answer queries (populate also_accept)
2. Merge PR #740
3. Update paper with ablation data
4. Re-eval nomic/GTE-Qwen2 with correct windowing
5. Real codebase eval (tokio/axum)

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1
- Openclaw variant for PLC process control
- BGE-large fine-tuning + CoIR
- Publish 500K/1M datasets to HF

## Open Issues (cqs)
- #717 RM-40 (HNSW fully in RAM, no mmap)
- #389 (upstream cuVS CAGRA memory)
- #255, #106, #63 (upstream deps)

## Architecture
- Version: 1.13.0
- Languages: 52
- Commands: 52+
- Env overrides: 14 (added CQS_SKIP_ENRICHMENT, CQS_EVAL_OUTPUT)
- Enrichment hierarchy: doc (+6.8pp) > filecontext (+4.1pp) >> signatures (+1.4pp) >> callgraph ≈ parent (+0.4pp)
