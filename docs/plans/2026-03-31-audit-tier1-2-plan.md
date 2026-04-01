# Audit Tier 1+2 Implementation Plan

## Goal
Fix remaining P2/P3 findings from the v1.13.0 audit + establish the "configurable constants" pattern that unlocks research extensibility.

## Phases

### Phase 1: Configurable Constants (7 agents, parallel, no file conflicts)

Each agent gets ONE file. Pattern: `const FOO: usize = N;` → read from `CQS_<NAME>` env var with fallback. Add tracing::info! on override. Add unit test for default + override.

| Agent | Finding | File | Constant | Env Var | Default |
|-------|---------|------|----------|---------|---------|
| A1 | SHL-6 | `src/hnsw/mod.rs` | M=24, ef_construction=200, ef_search=100 | `CQS_HNSW_M`, `CQS_HNSW_EF_CONSTRUCTION`, `CQS_HNSW_EF_SEARCH` | 24, 200, 100 |
| A2 | SHL-12 | `src/cli/pipeline.rs` | EMBED_BATCH_SIZE=64 | `CQS_EMBED_BATCH_SIZE` | 64 |
| A3 | SHL-2 | `src/reranker.rs` | max_length=512 | `CQS_RERANKER_MAX_LENGTH` | 512 |
| A4 | SHL-8 | `src/gather.rs` | DEFAULT_MAX_EXPANDED_NODES=200 | `CQS_GATHER_MAX_NODES` | 200 |
| A5 | SHL-9 | `src/impact/bfs.rs` | DEFAULT_BFS_MAX_NODES=10000 | `CQS_IMPACT_MAX_NODES` | 10000 |
| A6 | SHL-11 | `src/reference.rs` + `src/project.rs` | num_threads(4) | `CQS_RAYON_THREADS` | 4 |
| A7 | SHL-15 | `src/search/query.rs` | DEFAULT_QUERY_CACHE_SIZE=32 | `CQS_QUERY_CACHE_SIZE` | 32 (raise default to 128) |

**Each agent must:**
1. Read the file, find the constant
2. Replace with env-var-with-fallback pattern:
   ```rust
   let value: usize = std::env::var("CQS_FOO")
       .ok()
       .and_then(|v| v.parse().ok())
       .unwrap_or(DEFAULT);
   if value != DEFAULT {
       tracing::info!(value, "CQS_FOO override active");
   }
   ```
3. Add `tracing::info_span!` if the function lacks one
4. Add test: `fn test_foo_default()` verifying the default value
5. Add test: `fn test_foo_env_override()` with ENV_MUTEX pattern (set var, check value, restore)
6. `cargo fmt` the changed file

**No agent touches another agent's file. Merge all after Phase 1 completes.**

### Phase 2: Tests + Nits (4 agents, parallel, no file conflicts)

| Agent | Finding | File | Task |
|-------|---------|------|------|
| B1 | TC-2 | `src/language/structured_text.rs` | Add tests for method_definition, action_definition, TYPE_QUERY type extraction. 3 new tests. |
| B2 | TC-4 | `src/search/scoring/candidate.rs` | Add NaN embedding adversarial test. Test that NaN cosine → None/filtered. |
| B3 | PB-2 | `src/cli/watch.rs` | Canonicalize deleted-file paths before comparison with cfg.cqs_dir. |
| B4 | SEC-5 + EH-1 | `src/search/synonyms.rs` + `src/task.rs` + `src/impact/format.rs` | Add debug_assert on synonyms input. Add tracing::debug before .ok() in task/impact serialization. |

**B4 touches 3 files but none overlap with B1-B3.**

### Phase 3: Shared Serialization (1 agent, serial — touches shared files)

| Agent | Findings | Files |
|-------|----------|-------|
| C1 | CQ-1, CQ-3, CQ-5 | `src/cli/commands/trace.rs`, `src/cli/batch/handlers/graph.rs`, shared helper |

**Task:**
1. Extract `bfs_shortest_path` from `src/cli/commands/trace.rs` into `src/impact/bfs.rs` as a public function
2. Make `dispatch_trace` in `src/cli/batch/handlers/graph.rs` call the shared function instead of its inline BFS
3. Extract `format_test_suggestions` from cmd_impact into a shared function in `src/impact/format.rs`
4. Make `dispatch_impact` call the shared function
5. Make `cmd_trace` use batched `search_by_name` like `dispatch_trace` does
6. Add tracing spans to all extracted functions
7. Test: verify trace output is identical between cmd and batch paths

**This agent runs AFTER Phase 1+2 are merged — it touches files that Phase 2 agents also touch (watch.rs, trace.rs).**

## Execution Order

```
Time →
Phase 1: [A1] [A2] [A3] [A4] [A5] [A6] [A7]  (parallel, 7 agents)
          ↓ merge all ↓
Phase 2: [B1] [B2] [B3] [B4]                    (parallel, 4 agents)
          ↓ merge all ↓
Phase 3: [C1]                                    (serial, 1 agent)
          ↓ merge ↓
Done: PR + CI + binary rebuild
```

## Verification Gate Between Phases

After each phase merge:
- `cargo build --features gpu-index` — zero errors
- `cargo clippy --features gpu-index -- -D warnings` — zero warnings
- `cargo test --features gpu-index --lib` — all pass
- `git diff --stat` — confirm only expected files changed

## File Conflict Matrix

| File | Phase 1 | Phase 2 | Phase 3 |
|------|---------|---------|---------|
| hnsw/mod.rs | A1 | — | — |
| cli/pipeline.rs | A2 | — | — |
| reranker.rs | A3 | — | — |
| gather.rs | A4 | — | — |
| impact/bfs.rs | A5 | — | C1 (adds fn) |
| reference.rs | A6 | — | — |
| project.rs | A6 | — | — |
| search/query.rs | A7 | — | — |
| language/structured_text.rs | — | B1 | — |
| search/scoring/candidate.rs | — | B2 | — |
| cli/watch.rs | — | B3 | — |
| search/synonyms.rs | — | B4 | — |
| task.rs | — | B4 | — |
| impact/format.rs | — | B4 | C1 (adds fn) |
| cli/commands/trace.rs | — | — | C1 |
| cli/batch/handlers/graph.rs | — | — | C1 |

**Zero conflicts within any phase. Phase 3 waits for Phase 2 merge.**
