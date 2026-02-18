# Project Continuity

## Right Now

**Phase 1d complete + v0.12.12 release.** 2026-02-18.

- Phase 1d: Parent type context enrichment + embedding model eval (PR #455)
  - E5-base-v2 confirmed: 90.9% Recall@1, 0.941 MRR (beats jina 80.0%, 0.863)
  - Method NL descriptions now include parent type ("circuit breaker method")
  - Hard eval: 55 confusable queries across 5 languages
- Docs overhaul: repositioned as code intelligence + RAG (not just search)

Previous phases: 1a (type extraction), 1b (type wiring), 1c (note-boosted search), 2a (batch completeness).

Next: Phase 2b+ per MOONSHOT.md (onboard, drift, auto-stale notes) or C# language support.

## Pending Changes

None.

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

- Version: 0.12.12
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 1026 total (998 pass + 28 ignored)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 3901 edges, 321 unique types (Phase 1a+1b complete)
- Eval: E5-base-v2 90.9% Recall@1, 0.941 MRR on 55-query hard eval
