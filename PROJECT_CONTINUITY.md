# Project Continuity

## Right Now

**v0.9.7 audit fix session.** 2026-02-10.

### Active
- v0.9.7 audit complete. 125 fixes across PRs 1-7c + 5 P4 issues filed. 11 P3 deferred (lower ROI).
- Remaining P3 (unfixed): TC2, TC3, TC5, TC14, A9, P8, RM1, RM3, RM4, X7

### Completed This Session
- PR7c (P3 batch 3): merged as PR #343 — 7 fixes
  - Test coverage: TC4 (search_filtered), TC8 (chunks.rs), TC9 (reference.rs) — 30 new tests
  - Data safety: DS12 (SQLite integrity check on open)
  - Performance: P11 (gather call graph doc)
  - API docs: A2 (callers/callees asymmetry), RM8 (embedder lifecycle + clear_session)
- PR7b (P3 batch 2): merged as PR #341 — 14 fixes
  - Test coverage: TC7, TC10, TC11, TC13, TC15
  - Performance: P6 (FTS dedup), P9 (pipeline clone), DS9 (cursor pagination)
  - Security: S1 (FTS sanitization)
  - API docs: A6, A8 (verified), EH12, S8 (verified), RM10 (documented)
- PR7a (P3 batch 1): merged as PR #340 — 13 fixes
  - Data safety: DS1, DS10, DS13
  - Algorithm: AC8, AC6, A10, RM11
  - Perf: P12. Observability: O10, O11
  - Platform: PB7, S7, PB8
- PR6 (API+Algorithm): merged as PR #338 — 13 fixes
- PR5 (Safety+Security): merged as PR #337 — 14 fixes

### Completed Prior Sessions
- PR4 (Performance): merged as PR #336 — 10 fixes
- PR3 (Mechanical): merged as PR #335 — 33 fixes
- PR2 (Critical Bugs): merged as PR #334 — 10 fixes
- PR1 (Foundation): merged as PR #333 — 11 fixes
- Release binary updated after each merge

### Plan
Full audit fix plan at `/home/user001/.claude/plans/witty-strolling-melody.md`
- 8 PRs, ~141 fixes, dependency-ordered
- PR1: Foundation (11) → PR2: Bugs (10) → PR3: Mechanical (33) → PR4: Performance (10) → PR5: Safety+Security (14) → PR6: API+Algorithm (13) → PR7: P3 Polish (45) → PR8: P4 Issues (5)
- Fresh-eyes reviewed twice: fixed 10 file conflicts, 3 missing findings, 4 wrong counts

### Earlier this session
- Ran full 14-category audit (3 batches, 14 agents) → 161 raw findings
- Triaged to 144 unique (P1:49, P2:45, P3:45, P4:5) in `docs/audit-triage.md`
- Updated audit skill with cqs tools block for agents
- Archived previous audit files as v0.9.1

### Pending
- `.cqs.toml` — untracked, has aveva-docs reference config

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files, 38 cross-referenced docs still missing. User converting more PDFs.
- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references (substring matching of chunk names across docs)
- **Phase 8**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Multi-index follow-ups
- #255: Pre-built reference packages
- #256: Cross-store dedup
- #257: Parallel search + shared Runtime

### Remaining audit items (P4 deferred)
- #269: Brute-force search loads all embeddings
- #236: HNSW-SQLite freshness validation
- #302: CAGRA OOM guard
- #344: embed_documents tests
- #345: MCP schema generation from types

## Architecture

- Version: 0.9.7
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- 310 lib + 243 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (also available as CLI commands now)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
