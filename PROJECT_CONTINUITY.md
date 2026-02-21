# Project Continuity

## Right Now

**v0.12.12 audit complete (P1-P3).** 2026-02-21.

- P1: 12 fixes merged (#459) — security, correctness, data safety, error handling
- P2: 18 fixes merged (#460) — BatchContext caching, N+1 queries, API design, robustness. Also renamed `gpu-search` → `gpu-index`
- P3: 31 fixes merged (#461) — docs (9), error handling (5), observability (4), API design (5), robustness (5), algorithm/platform/perf (5), test coverage (2), doc comments (3). 8 non-issues triaged.
- P4: 18 findings deferred (tests, extensibility, design)
- Binary updated, index rebuilt (3984 chunks, 4276 type edges)

Audit triage: `docs/audit-triage.md`

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
- **P4 audit findings** — 18 findings deferred (issues TBD)
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

- Version: 0.13.0
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 1020 pass + 28 ignored
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4276 edges (Phase 1a+1b complete)
- Eval: E5-base-v2 90.9% Recall@1, 0.941 MRR on 55-query hard eval
