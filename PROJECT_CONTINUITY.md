# Project Continuity

## Right Now

**Testing markdown indexing with AVEVA docs reference.** 2026-02-08.

### Hot-reload branch
- Branch `feat/reference-hot-reload`, commit `f2cc890`
- Changes in `src/mcp/server.rs`, `src/mcp/tools/search.rs`, `src/mcp/tools/stats.rs`
- Design: mtime-based lazy reload with `RwLock<ReferenceState>`, double-check locking
- Needs: PR, merge

### AVEVA docs reference testing
- `aveva-docs` reference: 5662 chunks from 39 markdown files in `samples/md/`
- Source: PDF→MD converted AVEVA System Platform docs (pymupdf4llm)
- Semantic search working — tested historian scripting, WebView2, MES, supply chain queries
- Identified 38 cross-referenced docs missing from the set (MES alone = 15 gaps)
- User will convert more PDFs to fill gaps

### Bugs found during testing
- **#318**: `ref update` silently prunes all chunks when binary lacks language support (v0.9.5 binary didn't know markdown, pruned entire index)
- **#319**: `ref remove` leaves stale metadata, blocking re-add with same name (UNIQUE constraint on metadata table)
- Root cause of #318: release binary was v0.9.5, not rebuilt after v0.9.6 merge. Fixed by rebuilding and installing.

### Pending
- `.cqs.toml` — untracked, has aveva-docs reference config
- `PROJECT_CONTINUITY.md` — modified (this update)
- Release binary now v0.9.6 (rebuilt and installed to `~/.cargo/bin/cqs`)

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

### Reference index bugs (new)
- #318: ref update silently prunes all chunks when binary lacks language support
- #319: ref remove leaves stale metadata, blocking re-add with same name

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
