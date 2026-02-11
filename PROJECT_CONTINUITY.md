# Project Continuity

## Right Now

**7 agent experience features.** 2026-02-11.

Plan at `/home/user001/.claude/plans/linear-brewing-flame.md`. Build order: 2→6→5→3→4→1→7.

### Done
- PR #365: `cqs stale` + proactive staleness warnings (Features 2+6). Merged.
- PR #366: `cqs context --compact` (Feature 5). Merged.
- PR #367: `cqs related` (Feature 3). Merged.

### Next
- Feature 4: `cqs impact --suggest-tests` — test suggestions for untested callers
- Feature 1: `cqs where` — placement suggestion for new code
- Feature 7: `cqs scout` — pre-investigation dashboard (capstone)

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files, 38 cross-referenced docs still missing. User converting more PDFs.
- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred from Phase 6, tracked in #255
- **Speculative R&D: `cqs plan`** — strong AI planning. Agent reasoning between sequential calls is load-bearing; collapsing it risks removing valuable intermediate decisions. Revisit when `scout` proves sufficient.

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)

## Architecture

- Version: 0.11.0
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
