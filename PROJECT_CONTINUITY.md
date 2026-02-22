# Project Continuity

## Right Now

**batch.rs split + P4 audit.** 2026-02-21.

- P1-P3 merged (#459, #460, #461)
- P4: 18 fixes on branch `fix/p4-audit-findings`
- batch.rs split: 2844-line monolith → batch/ directory (4 files) + CQ-8/CQ-9 read dedup
- Ready to commit split work, then PR

Audit triage: `docs/audit-triage.md`

## Pending Changes

Uncommitted on `fix/p4-audit-findings`:
- `src/cli/batch.rs` deleted → `src/cli/batch/{mod,commands,handlers,pipeline}.rs`
- `src/cli/commands/read.rs` rewritten with shared core functions
- `src/health.rs` cosmetic comment fix
- `CONTRIBUTING.md` architecture section updated

## Parked

- **Pre-built release binaries** (GitHub Actions) — deferred
- **`cqs plan` templates** — add more task-type templates as patterns emerge
- **VB.NET language support** — VS2005 project delayed
- **Post-index name matching** — fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred, tracked in #255
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **P4 audit findings** — 18 findings fixed on `fix/p4-audit-findings`
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

- Version: 0.13.0
- MSRV: 1.93
- Schema: v11
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- Tests: 1037 pass + 31 ignored
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/, hnsw/, impact/ are directories (impact split in PR #402)
- convert/ module (7 files) behind `convert` feature flag
- Build target: `~/.cargo-target/cqs/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
- Reference: `aveva` → `samples/converted/aveva-docs/` (10,482 chunks, 76 files)
- type_edges: 4276 edges (Phase 1a+1b complete)
- Eval: E5-base-v2 90.9% Recall@1, 0.941 MRR on 55-query hard eval
