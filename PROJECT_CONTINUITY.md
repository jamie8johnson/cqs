# Project Continuity

## Right Now

**v0.12.1 P1 audit fixes done.** 2026-02-11. Branch: `fix/audit-p1`.

- All 26 P1 findings fixed (commit 4dccd9d), 21 files changed
- Build/tests/clippy/fmt all pass
- `docs/audit-triage.md` P1 status column updated with ✅
- Next: push branch, create PR, then P2 fixes

Uncommitted: audit-findings.md, docs/notes.toml, PROJECT_CONTINUITY.md, archived prior audit files.

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

## Architecture

- Version: 0.12.1
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 408 lib + 213 integration + 11 doc (632 total)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
