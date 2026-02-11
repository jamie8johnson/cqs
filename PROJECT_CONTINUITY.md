# Project Continuity

## Right Now

**v0.10.2 + Proactive hints & diff-aware impact.** 2026-02-11.

Branch `feat/proactive-hints-diff-impact` — two features implemented, all tests passing, ready for PR:

1. **Proactive hints** — `cqs explain` and `cqs read --focus` now show caller count + test count for functions. Skipped for non-function chunk types. JSON output includes `hints` object with `caller_count`, `test_count`, `no_callers`, `no_tests`.

2. **`cqs impact-diff`** — new subcommand. Parses unified diff (from stdin or `git diff`), maps hunks to indexed functions, runs aggregated impact analysis. Shows changed functions, affected callers, and tests to re-run. Supports `--base`, `--stdin`, `--json`.

New files: `src/diff_parse.rs`, `src/cli/commands/impact_diff.rs`, `tests/hints_test.rs`, `tests/impact_diff_test.rs`. Modified: `src/impact.rs`, `src/lib.rs`, `src/cli/mod.rs`, `src/cli/commands/mod.rs`, `src/cli/commands/explain.rs`, `src/cli/commands/read.rs`.

Previous: PR #361 (table chunking + parent retrieval), PR #360 (Markdown RAG).

### Pending
- `docs/notes.toml` — modified, not committed (groom changes)
- `.cqs.toml` — untracked, has aveva-docs reference config
- Need `cqs-impact-diff` skill file (optional)

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files, 38 cross-referenced docs still missing. User converting more PDFs.
- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred from Phase 6, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)

## Architecture

- Version: 0.10.2
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
