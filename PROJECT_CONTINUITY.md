# Project Continuity

## Right Now

**Session complete. CAGRA stabilized, enrichment ablated, upstream PR filed. (2026-04-13 17:00 CDT)**

### Session eval results

| Config | R@1 | Notes |
|--------|-----|-------|
| Original production baseline | 41.5% | Pre-session |
| Base only (no summaries) | 42.3% | All queries → base HNSW |
| Enriched only (HNSW over-fetch) | 41.9% | All queries → enriched, 3x over-fetch |
| Enriched only (CAGRA filter) | 42.6% | All queries → enriched, GPU bitset filter |
| Oracle routing (theoretical) | 43.8% | Best arm per category |
| Fully routed (clean index) | 41.5% | Router + CAGRA filter — net zero vs baseline |

**CAGRA filtering helps some categories but regresses others.** Negation +10pp, multi_step +3pp, but conceptual −5.5pp, structural −3.8pp. Investigation needed: CAGRA bitset vs HNSW traversal filtering return different candidates on enriched index.

### What shipped
1. Enrichment ablation — 2-arm eval, per-category summary impact
2. Router update — type_filtered + multi_step → DenseBase
3. Batch base index support — daemon routes to base/enriched correctly
4. cuVS 26.2→26.4 — fixed daemon segfault
5. CAGRA simplified — removed IndexRebuilder (−357 lines), non-consuming search
6. CAGRA native bitset filtering — GPU-side type/language filter
7. Upstream PR — rapidsai/cuvs#2019 (search_with_filter)
8. Dependabot PRs merged — #935 (cuvs), #936 (rand), #937 (clap_complete), #938 (libc)

### PR #939 (open, 4 commits)
All changes on `feat/enrichment-ablation-routing` branch.

### Next session priorities
1. **Investigate CAGRA filtering regression** — conceptual −5.5pp, structural −3.8pp on enriched. Hypothesis: CAGRA graph walk strands in filtered-out regions. Options: HNSW for enriched + CAGRA for base, or increase itopk_size for filtered queries.
2. **Merge PR #939** after CI passes
3. **Alpha re-sweep** — only after retrieval path is stable
4. **Query-time HyDE** for structural queries
5. **Simplify CLAUDE.md** — slim agent adoption

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.23.0, Schema: v20
- Daemon: `cqs watch --serve` (cuVS 26.4, non-consuming CAGRA, stable)
- Router: id→NameOnly, type/behavioral/multi/negation→DenseBase, structural/conceptual/cross_lang→enriched
- CAGRA: GPU bitset filtering via patched cuvs (upstream PR rapidsai/cuvs#2019)
- Per-category SPLADE alpha defaults in resolve_splade_alpha()
- LLM summary coverage: 78% of code chunks (6,275 summaries)
- cuVS: 26.4 (libcuvs 26.04, conda, CUDA 13)
