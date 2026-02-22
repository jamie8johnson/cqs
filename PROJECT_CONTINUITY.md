# Project Continuity

## Right Now

**Phase 3 Moonshot: `cqs task` implemented.** 2026-02-22.

- `cqs task "description"` — single-call implementation brief (scout + gather + impact + placement + notes)
- New files: `src/task.rs` (~580 lines), `src/cli/commands/task.rs` (~610 lines)
- Modified: `src/scout.rs` (scout_core extraction), `src/cli/mod.rs`, `src/cli/batch/commands.rs`, `src/cli/batch/handlers.rs`, `src/lib.rs`, `src/cli/commands/mod.rs`
- 1,058 tests passing (20 new), zero clippy warnings
- Binary installed, manual testing complete
- Ecosystem updates done (skill, bootstrap, CLAUDE.md, CONTRIBUTING.md, CHANGELOG, MOONSHOT)
- Ready for commit + PR

## Pending Changes

Uncommitted: `cqs task` implementation + ecosystem docs.

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **resolve_target test bias** — ambiguous names resolve to test functions over production code. Not blocking, but `cqs related foo` may pick `test_foo_bar` instead of `foo`. Fix: prefer non-test chunks in resolve_target.

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #389: CAGRA GPU memory — needs disk persistence layer

## Architecture

- Version: 0.13.1
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 1058 pass + 31 ignored
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4276 edges (Phase 1a+1b complete)
- Eval: E5-base-v2 90.9% Recall@1, 0.941 MRR on 55-query hard eval
