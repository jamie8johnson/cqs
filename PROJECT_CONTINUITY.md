# Project Continuity

## Right Now

**Releasing v0.12.3.** 2026-02-12.

- PR #400: `cqs review` with risk scoring — merged
- PR #399: `cqs gather --ref` cross-index gather
- PR #398: `--tokens` on 5 commands, `--ref` scoped search, `cqs convert`
- PR #396: `cqs plan` skill with 5 task-type templates

## Pending Changes

None.

## Parked

- **Cross-encoder re-ranking** — `--rerank` flag, second-pass scoring. Next RAG improvement.
- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255
- **Query-intent routing** — auto-boost ref weight when query mentions product names

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

- Version: 0.12.3
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 760 total (465 lib + ~283 integration + 12 doc)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
