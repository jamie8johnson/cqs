# Project Continuity

## Right Now

**Preparing v0.9.8 release.** 2026-02-11.

### Active
- v0.9.7 audit complete. 125 fixes across 9 PRs (#333-#343). 5 P4 deferred as issues. 11 P3 deferred (lower ROI).
- Preparing v0.9.8 release with all audit fixes

### Completed This Session
- PR8: Created 2 new P4 issues (#344 TC6, #345 X8). 3 existing (#269, #236, #302).
- PR7c (P3 batch 3): merged as PR #343 — 7 fixes
- PR7b (P3 batch 2): merged as PR #341 — 14 fixes
- PR7a (P3 batch 1): merged as PR #340 — 13 fixes
- PR6 (API+Algorithm): merged as PR #338 — 13 fixes
- PR5 (Safety+Security): merged as PR #337 — 14 fixes

### Completed Prior Sessions
- PR4 (Performance): merged as PR #336 — 10 fixes
- PR3 (Mechanical): merged as PR #335 — 33 fixes
- PR2 (Critical Bugs): merged as PR #334 — 10 fixes
- PR1 (Foundation): merged as PR #333 — 11 fixes
- Notes groomed: 8 removed, 2 updated, 4 added (79 → 75)
- Release binary updated, index rebuilt (2249 chunks, 9316 calls)

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

- Version: 0.9.8 (pending release)
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- 339 lib + 243 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (also available as CLI commands now)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
