# Project Continuity

## Right Now

**v0.12.3 released, clean state.** 2026-02-12.

v0.12.3 released to crates.io + GitHub. Post-release: split `impact.rs` monolith into `src/impact/` directory (PR #402). Added Code Quality section to roadmap.

Next on roadmap: `cqs ci` (Phase 5 in plan), `cqs health` (Phase 3), re-ranking (Phase 4).

## Pending Changes

- `PROJECT_CONTINUITY.md` — updated tears (this file)
- `docs/notes.toml` — 2 new notes added (review composition, risk entry-point exception), uncommitted

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
- Tests: 808 total (470 lib + ~318 integration + 12 doc + 8 doc-tests)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
