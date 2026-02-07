# Project Continuity

## Right Now

**Post-Phase 7 features merged** (2026-02-07). PR #280 squash-merged to main.

All 8 post-roadmap features shipped:
- Batch A: `--chunk-type` filter + `cqs dead`
- Batch B: Staleness warnings + `cqs gc`
- Batch C: `--format mermaid` on trace + `cqs project` cross-project search
- Batch D: `cqs gather` (smart context assembly) + `--pattern` structural filter

**In progress**: docs review (agent running), then version bump to 0.9.0, changelog, release

### Refactoring roadmapped
- Split `parser.rs` (1071 lines) → per-language modules under `src/language/`
- Split `hnsw.rs` (1150 lines) → `src/hnsw/` directory (build, persist, search)

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 8**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255
- **Relevance feedback** — Feature 9, deferred indefinitely (low impact)

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

- Version: 0.8.0
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 258 lib tests (no GPU), 0 warnings, clippy clean
- MCP tools: 21 (search, stats, callers, callees, read, add_note, update_note, remove_note, dead, audit_mode, diff, explain, similar, impact, trace, test_map, batch, context, gc, gather + pattern on search)
