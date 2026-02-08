# Project Continuity

## Right Now

**SQL language support** — implementation complete, tests passing. Ready for branch + PR.

### Uncommitted
- SQL language support (Part 1: forked tree-sitter-sql, Part 2: cqs integration)
  - `Cargo.toml`: tree-sitter-sql git dep, lang-sql feature
  - `src/language/sql.rs`: new module (chunk query, call query, stopwords, extract_return)
  - `src/language/mod.rs`: SignatureStyle::UntilAs, define_languages! entry for Sql
  - `src/parser/chunk.rs`: UntilAs match arm in extract_signature
  - `tests/fixtures/sample.sql`: SQL test fixture (5 objects, GO separators)
  - `tests/parser_test.rs`: 5 new SQL tests
  - `tests/eval_test.rs`, `tests/model_eval.rs`: Sql match arms
  - CLAUDE.md, CONTRIBUTING.md, ROADMAP.md: updated for 8 languages
- Grammar fork: `jamie8johnson/tree-sitter-sql` (CREATE PROCEDURE, GO, EXEC added)

### Recent merges
- PR #308: Audit cleanup batch (#265, #264, #241, #267, #239, #232)
- PR #307: Language extensibility via define_languages! macro (#268)
- PR #306: v0.9.3 release
- PR #305: Fix gather/cross-project search to use RRF hybrid

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

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar — only PostgreSQL-style triggers work
- `type_map` field in LanguageDef is defined but never read (dead code — extract_chunk uses hardcoded capture_types)

## Parked

- **VB.NET language support** — next language after SQL
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

- Version: 0.9.4
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 8 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL)
- 286 lib + 233 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (note_only, summary, mermaid added as params in v0.9.2+)
- Source layout: parser/ and hnsw/ are now directories (split from monoliths in v0.9.0)
