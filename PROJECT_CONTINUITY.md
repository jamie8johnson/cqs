# Project Continuity

## Right Now

**v0.9.7 audit fix session.** 2026-02-10.

### Active
- Next: PR3 (Mechanical Improvements) — 33 fixes across eprintln→tracing, docs, observability, error handling, permissions, extensibility

### Completed
- PR2 (Critical Bugs): merged as PR #334 — 10 bug fixes
  - nl.rs: floor_char_boundary for CJK-safe truncation (R7/S10)
  - notes.rs: char_indices preview truncation (R8/EH13)
  - search.rs: windowed chunk ID path + resolve_target error on filter miss (AC10, EH11)
  - calls.rs: TRAIT_METHOD_NAMES set for dead code detection (AC5)
  - cli/mod.rs: limit clamp 1..100 (R4)
  - gather.rs: deterministic sort + search_degraded flag (AC2, EH4)
  - impact.rs: tracing::warn instead of .ok() (EH5/EH6)
  - tests: non-tautological assertions (TC1, TC12)
- PR1 (Foundation): merged as PR #333 — extracted shared modules + dedup (11 fixes)
- Release binary updated after PR2 merge

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
- TC6: embed_documents tests (no issue yet)
- X8: MCP schema generation from types (no issue yet)

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
