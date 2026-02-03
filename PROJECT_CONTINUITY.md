# Project Continuity

## Right Now

**MAJOR REFACTOR 6/6 COMPLETE** - Clean architecture for SQL Server support

Plan file: `/home/user001/.claude/plans/snuggly-orbiting-journal.md`

**All tasks done:**
1. Schema v10: origin/source_type - DONE
2. Migrate rusqlite → sqlx (async) - DONE
3. Language registry module - DONE
4. Source trait + FileSystemSource - DONE
5. Async indexing pipeline - NOT NEEDED (sync wrappers in store.rs)
6. Feature flags for languages - DONE

The sqlx migration preserves sync API via internal `Runtime::block_on()` wrappers.
This means cli.rs and mcp.rs didn't need changes - store.rs is ~1700 lines of async sqlx
with sync methods that callers use exactly as before.

**Fresh-eyes review done** - no critical issues. Minor notes:
- `function_calls` table still uses `file` (not `origin`) - intentional, call graph is file-centric
- Removed dead `hunches` and `scars` tables from schema (notes replaced them in v8)

**Key files changed this session:**
- `src/store.rs` - complete rewrite: rusqlite → sqlx with sync wrappers
- `Cargo.toml` - replaced rusqlite/r2d2 with sqlx

**Waiting on:**
- awesome-mcp-servers PR #1783

**1.0 progress:**
- Schema v10 done (breaking change)
- sqlx migration done
- All refactor tasks complete

Pronunciation: cqs = "seeks" (it seeks code semantically).

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- E5-base-v2 model with "passage: " / "query: " prefixes
- Schema v10: `origin` + `source_type` + `source_mtime` (nullable)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- `src/language/` - LanguageRegistry with LanguageDef structs
- `src/source/` - Source trait abstracts file/database sources
- Feature flags: lang-rust, lang-python, lang-typescript, lang-javascript, lang-go
- Storage: sqlx async SQLite with sync wrappers (4 connection pool, WAL mode)

## Build & Run

```bash
conda activate cuvs  # LD_LIBRARY_PATH set automatically via conda env vars
cargo build --release --features gpu-search
```

## Parked

- CAGRA persistence - hybrid startup approach used instead
- API key auth for HTTP transport
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
