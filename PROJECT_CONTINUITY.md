# Project Continuity

## Right Now

**C# language support — implementation complete, ready for PR.** 2026-02-25.

Branch: `feat/csharp-language-support` (7 commits ahead of main).

Design: `docs/plans/2026-02-25-csharp-language-support-design.md`
Plan: `docs/plans/2026-02-25-csharp-implementation-plan.md`

### All tasks complete:

1. **Task 1: ChunkType variants** — Property, Delegate, Event. `callable_sql_list()`. `is_callable()`.
2. **Task 2: Dynamic callable SQL** — 3 hardcoded queries replaced.
3. **Tasks 3+4: Infrastructure + backfill** — Per-language common_types, container_body_kinds, extract_container_name. Data-driven container extraction. All 9 existing languages backfilled.
4. **Tasks 5+6: tree-sitter-c-sharp + C# module** — Full csharp.rs with chunk/call/type queries, stopwords, common types, extract_return. Registered in define_languages! macro.
5. **Task 7: C# unit tests** — 8 parse tests (class, method, property, delegate, event, interface, enum, record→struct, constructor, local function). eval_common.rs CSharp arm added.
6. **Task 8: Registry tests** — folded into Task 6.
7. **Task 9: Documentation** — README (10 languages, C# in list), CHANGELOG, CONTRIBUTING, ROADMAP all updated.
8. **Task 10: Final verification** — clippy clean, release build clean, 1101 tests pass (0 failures, 35 ignored).

### Next: Push branch and create PR.

## Pending Changes

Branch `feat/csharp-language-support` — not yet pushed. 7 local commits.

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
- Tests: 1101 pass + 35 ignored, 0 failures
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4276 edges (Phase 1a+1b complete)
- Eval: E5-base-v2 90.9% Recall@1, 0.951 NDCG@10, 0.941 MRR on 55-query hard eval
