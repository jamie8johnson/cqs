# Project Continuity

## Right Now

**v1.22.0 audit — 87 findings fixed + 1 won't-fix + 13 issues created. PR #911 in CI. (2026-04-12 14:00 CDT)**

Branch: `fix/audit-p2p3-batch`

### Session PRs

| PR | Theme | Findings | Status |
|---|---|---|---|
| #893–#908 | Prior session audit fixes | 59 | **all merged** |
| #910 | AC-1: SPLADE hybrid fusion score preservation | 1 | **merged** |
| #911 | Mega-batch: 28 findings + 10 tests + 3 perf issues + daemon plan | 28 | CI running |

### What's in #911 (28 findings + 3 issue fixes)

**Audit findings (25 from prior push + 3 new):**
- All prior P2/P3 items (DS-W5, CQ-4, SHL-*, OB-*, EXT-*, PF-*, etc.)
- #924: Integrity check flipped to opt-in (CQS_INTEGRITY_CHECK=1)
- #919: Batch/chat store opened read-only (skip write pool + quick_check)
- #918: Store::clear_caches replaces drop+reopen churn in watch

**Tests (10):** sparse store, embeddings, drift, token budget

**Daemon plan:** `docs/plans/2026-04-12-persistent-daemon.md` — extend `cqs watch` with Unix socket. Fresh-eyes reviewed. #913 + #915 scoped into Phase 0.

### Issues created (#912–#925)

14 issues total. Top 3 by ROI (#924, #919, #918) now fixed in #911.

### Next session

- **Daemon implementation** (#912) — Phase 0a/0b/0c then Phases 1-5. ~360 lines.
- **SPLADE re-eval** — AC-1 merged, alpha knob now functional. Re-run eval.
- **Remaining audit items** (~22): API hygiene, extensibility, happy-path test gaps.

## Open Issues
- #909–#925, #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v20 (v19 FK CASCADE, v20 AFTER DELETE trigger on chunks)
- Tests: ~1375 (pending #911 merge for exact count)
- Three on-disk indexes: `index.hnsw.*` + `index_base.hnsw.*` + `splade.index.bin`
- New env vars: CQS_BUSY_TIMEOUT_MS, CQS_IDLE_TIMEOUT_SECS, CQS_MAX_CONNECTIONS, CQS_MMAP_SIZE, CQS_SPLADE_MAX_CHARS, CQS_MAX_QUERY_BYTES, CQS_HNSW_BATCH_SIZE, CQS_INTEGRITY_CHECK
- Store::clear_caches() replaces drop+reopen in watch (RM-6/#918)
- Batch/chat opens read-only store (RM-7/#919)
