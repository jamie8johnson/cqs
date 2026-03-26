# Configurable Embedding Models Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users configure which ONNX embedding model cqs uses, with E5-base-v2 as default and BGE-large-en-v1.5 as a built-in alternative.

**Architecture:** `ModelConfig` struct holds per-model settings (repo, paths, dim, prefixes). Resolution: CLI flag > env var > config file > default. The `Embedder`, HNSW, CAGRA, Store, and Embedding layers all accept dim as a runtime parameter.

**Tech Stack:** Rust, ONNX Runtime, hf-hub, toml config

## Execution Strategy

```
Phase 1 (parallel):
  Agent A: Task 1 (models.rs) + Task 3 (embedder wiring)
  Agent B: Task 2 (config.rs) — no file overlap

Phase 2 (parallel, after Phase 1 merges):
  Agent C: Task 4a (Embedding::new + Embedder dim fallback) — src/embedder/mod.rs
  Agent D: Task 4b (HNSW dim threading) — src/hnsw/*.rs
  Agent E: Task 4c (CAGRA dim threading) — src/cagra.rs
  Agent F: Task 4d (Store dim + helpers + metadata chain) — src/store/**

Phase 3 (parallel, after Phase 2 merges):
  Agent G: Task 5 (CLI flag + doctor) — src/cli/definitions.rs, dispatch.rs, doctor.rs
  Agent H: Task 6 (export command) — src/cli/commands/export_model.rs ONLY
           (definitions.rs + dispatch.rs routing added by Agent G)

Phase 4 (sequential):
  Task 7: Documentation
  Task 8: Eval
```

**File ownership per phase:**

| Phase | Agent | Files (EXCLUSIVE) |
|-------|-------|-------------------|
| 1 | A | `src/embedder/models.rs` (create), `src/embedder/mod.rs`, 25+ call sites |
| 1 | B | `src/config.rs` |
| 2 | C | `src/embedder/mod.rs` (Embedding::new only) |
| 2 | D | `src/hnsw/mod.rs`, `src/hnsw/build.rs`, `src/hnsw/persist.rs` |
| 2 | E | `src/cagra.rs` |
| 2 | F | `src/store/helpers.rs`, `src/store/metadata.rs`, `src/store/chunks/*`, `src/search/query.rs` |
| 3 | G | `src/cli/definitions.rs`, `src/cli/dispatch.rs`, `src/cli/commands/doctor.rs`, `src/store/metadata.rs` |
| 3 | H | `src/cli/commands/export_model.rs` (create only) |

---

### Task 1: ModelConfig Struct + Presets

**Files:** Create `src/embedder/models.rs`, modify `src/embedder/mod.rs`
**Phase:** 1 (Agent A)

- [ ] **Step 1: Write tests for ModelConfig resolution**

Tests: default → e5-base, env var by name, env var by repo ID (backward compat), CLI overrides env, unknown preset warns and defaults, empty [embedding] section uses default, sad paths (dim=0, empty custom repo).

Use `with_clean_env` helper to save/restore CQS_EMBEDDING_MODEL.

- [ ] **Step 2: Implement ModelConfig**

`resolve(cli_model: Option<&str>, config_embedding: Option<&EmbeddingConfig>) -> Self`

Priority: CLI > env > config > default. `from_preset()` matches both short names AND repo IDs. `tracing::info_span!` on resolve, `tracing::warn!` on unknown preset and invalid custom config. `EmbeddingConfig.model` has `#[serde(default = "default_model_name")]`.

- [ ] **Step 3: Commit**

---

### Task 2: Config File Parsing

**Files:** `src/config.rs`
**Phase:** 1 (Agent B — parallel with Task 1, no file overlap)

- [ ] **Step 1: Add `embedding: Option<EmbeddingConfig>` to Config struct**
- [ ] **Step 2: Write tests (preset, custom, empty section)**
- [ ] **Step 3: Commit**

---

### Task 3: Wire ModelConfig Into Embedder

**Files:** `src/embedder/mod.rs`, 25+ call sites across `src/cli/**`, `src/gather.rs`, etc.
**Phase:** 1 (Agent A — sequential after Task 1, same agent)

- [ ] **Step 1: Add `model_config: ModelConfig` to Embedder, update `new()` and `new_cpu()` signatures**
- [ ] **Step 2: Replace hardcoded prefixes** (`"query: "` → `self.model_config.query_prefix`)
- [ ] **Step 3: Replace hardcoded paths in `ensure_model()`** (use `config.repo`, `config.onnx_path`)
- [ ] **Step 4: Update `model_repo()` to delegate to ModelConfig**
- [ ] **Step 5: Update ALL 25+ Embedder::new() call sites** (grep for complete list)
- [ ] **Step 6: Run full test suite, commit**

---

### Task 4a: Embedding Type + Embedder Dim Fallback

**Files:** `src/embedder/mod.rs` (Embedding::new, try_new, embedding_dim only)
**Phase:** 2 (Agent C)

- [ ] **Step 1: Fix `Embedding::new()`** — remove warn on non-768 (any dim is valid)
- [ ] **Step 2: Fix `Embedding::try_new()`** — accept any dim > 0 instead of rejecting non-768
- [ ] **Step 3: Fix `Embedder::embedding_dim()` fallback** — use `self.model_config.dim` instead of `EMBEDDING_DIM`
- [ ] **Step 4: Run tests, commit**

---

### Task 4b: HNSW Dim Threading

**Files:** `src/hnsw/mod.rs`, `src/hnsw/build.rs`, `src/hnsw/persist.rs`
**Phase:** 2 (Agent D — parallel with 4a/4c/4d, exclusive HNSW files)

- [ ] **Step 1: `prepare_index_data()` — accept `expected_dim: usize` param**
- [ ] **Step 2: `insert_batch()` — use `self.dim` instead of `EMBEDDING_DIM`**
- [ ] **Step 3: `build()` and `build_batched()` — accept `dim: usize`, pass through, set on struct**
- [ ] **Step 4: `load()` — accept `dim: usize` parameter from caller** (data file is bincode graph, not flat vectors — cannot infer dim). Update all `load()` callers to pass dim from Store metadata.
- [ ] **Step 5: Run HNSW tests, commit**

---

### Task 4c: CAGRA Dim Threading

**Files:** `src/cagra.rs`
**Phase:** 2 (Agent E — parallel with 4a/4b/4d, exclusive file)

- [ ] **Step 1: Add `dim: usize` field to CagraIndex struct (or accept as param)**
- [ ] **Step 2: Thread dim through `build()`, `search()`, `build_from_store()`, `build_from_flat()`** (10 production EMBEDDING_DIM sites)
- [ ] **Step 3: Update Array2/buffer allocations to use runtime dim**
- [ ] **Step 4: Run CAGRA tests, commit**

---

### Task 4d: Store Dim + Helpers + Metadata Chain

**Files:** `src/store/helpers.rs`, `src/store/metadata.rs`, `src/store/mod.rs`, `src/store/chunks/embeddings.rs`, `src/store/chunks/query.rs`, `src/store/chunks/async_helpers.rs`, `src/store/chunks/crud.rs`, `src/search/query.rs`
**Phase:** 2 (Agent F — parallel with 4a/4b/4c, exclusive Store files)

- [ ] **Step 1: Add `dim: usize` field to Store struct** — set from metadata `dimensions` key during open, default to EMBEDDING_DIM
- [ ] **Step 2: Remove `MODEL_NAME` and `EXPECTED_DIMENSIONS` constants** — replace with `ModelInfo::new(&ModelConfig)` constructor
- [ ] **Step 3: Fix `ModelInfo::default()`** — use EMBEDDING_DIM, document as test-only
- [ ] **Step 4: Fix `embedding_to_bytes(dim)` / `embedding_slice(dim)` / `bytes_to_embedding(dim)`** — accept `expected_dim: usize`, replace `EXPECTED_BYTES` with `dim * 4`
- [ ] **Step 5: Update all 7+ callers** of embedding byte functions — pass `self.dim` from Store
- [ ] **Step 6: Fix `Store::init()` → `check_model_version()` chain:**
  - `init()` passes `ModelInfo::new(&model_config)` instead of default
  - `check_model_version()` accepts `expected_model: &str` param
  - Add `stored_model_name(&self) -> Option<String>` method (callers check after open, avoids changing Store::open signature)
- [ ] **Step 7: Add test: `embedding_slice` with 1024-dim bytes**
- [ ] **Step 8: Grep for remaining EMBEDDING_DIM in store/search production code**
- [ ] **Step 9: Run store tests, commit**

---

### Task 5: CLI Flag + Model Mismatch + Doctor

**Files:** `src/cli/definitions.rs`, `src/cli/dispatch.rs`, `src/cli/commands/doctor.rs`
**Phase:** 3 (Agent G — after Phase 2 merges)

- [ ] **Step 1: Add `--model` to Cli struct** (top-level field, not global=true)
- [ ] **Step 2: Wire into ModelConfig::resolve()** in dispatch.rs `run_with()`
- [ ] **Step 3: Update model mismatch warning message**
- [ ] **Step 4: Update `cqs doctor`** to check model consistency
- [ ] **Step 5: Write mismatch integration test**
- [ ] **Step 6: Add ExportModel variant to Commands enum + dispatch routing** (for Task 6's file)
- [ ] **Step 7: Commit**

---

### Task 6: Export Helper Command

**Files:** Create `src/cli/commands/export_model.rs` ONLY
**Phase:** 3 (Agent H — parallel with Task 5. Agent G adds the enum variant + routing, Agent H writes the implementation file)

- [ ] **Step 1: Implement `run_export_model()`** — shell out to optimum, --opset 11, capture stderr, write model.toml template
- [ ] **Step 2: Commit**

---

### Task 7: Documentation + Changelog

**Phase:** 4 (after all code tasks)

- [ ] README model selection section
- [ ] CONTRIBUTING.md architecture update
- [ ] CHANGELOG entry
- [ ] Commit

---

### Task 8: Eval — BGE-large with Enrichment

**Phase:** 4 (after Task 7)

- [ ] Set `CQS_EMBEDDING_MODEL=bge-large`, `cqs index --force`
- [ ] Run enriched hard eval
- [ ] Record results, compare to base E5
- [ ] Switch back to default, reindex
