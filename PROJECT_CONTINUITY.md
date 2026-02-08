# Project Continuity

## Right Now

**Exploring PDF indexing.** v0.9.5 shipped. Investigating markdown-based chunking for large technical manuals.

- Converted 39 AVEVA Historian PDFs → markdown via `pymupdf4llm` (14MB, 457K lines)
- PDFs in `samples/pdf/`, markdown in `samples/md/` (both gitignored)
- Next: build a markdown heading-based chunker as a new cqs language
- Heading hierarchy maps to chunks: `#` = chapter, `##`-`####` = sections, content below = chunk body

### Uncommitted
- `.gitignore` — added `samples/`
- `PROJECT_CONTINUITY.md`, `ROADMAP.md`, `docs/notes.toml` — tears + release updates

### Recent merges
- PR #314: Release v0.9.5
- PR #313: T-SQL name extraction fix (ALTER PROCEDURE/FUNCTION, position-based fallback)
- PR #312: Update tears for v0.9.4 release
- PR #311: Use crates.io dep for tree-sitter-sql
- PR #310: Release v0.9.4
- PR #309: SQL language support
- PR #308: Audit cleanup batch (#265, #264, #241, #267, #239, #232)
- PR #307: Language extensibility via define_languages! macro (#268)

### P4 audit items tracked in issues
- #300: Search/algorithm edge cases (5 items)
- #301: Observability gaps (5 items)
- #302: Test coverage gaps (4 items)
- #303: Polish/docs (3 items)

### Dev environment
- `~/.bashrc`: CUDA/conda/cmake env vars above non-interactive guard (CUDA_PATH, CPATH, LIBRARY_PATH, LD_LIBRARY_PATH, CMAKE_PREFIX_PATH, miniforge3/bin in PATH)
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot
- GPU: RTX A6000, always use `--features gpu-search`
- Node.js 25+ via conda (for tree-sitter grammar development)
- `pymupdf4llm` installed via conda (`conda run python3 -c "import pymupdf4llm"`) for PDF→MD conversion

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar — only PostgreSQL-style triggers work
- `type_map` field in LanguageDef is defined but never read (dead code — extract_chunk uses hardcoded capture_types)

## Parked

- **VB.NET language support** — parked, VS2005 project delayed
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

### Remaining audit items
- #269: Brute-force search loads all embeddings (P4)
- #270: HNSW LoadedHnsw unsafe transmute (P4)

### P4 Deferred (v0.5.1 audit, still open)
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #240: embedding_batches cursor pagination

## Architecture

- Version: 0.9.5
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 8 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL)
- 286 lib + 233 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (note_only, summary, mermaid added as params in v0.9.2+)
- Source layout: parser/ and hnsw/ are now directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (git dep — not yet published to crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS, NOT `/mnt/c/Projects/cq/target/` which was deleted)
