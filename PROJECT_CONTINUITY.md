# Project Continuity

## Right Now

**Post-merge clean state.** 2026-02-13.

PR #405 merged — eliminated unsafe transmute in HNSW load (`self_cell`), added 4 `--ref` CLI integration tests. Closed #270.

## Pending Changes

ROADMAP.md, docs/notes.toml, PROJECT_CONTINUITY.md — uncommitted tears updates.

## Parked

- **Cross-encoder re-ranking** — `--rerank` flag, second-pass scoring. Next RAG improvement.
- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **P4 audit findings** — 14 deferred items in `docs/audit-triage.md` (reverse BFS depth, risk scoring edge cases, convert TOCTOU, cross-index bridge perf, test gaps)

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #270: HNSW LoadedHnsw unsafe transmute — closed (PR #405)
- #389: CAGRA GPU memory — needs disk persistence layer

## Architecture

- Version: 0.12.5
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 732 total (480 lib + 244 integration + 8 doc)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
