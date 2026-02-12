# Project Continuity

## Right Now

**Deferred audit findings complete.** 2026-02-12. Branch: `fix/audit-deferred`. Ready to commit + PR.

All 9 groups implemented. 747 tests pass, clippy clean, fresh-eyes complete.

- Group A: Unified `is_test_chunk()` in lib.rs, replaced 3 divergent call sites
- Group B: `DeadFunction` + `DeadConfidence` scoring, `ENTRY_POINT_NAMES`, `--min-confidence` CLI
- Group C: Embedder `clear_session(&self)` via `Mutex<Option<Session>>`, watch idle clearing (5min)
- Group D: Pipeline `file_batch_size` 100K → 5K
- Group E: Improved HNSW checksum error messages + stale temp cleanup
- Group F: HNSW file locking (exclusive save, shared load)
- Group G: `HnswIndex::insert_batch()` for incremental HNSW
- Group I: `extract_imports()` helper + C/SQL/Markdown support in `where_to_add`
- Group J: Doc comment on `LocalPatterns` explaining string-based design

**Descoped:** Group H (#389) — CAGRA GPU memory, requires new disk persistence layer

**Prior audit totals:**
- P1: 26 fixes (PR #360), P2: 41 fixes (PR #380), P3: 40 fixes (PR #381)
- Total: 107 fixes + this deferred PR = ~116 fixes across 4 PRs

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` skill** — template-based planning using scout/impact data
- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)

## Architecture

- Version: 0.12.1
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 441 lib + 297 integration + 7 doc (747 total)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
