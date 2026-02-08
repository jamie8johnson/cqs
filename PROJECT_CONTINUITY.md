# Project Continuity

## Right Now

**Reference hot-reload implemented, needs commit/PR.** 2026-02-08.

### In progress
- Hot-reload for MCP server reference indexes — implemented on `main` (uncommitted)
- Changes in `src/mcp/server.rs`, `src/mcp/tools/search.rs`, `src/mcp/tools/stats.rs`
- Design: mtime-based lazy reload with `RwLock<ReferenceState>`, double-check locking
- All tests pass (544 total), clippy clean, 0 warnings
- Needs: branch, commit, PR, merge

### Pending
- `.cqs.toml` created by `ref add` — untracked, has aveva-docs reference config
- AVEVA reference is temporary (for testing only) — `cqs ref remove aveva-docs` when done

### Recent merges
- PR #316: Release v0.9.6
- PR #315: Markdown indexing support

### P4 audit items tracked in issues
- #300: Search/algorithm edge cases (5 items)
- #301: Observability gaps (5 items)
- #302: Test coverage gaps (4 items)
- #303: Polish/docs (3 items)

### Dev environment
- `~/.bashrc`: CUDA/conda/cmake env vars above non-interactive guard
- `.mcp.json`: fixed LD_LIBRARY_PATH to include miniforge3/lib + cuda lib64
- GPU: RTX A6000, always use `--features gpu-search`
- `pymupdf4llm` installed via conda for PDF→MD conversion

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references (substring matching of chunk names across docs)
- **Phase 8**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255
- **`.cq` rename to `.cqs`** — breaking change needing migration

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

- Version: 0.9.6
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- 298 lib + 233 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
