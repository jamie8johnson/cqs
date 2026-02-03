# Project Continuity

## Right Now

**9-layer audit complete** - 1 action item

PR #58 merged: API key auth for HTTP transport + saturating casts.

**Dependency vulnerability:**
- `bytes` 1.11.0 has RUSTSEC-2026-0007 (integer overflow)
- Fix: `cargo update -p bytes` (needs >=1.11.1)

**Waiting on:**
- awesome-mcp-servers PR #1783

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- E5-base-v2 model with "passage: " / "query: " prefixes
- Schema v10: `origin` + `source_type` + `source_mtime` (nullable)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- `src/language/` - LanguageRegistry with LanguageDef structs
- `src/source/` - Source trait abstracts file/database sources
- Feature flags: lang-rust, lang-python, lang-typescript, lang-javascript, lang-go
- Storage: sqlx async SQLite with sync wrappers (4 connection pool, WAL mode)
- HTTP auth: `--api-key` or `CQS_API_KEY` env var (required for non-localhost)

## Build & Run

```bash
conda activate cuvs  # LD_LIBRARY_PATH set automatically via conda env vars
cargo build --release --features gpu-search
```

## Parked

- CAGRA persistence - hybrid startup approach used instead
- Curator agent, fleet coordination

## Open Questions

None active.

## Hardware

- i9-11900K, 128GB physical / 92GB WSL limit
- RTX A6000 (48GB VRAM), CUDA 12.0/13.0
- WSL2

## Test Repo

`/home/user001/rust` (rust-lang/rust, 36k files) - indexed with E5-base-v2

## Timeline

Project started: 2026-01-30
