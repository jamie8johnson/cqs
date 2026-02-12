# Project Continuity

## Right Now

**Token budgeting (`--tokens`) shipped across 5 commands.** 2026-02-12.

Implemented `--tokens N` greedy knapsack packing on:
- `cqs "query" --tokens N` — pack search results into budget
- `cqs gather --tokens N` — pack gathered chunks into budget
- `cqs context file --tokens N` — include chunk content within budget
- `cqs explain func --tokens N` — include target + similar content
- `cqs scout "task" --tokens N` — fetch and include chunk content

Also shipped in same session (PR #398, already merged):
- `--ref` scoped search — `cqs "query" --ref aveva` skips project index
- `cqs convert` — document-to-Markdown conversion (PDF, HTML, CHM, web help)

Changes uncommitted on `feat/rag-strengthen` branch, ready to commit + PR.

## Pending Changes

- Token budgeting across 5 commands (uncommitted on `feat/rag-strengthen`)
- `PROJECT_CONTINUITY.md` and `docs/notes.toml` — session state

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

- Version: 0.12.2
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 757 total (465 lib + ~280 integration + 12 doc)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
