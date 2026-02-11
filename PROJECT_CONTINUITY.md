# Project Continuity

## Right Now

**MCP server removed.** 2026-02-10.

Steps 1-13 complete. Remaining: close issues #345/#301, rebuild release binary, reindex.

### Completed This Session
- Moved `parse_duration()` from `src/mcp/validation.rs` → `src/audit.rs`
- Deleted `src/mcp/` (27 files, ~4649 lines), `tests/mcp_test.rs` (~1565 lines), `src/cli/commands/serve.rs`
- Removed 7 deps (axum, tower, tower-http, futures, tokio-stream, subtle, zeroize)
- Slimmed tokio from 6 features to 2 (`rt-multi-thread`, `time`)
- Fixed all MCP references across source, docs, notes, skills
- Build clean, all tests pass, clippy clean

### Completed Prior Sessions
- v0.9.9 released
- PR #348: HNSW staleness fix (#236)
- PR #349: MSRV bump 1.88→1.93, dropped fs4

### Pending
- `.cqs.toml` — untracked, has aveva-docs reference config

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files, 38 cross-referenced docs still missing. User converting more PDFs.
- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
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
- #302: CAGRA OOM guard
- #344: embed_documents tests

## Architecture

- Version: 0.9.9
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- CLI-only (MCP server removed)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
