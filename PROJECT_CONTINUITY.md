# Project Continuity

## Right Now

**`cqs plan` skill created.** 2026-02-12. Skill-based task planning with 5 task-type templates (add flag, add command, fix bug, add language, refactor). Experiment validated: templates improve agent speed (43% faster) and idiomatic correctness (ValueEnum vs raw String).

**v0.12.1 audit complete.** PR #379 (P1) was orphaned but fixes landed via P2-P4 PRs. Closed as redundant.

## Pending Changes

Uncommitted: `cqs-plan` skill, bootstrap update, CLAUDE.md update, notes, PROJECT_CONTINUITY.md.

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge from usage
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
