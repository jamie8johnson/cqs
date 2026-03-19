# P4 Refactors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 10 refactors — file splits, deduplication, performance, config, caching. No behavioral changes.

**Architecture:** 3 waves. Wave 1: small self-contained fixes (4 tasks). Wave 2: medium module work (2 tasks). Wave 3: file splits (4 tasks).

**Tech Stack:** Rust, clap, lru, sqlx

---

## Wave 1: Small fixes (all independent, parallelizable)

### Task 1: PERF-12 — CAGRA non-consuming search (DEFERRED — waiting on upstream)

**Status:** Upstream fix merged (rapidsai/cuvs PR #1839, 2026-03-04). Changes `search(self)` to `search(&self)`. Not yet released — expected in cuvs v26.04.00 (April 2026).

**When v26.04.00 releases:**
- [ ] Bump `cuvs` in Cargo.toml to 26.4.0
- [ ] Remove `IndexRebuilder` struct and `Drop` impl from `src/cagra.rs`
- [ ] Remove `ensure_index_rebuilt` method
- [ ] Simplify `search()` — no need to take index from Mutex, just borrow it
- [ ] `Mutex<Option<Index>>` can become `Mutex<Index>` (never None after build)
- [ ] Remove `rebuild_index_with_resources` (only called from IndexRebuilder)
- [ ] Verify CAGRA tests pass

**For now: skip this task.** Do lazy rebuild as a stopgap only if batch/REPL perf complaints arise before April.

Commit: `perf(cagra): non-consuming search — eliminate index rebuild per query (PERF-12)`

---

### Task 2: CQ-11 — Store::open_with_config (~80 lines deduped)

**Files:** `src/store/mod.rs`

Extract shared logic from `Store::open` and `Store::open_readonly` into `open_with_config`.

- [ ] Define internal `StoreConfig` struct with 5 fields: `read_only`, `worker_threads` (0=current_thread), `max_connections`, `mmap_size`, `cache_size`
- [ ] Extract `open_with_config(path, config)` with the shared connection setup, integrity check, schema/model/version checks
- [ ] Make `open` and `open_readonly` thin wrappers
- [ ] Verify all store tests pass

Commit: `refactor(store): extract open_with_config — deduplicate open/open_readonly (CQ-11)`

---

### Task 3: DS-9 — Watch mode Store re-open (~10 lines)

**Files:** `src/cli/watch.rs`

After each reindex cycle, close and re-open the Store to clear OnceLock caches.

- [ ] After successful reindex + HNSW save, drop the old Store and open a fresh one
- [ ] Keep embedder and parser across re-opens (they're stateless)
- [ ] Verify watch tests pass

Commit: `fix(watch): re-open Store after reindex to clear stale caches (DS-9)`

---

### Task 4: RM-18 — BatchContext reference LRU eviction (~15 lines)

**Files:** `src/cli/batch/mod.rs`

Replace `HashMap<String, ReferenceIndex>` with `LruCache` (lru crate already a dependency).

- [ ] Change `refs: RefCell<HashMap<String, ReferenceIndex>>` to `RefCell<lru::LruCache<String, ReferenceIndex>>`
- [ ] Init with capacity 4
- [ ] Update `get_ref` to use `cache.get()`/`cache.put()`
- [ ] Verify batch tests pass

Commit: `fix(batch): LRU eviction for reference index cache (RM-18)`

---

## Wave 2: Medium module work (independent, parallelizable)

### Task 5: EX-9 — LLM config env/config overrides (~55 lines)

**Files:** `src/llm.rs`, `src/config.rs`

- [ ] Define `LlmConfig` struct: `api_base`, `model`, `max_tokens`
- [ ] Add `from_env_and_config()` — checks `CQS_LLM_MODEL`, `CQS_API_BASE`, `CQS_LLM_MAX_TOKENS` env vars, falls back to Config fields, then constants
- [ ] Add `llm_model`, `llm_api_base`, `llm_max_tokens` optional fields to Config struct
- [ ] Update `Client::new` to accept `LlmConfig`
- [ ] Update `llm_summary_pass` to construct LlmConfig from env+config

Commit: `feat(llm): env/config overrides for model, API base, max tokens (EX-9)`

---

### Task 6: EX-8 — Shared CLI/batch arg structs (large, mechanical)

**Files:** `src/cli/mod.rs`, `src/cli/batch/commands.rs`, new `src/cli/args.rs`

- [ ] Create `src/cli/args.rs` with shared arg structs for the 16 commands that have CLI+batch duplicates
- [ ] Start with top 5 most-duplicated: `GatherArgs`, `ImpactArgs`, `ScoutArgs`, `ContextArgs`, `DeadArgs`
- [ ] Use `#[command(flatten)]` in both Commands and BatchCmd enums
- [ ] Update dispatch code to destructure from the flattened struct
- [ ] Verify CLI and batch tests pass

Commit: `refactor(cli): shared arg structs for CLI/batch commands (EX-8)`

---

## Wave 3: File splits (independent, parallelizable)

### Task 7: Split search.rs → search/ module

**Files:** `src/search.rs` → `src/search/mod.rs`, `src/search/scoring.rs`, `src/search/query.rs`

- [ ] Create `src/search/` directory
- [ ] Move target resolution to `mod.rs` (~110 lines)
- [ ] Move scoring helpers to `scoring.rs` (~520 lines + ~710 tests)
- [ ] Move Store impl search methods to `query.rs` (~390 lines + ~630 tests)
- [ ] Update imports across codebase (anything that `use crate::search::*`)
- [ ] Verify all search tests pass

Commit: `refactor: split search.rs into search/ module (scoring, query, mod)`

---

### Task 8: Extract enrichment from pipeline.rs

**Files:** `src/cli/pipeline.rs` → extract to `src/cli/enrichment.rs`

- [ ] Move `enrichment_pass`, `compute_enrichment_hash_with_summary`, `flush_enrichment_batch` (~260 lines)
- [ ] Re-export from `pipeline.rs`: `pub use enrichment::enrichment_pass;`
- [ ] Update callers
- [ ] Verify pipeline tests pass

Commit: `refactor: extract enrichment pass from pipeline.rs (CQ-7)`

---

### Task 9: Extract ORT provider from embedder.rs

**Files:** `src/embedder.rs` → convert to `src/embedder/mod.rs` + `src/embedder/provider.rs`

- [ ] Create `src/embedder/` directory, rename `embedder.rs` to `embedder/mod.rs`
- [ ] Move ORT provider functions to `provider.rs` (~240 lines)
- [ ] Export `select_provider()` and `create_session()` from provider module
- [ ] Verify embedder tests pass

Commit: `refactor: extract ORT provider setup into embedder/provider module`

---

### Task 10: EX-11 — ScoringConfig (after Task 7)

**Files:** `src/search/scoring.rs` (after split)

- [ ] Define `ScoringConfig` struct with all 9 scoring constants
- [ ] Add `const DEFAULT` implementation
- [ ] Update `NameMatcher`, `chunk_importance`, `apply_parent_boost`, `note_boost` to use `ScoringConfig::DEFAULT.*`
- [ ] Verify search tests pass

**Dependency:** Do after Task 7 (search.rs split).

Commit: `refactor(search): consolidate scoring constants into ScoringConfig (EX-11)`

---

## Parallelism Map

| Wave | Tasks | Can parallelize? |
|------|-------|-----------------|
| 1 | 1, 2, 3, 4 | All 4 independent |
| 2 | 5, 6 | Both independent |
| 3 | 7, 8, 9 | All 3 independent |
| Post | 10 | After 7 |

**File ownership (no overlaps within waves):**
- Wave 1: cagra.rs, store/mod.rs, watch.rs, batch/mod.rs
- Wave 2: llm.rs + config.rs, cli/mod.rs + batch/commands.rs + cli/args.rs
- Wave 3: search.rs→search/, pipeline.rs→enrichment.rs, embedder.rs→embedder/
