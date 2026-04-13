# Project Continuity

## Right Now

**Enrichment ablation complete, router updated, ready to commit. (2026-04-13 15:10 CDT)**

### Enrichment ablation results (78% summary coverage, SPLADE active)

| Category | N | Base | Enriched | Best | Δ |
|---|---|---|---|---|---|
| identifier_lookup | 50 | **100.0%** | 98.0% | base | +2.0pp |
| type_filtered | 24 | **41.7%** | 33.3% | base | +8.4pp |
| behavioral_search | 44 | **29.5%** | 27.3% | base | +2.2pp |
| multi_step | 34 | **23.5%** | 20.6% | base | +2.9pp |
| structural_search | 27 | 48.1% | **51.9%** | enriched | −3.8pp |
| conceptual_search | 36 | 27.8% | **33.3%** | enriched | −5.5pp |
| cross_language | 21 | 19.0% | **23.8%** | enriched | −4.8pp |
| negation | 29 | 13.8% | 13.8% | either | 0 |

Oracle routing: **43.8% R@1** (+1.9pp over enriched-only, +1.5pp over base-only)

### Router updated
- type_filtered → DenseBase (was DenseWithTypeHints enriched)
- multi_step → DenseBase (was DenseDefault enriched)
- Tests updated, all passing

### Code changes this session (uncommitted)
- `src/search/router.rs` — type_filtered + multi_step → DenseBase, updated comments + tests
- `src/cli/batch/handlers/search.rs` — base/enriched HNSW routing in batch handler
- `src/cli/batch/mod.rs` — `base_hnsw` field + `base_vector_index()` method
- `src/cli/commands/search/query.rs` — `CQS_FORCE_BASE_INDEX` env var check
- `src/cagra.rs` — itopk_size warning demoted to debug
- `Cargo.toml` — fixed stale cuvs comment
- `Cargo.lock` — dep bumps from merged PRs (#935-#938)

### Session work
- cuVS bumped 26.2→26.4 (conda + PR #935 merged). Fixed daemon CAGRA segfault.
- Dependabot PRs merged: #935 (cuvs), #936 (rand), #937 (clap_complete), #938 (libc)
- Enrichment ablation: batch handler lacked base index support → fixed properly
- cuVS 26.04 investigation: non-consuming search, filtered CAGRA (FFI only), persistence fix

### What's next
- Commit + PR this session's changes
- Per-category hyde eval (assess impact with SPLADE)
- Config file support for `[splade.alpha]`
- cuVS: simplify cagra.rs (non-consuming search), upstream filtered search PR
- Daemon: incremental SPLADE in watch mode

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.23.0, Schema: v20
- Daemon: `cqs watch --serve` (3-19ms graph queries, cuVS 26.4 stable)
- Per-category SPLADE alpha defaults in resolve_splade_alpha()
- Dual HNSW routing: base (id/type/behavioral/multi/negation) + enriched (structural/conceptual/cross_lang)
- Query cache: `~/.cache/cqs/query_cache.db`
- LLM summary coverage: 78% of code chunks (6,275 summaries)
- cuVS: 26.4 (libcuvs 26.04, conda, CUDA 13)
