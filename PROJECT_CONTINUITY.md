# Project Continuity

## Right Now

**187-query real eval complete. All files updated. (2026-04-01 12:30 CDT)**

### 187-Query Real Eval Results (authoritative)

| Model | Lookup R@1 (100q) | Lookup R@5 (100q) | Conceptual (40q) | GitBlame (27q) |
|-------|-------------------|-------------------|-------------------|----------------|
| **v9-200k** | **49.0%** | **71.0%** | 24/40 | 6/27 |
| BGE-large | 48.0% | 71.0% | 24/40 | 6/27 |
| nomic | 32.0% | 56.0% | 17/40 | 2/27 |

v9-200k and BGE-large virtually tied. Fixture inflation 42-54pp. R@5 of 71% is the agent-relevant metric.

### Results log gaps (remaining)
1. BGE-large CoIR (headline model, no benchmark)
2. v5/v7/v7b/v8 on 296q expanded fixture eval
3. v5 real eval (dimension mismatch — needs CQS_EMBEDDING_DIM=768)
4. Enrichment ablation on v9-200k (only BGE-large tested)
5. Other fine-tunings (v7/v7b/v8) on 187q real eval

### This session
- 187-query real eval framework built + run (50 fn lookup + 40 conceptual queries added)
- Updated: RESULTS.md, research_log.md, paper v0.10, ROADMAP.md, PROJECT_CONTINUITY.md
- Index restored to BGE-large (default)

### Pending
- Update paper with real eval findings — done (Section 6.6, finding #8)
- Commit new eval files to main (real_eval_expanded.json, updated run_real_eval.py)

## Parked
- Dart, hnswlib-rs, DXF, Openclaw PLC, Blackwell
- BGE-large fine-tuning + CoIR, GTE-Qwen2 (OOM risk)
- Publish datasets to HF

## Open Issues
- #717, #389, #255, #106, #63

## Architecture
- Version: 1.13.0, Languages: 52, Commands: 52+
- Search: code-only default, RRF off, 14 env vars
- Real eval (187q): v9-200k 49% > BGE-large 48% >> nomic 32% (R@1), 71%/71%/56% (R@5)
- Fixture eval (296q): 90.5% / 90.9% / 85.5% (inflated by 42-54pp)
