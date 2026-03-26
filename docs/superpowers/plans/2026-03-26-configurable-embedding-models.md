# Configurable Embedding Models Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users configure which ONNX embedding model cqs uses, with E5-base-v2 as default and BGE-large-en-v1.5 as a built-in alternative.

**Architecture:** `ModelConfig` struct holds per-model settings (repo, paths, dim, prefixes). Resolution: CLI flag > env var > config file > default. The `Embedder`, HNSW, CAGRA, Store, and Embedding layers all accept dim as a runtime parameter, replacing the compile-time `EMBEDDING_DIM` constant in production code.

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

Key points vs. original plan:
- `resolve()` signature: `pub fn resolve(cli_model: Option<&str>, config_embedding: Option<&EmbeddingConfig>) -> Self`
- Priority: CLI flag > env var > config file > default
- `from_preset()` matches BOTH short names ("bge-large") AND repo IDs ("BAAI/bge-large-en-v1.5") for backward compat
- `from_embedding_config()` logs `tracing::warn!` when custom model missing required fields (repo, dim)
- `resolve()` uses `tracing::info_span!` (not debug_span) per project convention
- `EmbeddingConfig.model` field has `#[serde(default = "default_model_name")]` where default returns `"e5-base"`

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

- [ ] **Step 5: Update ALL Embedder::new() call sites**

Grep: `Embedder::new()` appears in ~20+ files. Most should pass `ModelConfig::resolve(None, config.embedding.as_ref())`. Index command passes CLI flag. List of call sites to check:
- `src/cli/pipeline.rs`
- `src/cli/commands/index.rs`
- `src/cli/batch/mod.rs`
- `src/cli/commands/query.rs`
- `src/cli/commands/resolve.rs`
- `src/gather.rs`, `src/scout.rs`, `src/task.rs`, `src/plan.rs`
- `src/project.rs`
- Tests in `src/embedder/mod.rs`, `tests/`

- [ ] **Step 6: Run full test suite, commit**

---

### Task 4: Thread Dimension Through ALL EMBEDDING_DIM Sites

**This is the critical task.** Every production-code use of `EMBEDDING_DIM` must accept runtime dim.

**Files:**
- `src/embedder/mod.rs` — `Embedding::new()`, `Embedding::try_new()` (lines ~107, ~133)
- `src/hnsw/mod.rs` — `prepare_index_data()` (line ~256), `insert_batch()` (line ~207)
- `src/hnsw/build.rs` — `build()`, `build_batched()` (8 refs, already use `dim` field but set from EMBEDDING_DIM)
- `src/hnsw/persist.rs` — `load()` security validation + `dim` field setting (lines ~472, ~524)
- `src/hnsw/safety.rs` — test-only, no production change needed
- `src/cagra.rs` — 15 references across `build()`, `search()`, `build_incremental()`, buffer allocation
- `src/store/helpers.rs` — `EXPECTED_DIMENSIONS` (line ~29), `MODEL_NAME` (line ~25), `EXPECTED_BYTES` (lines ~908, ~926)
- `src/lib.rs` — `EMBEDDING_DIM` constant stays but doc comment updated

- [ ] **Step 1: Fix `Embedding::new()` and `try_new()`**

These validate `data.len() == EMBEDDING_DIM`. Change to accept any dimension (the Embedder's `detected_dim` validates consistency). Remove the hardcoded check or make it a warn instead of reject.

```rust
// Before: panics/warns on non-768
// After: accepts any dimension, Embedder validates consistency
pub fn new(data: Vec<f32>) -> Self { ... }
pub fn try_new(data: Vec<f32>) -> Option<Self> { ... }
```

- [ ] **Step 2: Fix `prepare_index_data()` — accept `expected_dim: usize`**

- [ ] **Step 3: Fix `insert_batch()` — use `self.dim` instead of `EMBEDDING_DIM`**

- [ ] **Step 4: Fix `hnsw/persist.rs` `load()` — read dim from stored metadata or accept as parameter**

The security validation calculates expected data file size using `EMBEDDING_DIM * 4`. Must use the stored index dim. The loaded `HnswIndex` must set `dim` from the actual data, not from `EMBEDDING_DIM`.

- [ ] **Step 5: Fix `cagra.rs` — thread dim through all 15 sites**

`build()`, `search()`, `build_incremental()` all use `EMBEDDING_DIM` for:
- `Array2::from_shape_vec` dimensions
- Buffer allocation (`EMBEDDING_DIM * 4`)
- Dimension validation

Accept `dim: usize` as parameter or store on the struct.

- [ ] **Step 6: Fix `store/helpers.rs`**

- `EXPECTED_DIMENSIONS`: change from `EMBEDDING_DIM as u32` to a function that reads from ModelConfig or Store metadata
- `MODEL_NAME`: change from hardcoded `"intfloat/e5-base-v2"` to runtime value from ModelConfig
- `EXPECTED_BYTES`: calculations using `EMBEDDING_DIM * 4` must use runtime dim

- [ ] **Step 7: Verify — grep for remaining EMBEDDING_DIM in production code**

```bash
grep -rn "EMBEDDING_DIM" src/ | grep -v "#\[cfg(test)\]" | grep -v "mod tests" | grep -v "fn test_" | grep -v "//"
```

Only `src/lib.rs` (the constant definition) should remain. All other production-code references should use runtime dim.

- [ ] **Step 8: Run full test suite, commit**

---

### Task 5: CLI Flag + Model Mismatch + Doctor

**Files:**
- Modify: `src/cli/definitions.rs` — `--model` flag on top-level `Cli` struct
- Modify: `src/cli/dispatch.rs` — pass to ModelConfig resolution
- Modify: `src/store/metadata.rs` — improve mismatch message
- Modify: `src/cli/commands/doctor.rs` — validate model consistency

- [ ] **Step 1: Add `--model` to Cli struct**

```rust
/// Embedding model: e5-base (default), bge-large, or a custom preset name
#[arg(long, global = true)]
pub model: Option<String>,
```

Using `global = true` so it's available on all subcommands (index, search, etc.).

- [ ] **Step 2: Wire CLI flag into ModelConfig::resolve()**

In dispatch, extract `cli.model.as_deref()` and pass as first arg to `ModelConfig::resolve()`.

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
