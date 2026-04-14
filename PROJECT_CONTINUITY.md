# Project Continuity

## Right Now

**v1.24.0 released. Next: investigate CAGRA filtering regression. (2026-04-13 EOD)**

### What shipped in v1.24.0 (PR #939 merged)
1. GPU-native CAGRA bitset filtering — patched cuvs crate, upstream PR rapidsai/cuvs#2019 pending
2. Batch/daemon base index routing — daemon now respects `DenseBase` classification
3. Router update — `type_filtered` + `multi_step` → DenseBase (per enrichment ablation)
4. cuVS 26.2 → 26.4 — fixes daemon SIGABRT under sustained CAGRA load
5. cagra.rs simplified — non-consuming search, −357 lines, `IndexRebuilder` removed
6. CI-safe cuvs patch — git dep on `jamie8johnson/cuvs-patched`

Also merged: dependabot #935-#938 (cuvs, rand, clap_complete, libc).

### Session eval results (for reference)

| Config | R@1 |
|--------|-----|
| Original baseline (pre-session) | 41.5% |
| Base only (no summaries) | 42.3% |
| Enriched only (HNSW over-fetch) | 41.9% |
| Enriched only (CAGRA filter) | 42.6% |
| Oracle routing (theoretical) | 43.8% |
| **v1.24.0 fully routed** | **41.5%** (net zero) |

CAGRA filtering helps negation +10pp, multi_step +3pp but regresses conceptual −5.5pp, structural −3.8pp on enriched.

### Next priorities (in roadmap)
1. **Investigate CAGRA filtering regression** — conceptual/structural regression on enriched. Three hypotheses to try: HNSW for enriched + CAGRA for base, increase itopk_size for filtered queries, separate CAGRA graphs per common filter.
2. **Query-time HyDE for structural queries** — old data shows +14pp, do at query time not index time.
3. **Config file support** — `[splade.alpha]` in `.cqs.toml`.
4. **Alpha re-sweep** — only after CAGRA regression fixed.
5. **Simplify CLAUDE.md** — telemetry showed main conversation uses 5 commands, not 30.

### Open
- PR #940 (roadmap update: add CAGRA regression investigation item)
- Upstream rapidsai/cuvs#2019 (awaiting maintainer vet)

## Architecture (v1.24.0)
- Version: 1.24.0, Schema: v20
- Daemon: `cqs watch --serve` (cuVS 26.4, non-consuming CAGRA, stable across 265+ queries)
- Router: id→NameOnly, type/behavioral/multi/negation→DenseBase, structural/conceptual/cross_lang→enriched
- CAGRA: GPU bitset filtering via patched cuvs
- Per-category SPLADE alpha defaults in `resolve_splade_alpha()`
- LLM summary coverage: 78% of code chunks (6,275 summaries)
- cuVS: 26.4 (libcuvs 26.04, conda, CUDA 13)
- cuvs crate patched via `[patch.crates-io] = { git = "..." }` until upstream merges
