# Project Continuity

## Right Now

**Phase 2 language expansion — Batch 2.** 2026-03-05.

Nix, Make, LaTeX done. v0.25.0. Uncommitted — ready for branch + PR.

Batch 1 (HTML, JSON, XML, INI) merged as PR #535.

Key fixes during Batch 2:
- `tree-sitter-latex` 0.1.0 broken (missing scanner.c) → switched to `codebook-tree-sitter-latex` 0.6.1
- `@section` capture was missing from `chunk.rs:capture_types` and `mod.rs:DEF_CAPTURES` — no prior tree-sitter language used it (Markdown uses custom parser)
- Nix needed `apply_expression` pattern for function-application bindings

## Pending Changes

Batch 2 language files (nix.rs, make.rs, latex.rs), fixtures, parser fixes (@section capture), mod.rs/eval_common/parser_test updates, doc updates. All tests pass (1440). Ready for branch + PR.

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

- Version: 0.25.0
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 43 languages (Rust, Python, TypeScript, JavaScript, Go, C, C++, Java, C#, F#, PowerShell, Scala, Ruby, Bash, HCL, Kotlin, Swift, Objective-C, SQL, Protobuf, GraphQL, PHP, Lua, Zig, R, YAML, TOML, Elixir, Erlang, Haskell, OCaml, Julia, Gleam, CSS, Perl, HTML, JSON, XML, INI, Nix, Make, LaTeX, Markdown)
- 16 ChunkType variants (Function, Method, Struct, Class, Interface, Enum, Trait, Constant, Section, Property, Delegate, Event, Module, Macro, Object, TypeAlias)
- Tests: 1440 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4567 edges
- Eval: E5-base-v2 90.9% Recall@1, 0.951 NDCG@10, 0.941 MRR on 55-query hard eval
