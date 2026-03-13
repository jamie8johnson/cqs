# Project Continuity

## Right Now

**v1.0.5 releasing (2026-03-13).** ASPX support (51 languages), search quality improvements (demotion, name_boost gating, parent_type_name, class NL), CUDA 13 fix, flaky HNSW test fix.

## Pending Changes

None.

## Parked

- **`cqs plan` templates** — 11 templates now; add more as patterns emerge
- **Post-index name matching** — fuzzy cross-doc references
- **ref install** — deferred, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently on rc.12, waiting for 2.0 stable)
- #63: paste dep unmaintained (RUSTSEC-2024-0436) — transitive via `tokenizers`, waiting on HuggingFace to switch to `pastey`

### Feature
- #255: Pre-built reference packages

### Audit
- #389: CAGRA CPU-side dataset retention (~146MB at 50k chunks) — can't drop because cuVS `search()` consumes the index, requiring rebuild from cached embeddings. Blocked on upstream API change.

## Architecture

- Version: 1.0.5
- MSRV: 1.93
- Schema: v12
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 51 languages (Rust, Python, TypeScript, JavaScript, Go, C, C++, Java, C#, F#, PowerShell, Scala, Ruby, Bash, HCL, Kotlin, Swift, Objective-C, SQL, Protobuf, GraphQL, PHP, Lua, Zig, R, YAML, TOML, Elixir, Erlang, Haskell, OCaml, Julia, Gleam, CSS, Perl, HTML, JSON, XML, INI, Nix, Make, LaTeX, Solidity, CUDA, GLSL, Svelte, Razor, VB.NET, Vue, ASPX, Markdown)
- 16 ChunkType variants (Function, Method, Struct, Class, Interface, Enum, Trait, Constant, Section, Property, Delegate, Event, Module, Macro, Object, TypeAlias)
- Tests: 1534 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4567 edges
- Eval: E5-base-v2 90.9% Recall@1, 0.936 MRR on 55-query hard eval (name_boost no longer harmful)
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64 (Intel Mac dropped — no ort prebuilt binaries)
