# Project Continuity

## Right Now

**C# language support — implementation in progress.** 2026-02-25.

Branch: `feat/csharp-language-support` (5 commits ahead of main).

Design: `docs/plans/2026-02-25-csharp-language-support-design.md`
Plan: `docs/plans/2026-02-25-csharp-implementation-plan.md`

### Completed tasks (committed):

1. **Task 1: ChunkType variants** — Added Property, Delegate, Event to ChunkType enum. Added `callable_sql_list()`. Updated nl.rs, capture_types, CLI help. Replaced inline callable checks with `is_callable()`.

2. **Task 2: Dynamic callable SQL** — Replaced 3 hardcoded `IN ('function','method')` queries in calls.rs and chunks.rs with `ChunkType::callable_sql_list()`.

3. **Tasks 3+4: Infrastructure + backfill** — Added `common_types`, `container_body_kinds`, `extract_container_name` fields to LanguageDef. Replaced per-language match arms in `extract_container_type_name` with data-driven algorithm. Rust has custom extractor for `impl_item`. COMMON_TYPES in focused_read.rs now union of per-language sets. All 9 languages backfilled.

4. **Tasks 5+6: tree-sitter-c-sharp + C# module** — Added dep, feature flag, csharp.rs with all queries (chunk, call, type), stopwords, common types, extract_return. Registered in define_languages! macro. Added C# to registry tests.

### Current blocker:

**1 test failure: `test_registry_all_languages`.** Already added the `#[cfg(feature = "lang-csharp")]` counter block, but still failing. Need to debug — probably a test count issue or the test needs rebuild. The build itself is clean (0 warnings).

### Remaining tasks:

- Task 7: C# unit tests (chunk extraction tests in parser/chunk.rs, return extraction tests in csharp.rs)
- Task 8: Registry tests (mostly done — folded into Task 6 commit)
- Task 9: Documentation (README, CONTRIBUTING, CHANGELOG, ROADMAP)
- Task 10: Final verification, release build, PR

## Pending Changes

Branch `feat/csharp-language-support` — not yet pushed. 5 local commits on feature branch.

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

- Version: 0.14.1
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 10 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, C#, SQL, Markdown)
- Tests: ~622 pass + 5 ignored (lib), 1 failure pending debug
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4276 edges (Phase 1a+1b complete)
- Eval: E5-base-v2 90.9% Recall@1, 0.951 NDCG@10, 0.941 MRR on 55-query hard eval
