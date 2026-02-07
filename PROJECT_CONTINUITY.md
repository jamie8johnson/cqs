# Project Continuity

## Right Now

**v0.9.1 Audit complete** (2026-02-07). Full 14-category audit done, triage written.

### Audit status
- 157 raw findings → ~138 unique after dedup
- P1: 47 findings (easy + high impact) — **fix next**
- P2: 37 findings (medium effort or moderate impact)
- P3: 37 findings (test coverage, observability, polish)
- P4: 17 findings (defer/existing issues)
- Files: `docs/audit-findings.md`, `docs/audit-triage.md` (v0.9.1 section appended)
- Previous findings archived to `docs/audit-findings-v0.5.3.md`

### Top P1 items to fix first
1. `config.rs:184` unwrap_or_default destroys config on I/O error
2. Watch mode never updates call graph
3. `gather.rs` truncates by file order not score
4. `parse_duration` integer overflow bypasses 24h cap
5. 7 dead code methods to remove

### Post-audit roadmap items
- Add `format: "mermaid"` output to `cqs_impact`, `cqs_context`, `cqs_dead` — added to ROADMAP.md
- Architecture/pipeline/ER diagrams for docs

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 8**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255
- **Relevance feedback** — deferred indefinitely (low impact)
- **`.cq` rename to `.cqs`** — breaking change needing migration, no issue filed yet

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

- Version: 0.9.1
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 262 lib tests (no GPU), 0 warnings, clippy clean
- MCP tools: 20
- Source layout: parser/ and hnsw/ are now directories (split from monoliths in v0.9.0)
