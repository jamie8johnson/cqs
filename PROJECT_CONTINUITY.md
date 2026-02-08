# Project Continuity

## Right Now

**Releasing v0.9.6.** 2026-02-08.

### Done
- Markdown indexing merged (PR #315)
- Design doc: `docs/plans/2026-02-08-markdown-indexing-design.md`
- 18-step plan fully implemented:
  - `scripts/clean_md.py` — 7-rule PDF artifact preprocessor (tested on 39 files)
  - `ChunkType::Section`, `SignatureStyle::Breadcrumb` added
  - `grammar: Option<fn()>` — made grammar optional for non-tree-sitter languages
  - All 8 existing language defs updated: `grammar: Some(...)`
  - `src/language/markdown.rs` — LanguageDef (no grammar, 55 prose stopwords)
  - `Markdown` registered in `define_languages!`, `lang-markdown` feature flag
  - `src/parser/markdown.rs` (~370 lines) — adaptive heading parser + cross-ref extraction
  - Parser wiring: 5 dispatch points guarded in mod.rs + calls.rs
  - NL description for Section chunks (breadcrumb + name + preview)
  - MCP schema + CLI error message updated with "section"
  - diff.rs test updated
  - eval tests updated (Language match exhaustiveness)
  - `.mcp.json` fixed (added miniforge3/lib + cuda to LD_LIBRARY_PATH)
- 298 lib + 233 integration tests pass, 0 warnings, clippy clean

### Key implementation details
- **Adaptive heading detection**: "shallowest heading level appearing more than once" = primary split level. Handles both standard (H1→H2→H3) and inverted (H2→H1→H3) AVEVA hierarchies.
- **Merge logic**: small sections (<30 lines) merge INTO the next big section (not the other way)
- **Regex fix**: Rust `regex` crate doesn't support lookbehind — filter image links by checking preceding `!` byte
- **Overflow split**: excludes title level from candidates (inverted hierarchy fix)

### Recent merges
- PR #314: Release v0.9.5
- PR #313: T-SQL name extraction fix
- PR #311: Use crates.io dep for tree-sitter-sql

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
