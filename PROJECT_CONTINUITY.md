# Project Continuity

## Right Now

**v0.4.6 released** (2026-02-05)

All sprint work published to GitHub and crates.io.

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
