# Project Continuity

## Right Now

**Multi-grammar parsing — implemented.** 2026-03-05.

HTML files now extract real JS/CSS chunks from `<script>` and `<style>` blocks using tree-sitter `set_included_ranges()`. Two-phase parsing: outer HTML grammar, then inner JS/CSS grammars.

Key additions:
- `InjectionRule` struct on `LanguageDef` (all 46 languages default to `&[]`)
- `src/parser/injection.rs` — find_injection_ranges, parse_injected_chunks, parse_injected_relationships
- HTML injection rules: script→JS (with TS detection via `lang`/`type` attrs), style→CSS
- 9 new tests (JS extraction, CSS, TypeScript detection, empty script, multiple scripts, whitespace-only, regression, call graph, non-injection language)
- `sample.html` fixture enhanced with inline `<script>` functions
- Total: 1465 tests pass, 0 fail

## Pending Changes

Uncommitted multi-grammar changes. Ready for branch + PR.

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

- Version: 0.26.0
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 46 languages (Rust, Python, TypeScript, JavaScript, Go, C, C++, Java, C#, F#, PowerShell, Scala, Ruby, Bash, HCL, Kotlin, Swift, Objective-C, SQL, Protobuf, GraphQL, PHP, Lua, Zig, R, YAML, TOML, Elixir, Erlang, Haskell, OCaml, Julia, Gleam, CSS, Perl, HTML, JSON, XML, INI, Nix, Make, LaTeX, Solidity, CUDA, GLSL, Markdown)
- 16 ChunkType variants (Function, Method, Struct, Class, Interface, Enum, Trait, Constant, Section, Property, Delegate, Event, Module, Macro, Object, TypeAlias)
- Tests: 1456 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/, batch/ are directories
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4567 edges
- Eval: E5-base-v2 90.9% Recall@1, 0.951 NDCG@10, 0.941 MRR on 55-query hard eval
