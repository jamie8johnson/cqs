# Project Continuity

## Right Now

**v1.22.0 audit — 84 findings fixed + 1 won't-fix + 13 issues created. (2026-04-12 13:35 CDT)**

Branch: `fix/audit-p2p3-batch`

### Session PRs

| PR | Theme | Findings | Status |
|---|---|---|---|
| #893–#908 | Prior session audit fixes | 59 | **all merged** |
| #910 | AC-1: SPLADE hybrid fusion score preservation | 1 | **merged** |
| #911 | P2/P3 mega-batch: 25 findings + 10 tests + daemon plan | 25 | CI pending (test queued) |

### What's in #911

**Code fixes (25 findings):**
- DS-W5: watch inode check for DB replacement
- CQ-4: incremental SPLADE encoding (skip already-encoded chunks)
- SHL-34/35/36/37/38/39/40: 7 env-var overrides for pool/batch config
- OB-13/15/16/18: observability (structured logging, fallback logs)
- SEC-NEW-2: telemetry advisory lock parity
- PF-6/8: name match + glob cache perf
- PB-NEW-9: SPLADE non-UTF-8 temp path
- EH-6: embedding batch iterator warn on corrupt
- EXT-7/8/9/11/13: registry lookups, dead variant removal, preset list
- API-12, AC-3, Doc-5, Doc-10: misc hygiene

**Tests added (10):**
- 5 sparse store tests (chunk_splade_texts concatenation, missing query, grouping)
- 3 embeddings/drift tests
- 2 token budget tests (required re-exporting ReviewedFunction+RiskSummary from lib.rs)

**Daemon plan:** `docs/plans/2026-04-12-persistent-daemon.md` — extend `cqs watch` with Unix socket query serving. Fresh-eyes reviewed: 5 design gaps identified, tracing/error handling/test plan added.

### Issues created (#912–#925)

13 architectural items + 1 won't-fix for future audit tracking:
- #912 PF-1: persistent daemon (has plan doc)
- #913 PF-2: query embedding cache
- #914 PF-3: CAGRA disk persist
- #915 PF-10: shared tokio runtime
- #916 PF-11: mmap SPLADE index
- #917 RM-5: streaming SPLADE serialize
- #918 RM-6: Store::clear_caches
- #919 RM-7: read-only store for batch/chat
- #920 PB-NEW-10: streaming sparse load
- #921 PB-NEW-7: SPLADE save blocks watch
- #922 EXT-10: custom_parser seam
- #923 EXT-12: INDEX_DB_FILENAME constant
- #924 Roadmap CPU: integrity check opt-in
- #925 PF-5: won't-fix tracking

### Remaining unfixed audit items (~25)

- API hygiene: API-2/3/4/5/6/7/13, CQ-6/7
- Extensibility: EXT-10/12
- Happy-path test gaps: ~10 remaining (CommandContext splade, BatchContext splade, build_test_map BFS, resolve_parent_context, batch handlers, pipeline fan-out)
- PF-5 won't-fix, PF-9 has issue #909

### SPLADE eval

AC-1 fix merged — alpha knob now functional. Re-eval needed to measure actual fusion quality vs candidate-set expansion.

## Open Issues
- #909–#925, #856, #717, #389, #255, #106, #63

## Architecture
- Version: 1.22.0
- Schema: v20 (v19 FK CASCADE, v20 AFTER DELETE trigger on chunks)
- Tests: ~1370 (lib + bin + integration, pending #911 merge for exact count)
- Three on-disk indexes: `index.hnsw.*` + `index_base.hnsw.*` + `splade.index.bin`
- New env vars: CQS_BUSY_TIMEOUT_MS, CQS_IDLE_TIMEOUT_SECS, CQS_MAX_CONNECTIONS, CQS_MMAP_SIZE, CQS_SPLADE_MAX_CHARS, CQS_MAX_QUERY_BYTES, CQS_HNSW_BATCH_SIZE
