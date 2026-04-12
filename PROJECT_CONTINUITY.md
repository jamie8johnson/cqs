# Project Continuity

## Right Now

**v1.22.0 audit — 72 findings fixed across 10 PRs (9 merged, #907 in CI). (2026-04-12 11:25 CDT)**

### Session PRs (this session = continuation from compacted context)

| PR | Theme | Findings | Status |
|---|---|---|---|
| #893 | Integrity check skip (86s → 6.9s) | 1 | **merged** (prior session) |
| #894 | Eval harness -- separator + timeout | 1 | **merged** (prior session) |
| #895 | SPLADE index persistence (45s → 9.7s) | 1 | **merged** (prior session) |
| #896 | OpenRCT2 spec rewrite | 0 | **merged** (prior session) |
| #897 | Audit docs (136 findings + triage) | — | **merged** |
| #898 | v19 FK CASCADE + generation bump + SPLADE file safety | 22 | **merged** |
| #900 | P3/P4: docs + router + dim + security + resource | 14 | **merged** |
| #901 | v20 AFTER DELETE trigger + watch warn | 3 | **merged** |
| #902 | rrf_k config wiring + dead --semantic-only | 2 | **merged** |
| #903 | SEC-NEW-1 + DS-W6 + RM-2 + EH-5 | 4 | **merged** |
| #904 | SHL-32/33: 15 pre-3.32 SQLite batch sizes | 15 | **merged** |
| #905 | API-1: remove misleading --format from TextJsonArgs | 1 | **merged** |
| #906 | OB-14 + EH-7 + EH-9 + PF-7 + OB-21 | 5 | **merged** |
| #907 | OB-19 + EH-8 + API-11 | 3 | CI running |

### Remaining P1 items

- **AC-1**: SPLADE hybrid fusion rewrite — hard, needs own session. `search_hybrid` discards fused scores; alpha is a no-op on final ranking.
- **DS-W5**: `cqs index --force` inter-process lock — medium.

### Remaining P2/P3/P4 (~65 items)

See `docs/audit-triage.md`. Main buckets:
- Test coverage: ~24 gaps (adversarial + happy-path)
- API hygiene: API-2/3/4/5/6/7/12/13
- Extensibility: EXT-7/8/9/10/11/12/13 (hardcoded lists next to registries)
- Performance: PF-1 persistent daemon (hard), PF-2/3/5/6/8/9/10/11
- Observability: OB-13/18/20
- Error handling: EH-6 (embedding batch drops)

### SPLADE eval result

Flag-driven SPLADE-Code 0.6B: −0.6pp R@1 net (41.8% vs 42.4%). cross_language +10pp. AC-1 finding means all evals measure candidate-set expansion, not fusion. Selective routing is next after the fusion rewrite.

## Open Issues
- #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v20 (v19 FK CASCADE, v20 AFTER DELETE trigger on chunks)
- Tests: 1351 lib + integration
- `max_rows_per_statement(N)` in `store/helpers/sql.rs` — all 15 SQLite batch sites migrated
- Three on-disk indexes: `index.hnsw.*` + `index_base.hnsw.*` + `splade.index.bin`
