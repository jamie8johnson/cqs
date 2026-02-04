# Project Continuity

## Right Now

**v0.1.18** - Audit fixes in progress.

**PRs merged (2026-02-03):**
- #87: Fixed #74 timing attack (subtle::ConstantTimeEq)
- #88: Fixed #75 rsa vuln (sqlx default-features)
- #89: Fixed #64,82-84 quick wins batch

**Open issues:**
- #76 HIGH: Security tests for validate_api_key
- #77 HIGH: cli.rs tests
- #78 HIGH: Split cli.rs (~2000 lines)
- #79 HIGH: embedder.rs tests
- #80 MEDIUM: Symlink docs
- #81 MEDIUM: FTS5 escape
- #85 LOW: HNSW SAFETY comments (already present)
- #86 LOW: Threat model docs

**Previous issues:** #62-70 (first audit), #65 SAFETY comments (done)

**Plan:** See `~/.claude/plans/snuggly-orbiting-journal.md`

**5 unmaintained deps:** bincode, derivative, instant, number_prefix, paste (all transitive)

**Waiting on:** awesome-mcp-servers PR #1783

## Learnings

**"Constant-time" isn't - verify implementations:**
- `.all()` short-circuits on first mismatch
- Length comparison leaks length
- Use `subtle::ConstantTimeEq` crate

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
