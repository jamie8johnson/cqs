# Project Continuity

## Right Now

**v0.19.2 P1 audit fixes complete.** 2026-02-27.

Second 14-category audit completed (117 findings across 3 batches). All 46 P1 fixes shipped in PR #501. 38 files changed, +646/-368 lines. 1217 tests pass.

Key P1 fixes: SQLite URL injection eliminated (SqliteConnectOptions), open_readonly quick_check, NaN-safe batch serialization (6 sites), onboard hybrid RRF, NoteBoostIndex O(1) lookup, NOT EXISTS anti-join, find_reference() helper (6 sites deduped), GatheredChunk::from_search (4 sites), GatherDirection/AuditModeState enums, HNSW_ALL_EXTENSIONS constant, ChunkType::ALL, backslash normalization, WSL inotify warning, rel_display tests.

P2 (31 items), P3 (29), P4 (3) remain in docs/audit-triage.md.

## Pending Changes

None — clean working tree on main.

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

- Version: 0.19.2
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 20 languages (Rust, Python, TypeScript, JavaScript, Go, C, C++, Java, C#, F#, PowerShell, Scala, Ruby, Bash, HCL, Kotlin, Swift, Objective-C, SQL, Markdown)
- 16 ChunkType variants (Function, Method, Struct, Class, Interface, Enum, Trait, Constant, Section, Property, Delegate, Event, Module, Macro, Object, TypeAlias)
- Tests: 1217 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4567 edges
- Eval: E5-base-v2 90.9% Recall@1, 0.951 NDCG@10, 0.941 MRR on 55-query hard eval
