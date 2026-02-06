# Project Continuity

## Right Now

**Phase 6: Discovery & UX** — implementation complete, needs review + PR (2026-02-06)

### Implemented (all uncommitted on main)
- [x] Store prereq methods (`get_chunk_with_embedding`, `all_chunk_identities`, `ChunkIdentity`)
- [x] `cqs similar` (CLI + MCP) — search by example using stored embeddings
- [x] `cqs explain` (CLI + MCP) — function card (signature, callers, callees, similar)
- [x] `cqs diff` (CLI + MCP) — semantic diff between indexed snapshots
- [x] Workspace-aware indexing — detect Cargo workspace root

### New files
- `src/diff.rs` — core diff algorithm
- `src/cli/commands/similar.rs` — CLI handler
- `src/cli/commands/explain.rs` — CLI handler
- `src/cli/commands/diff.rs` — CLI handler
- `src/mcp/tools/similar.rs` — MCP tool
- `src/mcp/tools/explain.rs` — MCP tool
- `src/mcp/tools/diff.rs` — MCP tool

### Modified files
- `src/store/chunks.rs`, `src/store/helpers.rs`, `src/store/mod.rs` — Store prereqs
- `src/cli/mod.rs`, `src/cli/commands/mod.rs` — CLI wiring (3 new commands)
- `src/mcp/tools/mod.rs` — MCP wiring (3 new tools)
- `src/cli/display.rs` — `display_similar_results_json`
- `src/cli/config.rs` — workspace-aware `find_project_root`
- `src/lib.rs` — `pub mod diff`
- `CLAUDE.md` — document new tools

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 7**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Multi-index follow-ups
- #255: Pre-built reference packages
- #256: Cross-store dedup
- #257: Parallel search + shared Runtime

### Remaining audit items (v0.6.0 audit)
- #264: Config load_file silently ignores parse errors (P3)
- #265: search_reference swallows errors (P3)
- #266: embedding_to_bytes should validate dimensions (P3)
- #267: Module boundary cleanup (P4)
- #268: Language extensibility (P4)
- #269: Brute-force search loads all embeddings (P4)
- #270: HNSW LoadedHnsw unsafe transmute (P4)

### P4 Deferred (v0.5.1 audit, still open)
- #231: Notes file locking
- #232: CAGRA RAII guard pattern
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #239: Test coverage gaps (low-priority)
- #240: embedding_batches cursor pagination
- #241: Config permission checks

## Architecture

- Version: 0.6.0
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 431 tests (no GPU), 0 warnings, clippy clean
- MCP tools: 12 (search, stats, callers, callees, read, add_note, update_note, remove_note, audit_mode, diff, explain, similar)
