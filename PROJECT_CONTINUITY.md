# Project Continuity

## Right Now

**v0.12.7 post-release cleanup complete.** 2026-02-13.

All shipped in PR #428:
- Skills synced with v0.12.7 CLI flags (created `cqs-review`, updated 8 skills, updated bootstrap)
- Added `--json` to `stats`, `notes list`, `ref list` (code existed, clap routing was broken)
- Stale warnings moved from stdout to stderr (stats, gc, ci, review)
- Fixed `-q` references in gather/scout skills
- Notes groomed (90→91: removed 2 MEMORY.md duplicates, added 3 from trial run)
- Comprehensive 56-command CLI trial run verified all functionality

## Pending Changes

None.

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **P4 audit findings** — 3 remaining in `docs/audit-triage.md` (#407 reverse BFS depth, #410 convert TOCTOU, #414 cross-index tests)

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #389: CAGRA GPU memory — needs disk persistence layer

## Architecture

- Version: 0.12.7
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 875 total (499 lib + 115 bin + 253 integration + 8 doc)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
