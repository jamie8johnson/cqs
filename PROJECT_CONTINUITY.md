# Project Continuity

## Right Now

**Scala + Ruby language support.** 2026-02-26.

Adding languages 13 (Scala) and 14 (Ruby). New ChunkType variants: `Object`, `TypeAlias`. New SignatureStyle: `FirstLine`.

Done:
- Infrastructure: ChunkType::Object/TypeAlias, SignatureStyle::FirstLine in mod.rs, chunk.rs, calls.rs, nl.rs, query.rs
- `src/language/scala.rs` — full module with TYPE_QUERY, 8 tests
- `src/language/ruby.rs` — full module, 7 tests
- `tests/eval_common.rs` — exhaustive match arms
- Build/clippy/fmt clean, all tests pass (17 new)
- Docs updated: README, ROADMAP, CHANGELOG, CONTRIBUTING

Not committed yet — ready for commit + PR.

## Pending Changes

Uncommitted: Scala + Ruby language support (all files listed above).

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

- Version: 0.16.0
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 14 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, C#, F#, PowerShell, Scala, Ruby, SQL, Markdown)
- Tests: 1115 pass + 34 ignored, 0 failures
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4567 edges
- Eval: E5-base-v2 90.9% Recall@1, 0.951 NDCG@10, 0.941 MRR on 55-query hard eval
