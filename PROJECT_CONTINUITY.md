# Project Continuity

## Right Now

**Moonshot Phase 2a complete.** 2026-02-17.

- Phase 1a: Parser type extraction + schema v11 + `cqs deps` (PRs #440, #442)
- Phase 1b: Wire type_edges into related, impact, read --focus, dead (PR #447)
- Phase 1c: Note-boosted search ranking (PR #452)
- Phase 2a: Batch completeness — 6 new commands (PR #453)

Also merged 4 dependabot bumps: simsimd 6.5.13, toml 1.0.1, ctrlc 3.5.2, indicatif 0.18.4 (#448-#451).

Next: Phase 2b+ per MOONSHOT.md (onboard, drift, auto-stale notes) or Phase 1d (embedding model eval — research).

## Pending Changes

Uncommitted notes in `docs/notes.toml` (Phase 1c + OnceLock notes). `PROJECT_CONTINUITY.md` updated.

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **P4 audit findings** — 1 remaining (#407 reverse BFS depth)
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

- Version: 0.12.11
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 991 total (964 pass + 27 ignored)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 3901 edges, 321 unique types (Phase 1a+1b complete)
