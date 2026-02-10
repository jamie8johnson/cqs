# Project Continuity

## Right Now

**Bug fixes session.** 2026-02-10. Reference index bugs #318 and #319 fixed.

### What shipped
- PR #330: fix ref remove cleanup + ref update prune guard (fixes #318, #319)
  - `ref remove` falls back to canonical `ref_path()` for directory cleanup
  - `Store::init` uses `INSERT OR REPLACE` for metadata resilience
  - `ref update` aborts when binary finds 0 files but index has chunks
  - `ref update` warns on large prune operations (>50% of chunks)
- Release binary updated

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

### Remaining audit items
- #269: Brute-force search loads all embeddings (P4)
- #270: HNSW LoadedHnsw unsafe transmute (P4)

### P4 Deferred (v0.5.1 audit, still open)
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #240: embedding_batches cursor pagination

## Architecture

- Version: 0.9.7
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- 302 lib + 233 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (also available as CLI commands now)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
