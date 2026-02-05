# Project Continuity

## Right Now

**Agent teams smoke-tested** (2026-02-05)

- Teams research preview is enabled and working
- Smoke test passed: spawnTeam → TaskCreate → spawn teammate → task execution → message → shutdown → cleanup
- Added "Agent Teams" section to CLAUDE.md with conventions (naming, model selection, cleanup, self-contained prompts)
- Updated 20-Category Audit execution to use teams (one team per batch, 5 teammates per batch)
- Built cqs binary to `/home/user001/.cargo-target/cq/debug/cqs` — MCP server needs Claude Code restart to connect

### Pending
- CLAUDE.md has uncommitted changes (Agent Teams section + audit execution update)
- Restart Claude Code to pick up cqs MCP server

### What shipped in v0.4.6
- Schema migration framework (#188)
- CLI integration tests (#206)
- Server transport tests (#205)
- Stress tests (#207)
- `--api-key-file` with zeroize (#202)
- Lazy grammar loading (#208)
- Pipeline resource sharing (#204)
- Atomic HNSW writes (#186)
- Note search warning at WARN level (#203)
- Fixed flaky HNSW test (top-3 → top-5)

## Parked

Nothing active.

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## Architecture

- Version: 0.4.6
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- Unified HNSW index (chunks + notes with prefix)
- Store: split into focused modules (7 files including migrations)
- CLI: mod.rs + display.rs + watch.rs + pipeline.rs
- 290+ tests (including CLI, server, stress tests)
