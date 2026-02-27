# Audit Triage — v0.19.0

Generated: 2026-02-27

## Summary

- **Total findings:** ~123 (41 Batch 1 + 43 Batch 2 + 39 Batch 3)
- **14 categories** across 3 batches
- **Findings detail lost** — agents wrote findings but file reverted during context compaction. This triage covers the P1+P2 items that were identified, triaged, and fixed.
- **Prior audit (v0.14.0):** archived as `audit-triage-v0.19.0-pre.md`

## P1: Easy + High Impact — Fix Immediately

All P1 items fixed.

| # | Finding | Difficulty | Location | Status |
|---|---------|-----------|----------|--------|
| 1 | **AC-1/AC-2/PF-5**: `partial_cmp().unwrap_or(Equal)` — use `f32::total_cmp()` for NaN-safe sorting | easy | 11 sites across 8 files (drift.rs, gather.rs, onboard.rs, search.rs, project.rs, reranker.rs, reference.rs, cli/commands/mod.rs) | ✅ fixed |
| 2 | **RB-1**: `first_sentence_or_truncate` panics on multibyte UTF-8 (`doc[..150]` can split codepoint) | easy | nl.rs:543 | ✅ fixed |
| 3 | **DOC-1**: `cqs diff --source <ref>` documented but `--source` flag doesn't exist (correct: `cqs diff <ref>`) | easy | CLAUDE.md, README.md, cqs-bootstrap SKILL.md | ✅ fixed |
| 4 | **DS-8**: `check_origins_stale` builds unbounded SQL placeholders — hits SQLite 999-param limit on large projects | easy | store/chunks.rs:548 | ✅ fixed |
| 5 | **EH-3**: `open_project_store` bare `Store::open` error lacks path context | easy | cli/mod.rs:32 | ✅ fixed |
| 6 | **EH-8**: Batch REPL `let _ = writeln!()` silently swallows broken pipe — should break loop | easy | cli/batch/mod.rs (5 sites) | ✅ fixed |
| 7 | **OB-9**: `gc` command `count_stale_files` error swallowed by `unwrap_or((0,0))` | easy | cli/commands/gc.rs:32 | ✅ fixed |
| 8 | **SEC-1/PB-2**: `config.rs` uses predictable `"toml.tmp"` temp file name | easy | config.rs:329,395 | ✅ fixed |
| 9 | **SEC-8**: `audit.rs` uses predictable `"json.tmp"` temp file name | easy | audit.rs:114 | ✅ fixed |
| 10 | **PF-10**: `get_call_graph` returns duplicate edges — missing `DISTINCT` | easy | store/calls.rs:473 | ✅ fixed |
| 11 | **PF-6**: `replace_file_chunks` does redundant per-chunk `DELETE FROM chunks_fts` after bulk origin DELETE | easy | store/chunks.rs:237 | ✅ fixed |
| 12 | **SEC-7**: Webhelp zip-slip vulnerability | easy | convert/ | ❌ false positive (walks directories, not archives) |

## P2: Medium Effort + High Impact — Fix in Batch

9 of 22 items fixed by parallel agents. 11 deferred as larger refactors. 2 not addressed.

| # | Finding | Difficulty | Location | Status |
|---|---------|-----------|----------|--------|
| 1 | **EX-1/EX-8**: ChunkType Display/FromStr/error duplicated in 4 manual match blocks — use `define_chunk_types!` macro | medium | language/mod.rs | ✅ fixed |
| 2 | **RB-2**: Embedder `embed_batch_inner` missing seq_len and total data length validation | medium | embedder.rs | ✅ fixed |
| 3 | **OB-3**: Pipeline missing elapsed time logging | easy | cli/pipeline.rs | ✅ fixed |
| 4 | **DS-1/DS-6**: Watch mode reindex cycle has no index lock — concurrent writes possible | medium | cli/watch.rs | ✅ fixed |
| 5 | **DS-2**: Watch `reindex_files` not atomic — chunk and call graph can diverge | medium | cli/watch.rs | ✅ fixed |
| 6 | **CQ-2**: `explain` CLI/batch duplicates ~130 lines of JSON assembly | medium | cli/commands/explain.rs, cli/batch/handlers.rs | ✅ fixed |
| 7 | **CQ-3**: `context` CLI/batch duplicates ~120 lines of JSON assembly | medium | cli/commands/context.rs, cli/batch/handlers.rs | ✅ fixed |
| 8 | **AD-8**: `HealthReport` missing `Serialize` derive — CLI/batch hand-assembles JSON | medium | health.rs, impact/hints.rs, suggest.rs, cli/commands/health.rs, cli/batch/handlers.rs, language/mod.rs, store/helpers.rs, tests/cli_health_test.rs | ✅ fixed |
| 9 | **RM-3**: `semantic_diff` loads all embeddings at once — O(N) peak memory | medium | diff.rs | ✅ fixed |
| 10 | **AD-1**: Inconsistent `String` vs `PathBuf` for file paths across result types | medium | multiple | deferred |
| 11 | **AD-5**: Error types inconsistent — some use `StoreError`, others `anyhow` | medium | multiple | deferred |
| 12 | **CQ-5**: Pipeline stage extraction — complex match arms in single function | medium | cli/pipeline.rs | deferred |
| 13 | **CQ-6**: Batch JSON output structs — manual `serde_json::json!` assembly | medium | cli/batch/handlers.rs | deferred |
| 14 | **EH-5**: `.context()` sweep — bare `?` on store operations across CLI | medium | multiple | deferred |
| 15 | **PF-1**: Multi-row INSERT for batch upserts | medium | store/chunks.rs | deferred |
| 16 | **PF-9**: FTS normalization redundantly computed on unchanged content | easy | store/chunks.rs | deferred |
| 17 | **RM-8**: Parallel reference loading in multi-ref search | medium | reference.rs | deferred |
| 18 | **EX-6**: Config expansion — adding config fields requires touching 4 locations | medium | config.rs | deferred |
| 19 | **AC-5**: `bfs_expand` revisits nodes when called with overlapping seeds | easy | gather.rs | deferred |
| 20 | **AC-8**: HNSW candidate multiplier hardcoded — suboptimal for varying index sizes | easy | hnsw/ | deferred |

## P3/P4: Not Addressed This Audit

~86 additional findings across P3 (easy+low impact) and P4 (hard/low impact) were identified by audit agents but the detailed findings were lost during context compaction. These categories were covered:

- Code Quality, Documentation, API Design, Error Handling, Observability (Batch 1)
- Test Coverage, Robustness, Algorithm Correctness, Extensibility, Platform Behavior (Batch 2)
- Security, Data Safety, Performance, Resource Management (Batch 3)

If a future audit is needed, re-running will re-discover these items. The P1+P2 fixes above addressed the highest-value findings.

## Changes Summary

### P1 Fixes (12 items, 1 false positive)

- **NaN-safe sorting**: Replaced `partial_cmp().unwrap_or(Equal)` with `f32::total_cmp()` across 11 sort sites
- **UTF-8 safety**: `floor_char_boundary(150)` before byte-slicing doc strings
- **Security**: PID+timestamp temp file names in config.rs and audit.rs (matches existing note.rs/project.rs pattern)
- **SQL safety**: Batched `check_origins_stale` in groups of 900 (SQLite 999-param limit)
- **Performance**: `SELECT DISTINCT` in `get_call_graph`, removed redundant per-chunk FTS DELETE
- **Error handling**: Store open path context, gc count warn, batch REPL broken-pipe detection
- **Docs**: Corrected `cqs diff` syntax in 3 files

### P2 Fixes (9 items by parallel agents)

- **`define_chunk_types!` macro**: Eliminated 4 manual match blocks for ChunkType (language/mod.rs)
- **Embedder validation**: seq_len and data length bounds checks (embedder.rs)
- **Pipeline timing**: Elapsed time in final tracing log (pipeline.rs)
- **Watch locking**: `acquire_index_lock()` with `try_lock()` before reindex cycles (watch.rs)
- **Atomic transactions**: `upsert_chunks_and_calls()` for chunk+call graph atomicity (watch.rs)
- **CLI/batch dedup**: Shared `pub(crate)` core functions for `explain` and `context` (-284 lines)
- **HealthReport Serialize**: Proper derive chain with `Hotspot` struct, eliminated ~50 lines of hand-assembled JSON
- **Diff batching**: Embedding loading in batches of 1000 pairs (peak memory ~9MB vs ~240MB)

### Test Results

1212 pass, 0 fail, 35 ignored. No warnings.
