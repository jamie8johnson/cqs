# Project Continuity

## Right Now

**v0.12.2 released.** 2026-02-12. All clean — no active work.

**v0.12.1 audit totals (complete):**
- P1: 26 fixes (PR #360), P2: 41 fixes (PR #380), P3: 40 fixes (PR #381), P4: 9 fixes (PR #393)
- Total: 116 fixes across 4 PRs. #389 (CAGRA GPU memory) deferred.
- Issues #383-#388, #390, #391 closed. GitHub release v0.12.2 published.

**Notes groomed.** 79 notes (pruned 1, updated 2, added 3).

## Pending Changes

`docs/notes.toml` — groomed notes (not yet committed).

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` skill** — template-based planning using scout/impact data
- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)
- #389: CAGRA GPU memory — needs disk persistence layer

## Architecture

- Version: 0.12.2
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 441 lib + 297 integration + 7 doc (747 total)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
