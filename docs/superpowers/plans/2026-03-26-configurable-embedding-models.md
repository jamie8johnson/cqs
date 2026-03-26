# Configurable Embedding Models Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users configure which ONNX embedding model cqs uses, with E5-base-v2 as default and BGE-large-en-v1.5 as a built-in alternative.

**Architecture:** `ModelConfig` struct holds per-model settings (repo, paths, dim, prefixes). Resolution: CLI flag > env var > config file > default (extends spec which only lists env > config > default — CLI flag added for ergonomics). The `Embedder`, HNSW, CAGRA, Store, and Embedding layers all accept dim as a runtime parameter, replacing the compile-time `EMBEDDING_DIM` constant in production code.

**Tech Stack:** Rust, ONNX Runtime, hf-hub, toml config

**Critical note:** `EMBEDDING_DIM` (768) is used in 30+ production code sites. ALL must be threaded to use runtime dim. See Task 4 for the complete list.

---

### Task 1: ModelConfig Struct + Presets

**Files:**
- Create: `src/embedder/models.rs`
- Modify: `src/embedder/mod.rs` (add `mod models; pub use models::{ModelConfig, EmbeddingConfig};`)

- [ ] **Step 1: Write tests for ModelConfig resolution**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn with_clean_env<F: FnOnce() -> R, R>(f: F) -> R {
        let saved = std::env::var("CQS_EMBEDDING_MODEL").ok();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        let result = f();
        match saved {
            Some(v) => std::env::set_var("CQS_EMBEDDING_MODEL", v),
            None => std::env::remove_var("CQS_EMBEDDING_MODEL"),
        }
        result
    }

    #[test]
    fn default_is_e5_base() {
        with_clean_env(|| {
            let config = ModelConfig::resolve(None, None);
            assert_eq!(config.name, "e5-base");
            assert_eq!(config.dim, 768);
            assert_eq!(config.query_prefix, "query: ");
        });
    }

    #[test]
    fn env_var_selects_preset_by_name() {
        std::env::set_var("CQS_EMBEDDING_MODEL", "bge-large");
        let config = ModelConfig::resolve(None, None);
        assert_eq!(config.name, "bge-large");
        assert_eq!(config.dim, 1024);
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn env_var_selects_preset_by_repo_id() {
        // Backward compat: existing users may set full repo ID
        std::env::set_var("CQS_EMBEDDING_MODEL", "BAAI/bge-large-en-v1.5");
        let config = ModelConfig::resolve(None, None);
        assert_eq!(config.name, "bge-large");
        assert_eq!(config.dim, 1024);
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn cli_flag_overrides_env_var() {
        std::env::set_var("CQS_EMBEDDING_MODEL", "e5-base");
        let config = ModelConfig::resolve(Some("bge-large"), None);
        assert_eq!(config.name, "bge-large");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
    }

    #[test]
    fn unknown_preset_warns_and_defaults() {
        with_clean_env(|| {
            std::env::set_var("CQS_EMBEDDING_MODEL", "nonexistent");
            let config = ModelConfig::resolve(None, None);
            assert_eq!(config.name, "e5-base");
            std::env::remove_var("CQS_EMBEDDING_MODEL");
        });
    }
}
```

- [ ] **Step 2: Implement ModelConfig**

Key points:
- `resolve()` signature: `pub fn resolve(cli_model: Option<&str>, config_embedding: Option<&EmbeddingConfig>) -> Self`
- Priority: CLI flag > env var > config file > default
- `from_preset()` matches BOTH short names ("bge-large") AND repo IDs ("BAAI/bge-large-en-v1.5") for backward compat
- **Env var with unknown value:** If CQS_EMBEDDING_MODEL is set to a value that matches neither a preset name nor a preset repo ID, log `tracing::warn!` and fall back to default. This is a behavioral change from the current code (which passes it as a raw repo ID). Document in CHANGELOG.
- `from_embedding_config()` logs `tracing::warn!` when custom model missing required fields (repo, dim)
- `resolve()` uses `tracing::info_span!` (not debug_span) per project convention
- `EmbeddingConfig.model` field has `#[serde(default = "default_model_name")]` where default returns `"e5-base"`
- Add sad-path tests: `dim = 0`, `dim = -1` (serde type error), empty repo string for custom model

- [ ] **Step 3: Run tests, verify pass**

- [ ] **Step 4: Commit**

---

### Task 2: Config File Parsing

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Add EmbeddingConfig to Config struct**

```rust
/// Embedding model configuration
#[serde(default)]
pub embedding: Option<EmbeddingConfig>,
```

- [ ] **Step 2: Write config parsing tests (including empty [embedding] section)**

```rust
#[test]
fn test_empty_embedding_section_uses_default() {
    let toml = "[embedding]\n";
    let config: Config = toml::from_str(toml).unwrap();
    let emb = config.embedding.unwrap();
    assert_eq!(emb.model, "e5-base"); // serde default
}
```

- [ ] **Step 3: Commit**

---

### Task 3: Wire ModelConfig Into Embedder

**Files:**
- Modify: `src/embedder/mod.rs`

- [ ] **Step 1: Add `model_config` field to Embedder, update `new()` signature**

`pub fn new(model_config: ModelConfig) -> Result<Self, EmbedderError>`

Also update `new_cpu()`.

- [ ] **Step 2: Replace hardcoded prefixes in `embed_query()` and `embed_documents()`**

`format!("query: {}", text)` → `format!("{}{}", self.model_config.query_prefix, text)`
`format!("passage: {}", t)` → `format!("{}{}", self.model_config.doc_prefix, t)`

- [ ] **Step 3: Replace hardcoded paths in `ensure_model()`**

Accept `&ModelConfig`. Use `config.repo`, `config.onnx_path`, `config.tokenizer_path`.

- [ ] **Step 4: Update `model_repo()` for backward compat**

```rust
pub fn model_repo() -> String {
    ModelConfig::resolve(None, None).repo
}
```

- [ ] **Step 5: Update ALL Embedder::new() and new_cpu() call sites (25+ files)**

Run `grep -rn "Embedder::new\b" src/` to get the complete list. Known sites:
- `src/cli/pipeline.rs` (new + new_cpu)
- `src/cli/commands/index.rs`
- `src/cli/commands/query.rs` (3 instances)
- `src/cli/commands/resolve.rs`
- `src/cli/commands/context.rs`
- `src/cli/commands/explain.rs`
- `src/cli/commands/onboard.rs`
- `src/cli/commands/project.rs`
- `src/cli/commands/init.rs`
- `src/cli/batch/mod.rs`
- `src/cli/watch.rs`
- `src/gather.rs`, `src/scout.rs`, `src/task.rs`, `src/plan.rs`
- `src/project.rs`
- Tests: `src/embedder/mod.rs`, `tests/*.rs`

Most pass `ModelConfig::resolve(None, config.embedding.as_ref())`. The index command passes CLI flag as first arg.

- [ ] **Step 6: Run full test suite, commit**

---

### Task 4: Thread Dimension Through ALL EMBEDDING_DIM Sites

**This is the critical task.** Every production-code use of `EMBEDDING_DIM` must accept runtime dim.

**Complete EMBEDDING_DIM site inventory (verified by grep):**

| File | Line(s) | Usage | Fix |
|------|---------|-------|-----|
| `src/embedder/mod.rs` | ~107 | `Embedding::new()` warns on non-768 | Remove warn (Embedder validates via detected_dim) |
| `src/embedder/mod.rs` | ~133 | `Embedding::try_new()` rejects non-768 | Change to accept any dim > 0 |
| `src/hnsw/mod.rs` | ~207 | `insert_batch()` validates `emb.len() != EMBEDDING_DIM` | Use `self.dim` |
| `src/hnsw/mod.rs` | ~256,268 | `prepare_index_data()` validates + allocates buffer | Accept `expected_dim` param |
| `src/hnsw/build.rs` | 8 sites | `build()`/`build_batched()` set `dim: EMBEDDING_DIM` | Accept `dim` param, pass through |
| `src/hnsw/persist.rs` | ~472 | `load()` security validation: file size check | Infer dim from `data_size / (id_count * 4)` |
| `src/hnsw/persist.rs` | ~524 | `load()` sets `dim: EMBEDDING_DIM` | Set from inferred dim |
| `src/cagra.rs` | ~12 sites | `build()`, `search()`, `build_from_store()`, `build_from_flat()` | Store dim on struct, thread through |
| `src/store/helpers.rs` | ~25 | `MODEL_NAME` constant | Remove constant, use runtime ModelConfig |
| `src/store/helpers.rs` | ~29 | `EXPECTED_DIMENSIONS = EMBEDDING_DIM as u32` | Remove, pass dim to `ModelInfo` |
| `src/store/helpers.rs` | ~892 | `embedding_to_bytes()` validates `len != EXPECTED_DIMENSIONS` | Accept expected dim param |
| `src/store/helpers.rs` | ~908,926 | `EXPECTED_BYTES` calculations | Use runtime dim |
| `src/store/helpers.rs` | `ModelInfo::default()` | Returns hardcoded MODEL_NAME + 768 | Accept ModelConfig param |
| `src/hnsw/safety.rs` | tests only | `EMBEDDING_DIM` in test assertions | Leave as-is (tests use default model) |
| `src/store/types.rs` | ~625 | test helper `vec![0.0; EMBEDDING_DIM]` | Leave as-is |

**NOT in scope:** ~30 test files with hardcoded `vec![0.0; 768]` — these test the default model and should continue using `EMBEDDING_DIM`. No change needed.

- [ ] **Step 1: Fix `Embedding::new()` and `try_new()`**

`new()` currently logs `tracing::warn!` on non-768 but accepts. Remove the warn (any dim is valid).
`try_new()` currently rejects non-768. Change to accept any dim > 0 (non-empty, finite values).

- [ ] **Step 2: Fix `prepare_index_data()` — accept `expected_dim: usize`**

- [ ] **Step 3: Fix `insert_batch()` — use `self.dim` instead of `EMBEDDING_DIM`**

- [ ] **Step 4: Fix `hnsw/persist.rs` `load()`**

The data file doesn't store dimension explicitly. Infer it:
```rust
let inferred_dim = data_file_size / (id_map.len() * std::mem::size_of::<f32>());
```
Use inferred dim for the security check AND set `dim: inferred_dim` on the loaded index.

- [ ] **Step 5: Fix `cagra.rs` — thread dim through all 12 production sites**

Functions: `build()`, `search()`, `build_from_store()`, `build_from_flat()`.
Note: `build_incremental()` does NOT exist in this file. Add `dim: usize` field to the CAGRA struct or accept as parameter.

- [ ] **Step 6: Fix `store/helpers.rs` — the structural chain**

This is the critical structural fix:

1. **Remove `MODEL_NAME` constant** — replace with `ModelInfo::new(config: &ModelConfig)` constructor
2. **Remove `EXPECTED_DIMENSIONS` constant** — `ModelInfo` gets dim from ModelConfig
3. **Fix `ModelInfo::default()`** — should use `EMBEDDING_DIM` (E5-base default), document that this is for tests only
4. **Fix `embedding_to_bytes()`** — accept `expected_dim: usize` parameter
5. **Fix `EXPECTED_BYTES` calculations** — use runtime dim

- [ ] **Step 7: Fix `Store::init()` → `check_model_version()` chain**

The structural threading problem:
- `Store::init(&ModelInfo)` — already accepts model info, writes to metadata. Pass `ModelInfo::new(&model_config)` instead of `ModelInfo::default()`.
- `check_model_version()` at `src/store/metadata.rs:113` — currently compares stored model vs `MODEL_NAME` constant. Change signature to `check_model_version(&self, expected_model: &str) -> Result<(), StoreError>`. Callers pass `model_config.repo`.
- `Store::open()` takes only `Path` — does NOT have ModelConfig. Two options:
  (a) Add `expected_model: Option<&str>` param to `open()`, or
  (b) Defer check to caller (Store returns stored model name, caller compares)

  **Recommend (b):** Add `pub fn stored_model_name(&self) -> Option<String>` method. Callers check after open. This avoids changing the Store::open() signature for all 20+ callers.

- [ ] **Step 8: Verify — grep for remaining EMBEDDING_DIM in production code**

```bash
grep -rn "EMBEDDING_DIM" src/ | grep -v "test\|Test\|#\[cfg(test)\]" | grep -v "// "
```

After fixes, only `src/lib.rs` (constant definition) and test code should reference `EMBEDDING_DIM`.

- [ ] **Step 9: Run full test suite, commit**

---

### Task 5: CLI Flag + Model Mismatch + Doctor

**Files:**
- Modify: `src/cli/definitions.rs` — `--model` flag on top-level `Cli` struct
- Modify: `src/cli/dispatch.rs` — pass to ModelConfig resolution
- Modify: `src/store/metadata.rs` — improve mismatch message
- Modify: `src/cli/commands/doctor.rs` — validate model consistency

- [ ] **Step 1: Add `--model` to Cli struct**

This codebase does NOT use clap `global = true` anywhere. Instead, add `--model` as a top-level field on `Cli`:
```rust
/// Embedding model: e5-base (default), bge-large
#[arg(long)]
pub model: Option<String>,
```

In `dispatch.rs`, extract `cli.model.as_deref()` early and thread it into `ModelConfig::resolve()`. Since `dispatch.rs` already has access to the full `Cli` struct, this doesn't require global propagation — just pass it to the pipeline functions that create Embedders.

- [ ] **Step 2: Wire CLI flag into ModelConfig::resolve()**

In `run_with()` in `dispatch.rs`:
```rust
let model_config = ModelConfig::resolve(cli.model.as_deref(), config.embedding.as_ref());
```
Pass `model_config` (or `cli.model`) to functions that create Embedders. Most commands create Embedders via helper functions — update those to accept `Option<&str>` for the model override.

- [ ] **Step 3: Update `check_model_version()` to use runtime MODEL_NAME**

Replace `const MODEL_NAME` comparison with the active ModelConfig's repo. Error message:
```
Model mismatch: index uses "{stored}" but configured model is "{configured}".
Run `cqs index --force` to reindex with the new model.
```

- [ ] **Step 4: Update `cqs doctor` to check model consistency**

In `src/cli/commands/doctor.rs`, add a check that the configured model matches the indexed model. Report as a warning, not an error.

- [ ] **Step 5: Write model mismatch integration test**

- [ ] **Step 6: Run tests, commit**

---

### Task 6: Export Helper Command

**Files:**
- Create: `src/cli/commands/export_model.rs`
- Modify: `src/cli/definitions.rs`, `src/cli/dispatch.rs`

- [ ] **Step 1: Implement `cqs export-model`**

Shells out to `python3 -m optimum.exporters.onnx --model <repo> --task feature-extraction --opset 11 <output>`. Captures stderr on failure for diagnostic output. Writes `model.toml` template.

- [ ] **Step 2: Route command, commit**

---

### Task 7: Documentation + Changelog

- [ ] **Step 1: README model selection section**
- [ ] **Step 2: CONTRIBUTING.md architecture update (models.rs)**
- [ ] **Step 3: CHANGELOG entry**
- [ ] **Step 4: Commit**

---

### Task 8: Eval — BGE-large with Enrichment

- [ ] **Step 1: Set `CQS_EMBEDDING_MODEL=bge-large`, `cqs index --force`**
- [ ] **Step 2: Run Rust hard eval: `cargo test --features gpu-index test_hard -- --nocapture`**
- [ ] **Step 3: Record results, compare to E5-base 92.7%**
- [ ] **Step 4: Switch back to default, reindex**
