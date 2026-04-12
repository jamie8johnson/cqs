# Project Continuity

## Right Now

**v1.22.0 audit — 78 findings fixed across 14 PRs (12 merged, #910 + #911 in CI). (2026-04-12 12:20 CDT)**

### Session PRs

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
| #907 | OB-19 + EH-8 + API-11 | 3 | **merged** |
| #908 | Triage status + OB-20 begin_write span | 1 | **merged** |
| #910 | AC-1: SPLADE hybrid fusion score preservation | 1 | CI running |
| #911 | P2/P3 batch: 16 findings (4 parallel agents + main) | 16 | CI running |

### Remaining P1 items

None. All P1s addressed in PRs #910 (AC-1) and #911 (DS-W5 + 16 P2/P3s).

### AC-1 fix (PR #910)

Extracted `apply_scoring_pipeline()` from `score_candidate()`. Hybrid search now passes pre-fused scores through the full scoring pipeline instead of discarding them and recomputing pure cosine. Alpha knob is now functional.

### Remaining P2/P3/P4 (~45 items)

See `docs/audit-triage.md`. Main buckets:
- Test coverage: ~24 gaps (adversarial + happy-path)
- API hygiene: API-2/3/4/5/6/7/13
- Extensibility: EXT-7/8/9/10/11/12/13
- Performance: PF-1 (daemon), PF-2/3/5/9/10/11
- Resource: RM-5/6/7
- Scaling: Roadmap CPU (integrity check opt-in)

### SPLADE eval result

Flag-driven SPLADE-Code 0.6B: −0.6pp R@1 net (41.8% vs 42.4%). cross_language +10pp. AC-1 fix means re-eval will now measure actual fusion quality. Selective routing is next.

## Open Issues
- #909 (PF-9 borrow checker), #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v20 (v19 FK CASCADE, v20 AFTER DELETE trigger on chunks)
- Tests: 1351 lib + integration
- `max_rows_per_statement(N)` in `store/helpers/sql.rs` — all 15 SQLite batch sites migrated
- Three on-disk indexes: `index.hnsw.*` + `index_base.hnsw.*` + `splade.index.bin`
- New env vars: CQS_BUSY_TIMEOUT_MS, CQS_IDLE_TIMEOUT_SECS, CQS_MAX_CONNECTIONS, CQS_MMAP_SIZE, CQS_SPLADE_MAX_CHARS, CQS_MAX_QUERY_BYTES, CQS_HNSW_BATCH_SIZE
