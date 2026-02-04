# Project Continuity

## Right Now

**v0.2.0** - Security audit complete, preparing release.

**Audit PRs merged (2026-02-03/04):**
- #88: Fixed #74 timing attack (subtle::ConstantTimeEq)
- #87: Fixed #75 rsa vuln (sqlx default-features)
- #89: Fixed #64,82-84 quick wins batch
- #90: Security tests (#76)
- #91: FTS5 special char tests (#81)
- #92: Split cli.rs into cli/ module (#78)
- #93: Unit tests for embedder/cli (#77, #79)
- #94: MCP edge case tests + IPv6 (#68)
- #95: Property tests + security docs (#67, #69, #80, #86)

**Closed issues (audit):**
#64, #66, #67, #68, #69, #74, #75, #76, #77, #78, #79, #80, #81, #82, #83, #84, #85, #86

**Remaining open:**
- #62: Broader test coverage (partial - embedder/cli done, cache/GPU init remain)
- #70: Low-priority cleanup (ongoing)
- #63: Monitor paste dep (external, no action)

**Tests:** 162 total (was ~75 before audit, 2x+)

**5 unmaintained deps:** bincode, derivative, instant, number_prefix, paste (all transitive)

**Waiting on:** awesome-mcp-servers PR #1783

## Learnings

**"Constant-time" isn't - verify implementations:**
- `.all()` short-circuits on first mismatch
- Length comparison leaks length
- Use `subtle::ConstantTimeEq` crate

**Property tests find real bugs:**
- RRF bound calculation was wrong (duplicates can boost scores)
- proptest found it immediately with minimal input

## Key Architecture

- 769-dim embeddings (768 + sentiment)
- E5-base-v2 model with "passage: " / "query: " prefixes
- Schema v10: `origin` + `source_type` + `source_mtime` (nullable)
- VectorIndex trait: CAGRA (GPU) > HNSW (CPU) > brute-force
- `src/language/` - LanguageRegistry with LanguageDef structs
- `src/source/` - Source trait abstracts file/database sources
- `src/cli/` - Split into mod.rs + display.rs
- Feature flags: lang-rust, lang-python, lang-typescript, lang-javascript, lang-go
- Storage: sqlx async SQLite with sync wrappers (4 connection pool, WAL mode)
- HTTP auth: `--api-key` or `CQS_API_KEY` env var (required for non-localhost)
- IPv6 localhost: `[::1]` now accepted in origin validation

## Build & Run

```bash
conda activate cuvs  # LD_LIBRARY_PATH set automatically via conda env vars
cargo build --release --features gpu-search
```

## Parked

- CAGRA persistence - hybrid startup approach used instead
- Curator agent, fleet coordination
- #62 broader test coverage - refactoring needed for testability

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
