# BGE-large-en-v1.5 as Alternative Embedding Model

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make cqs support multiple embedding models — ship with E5-base-v2 as default, add BGE-large-en-v1.5 as a configurable option. BGE-large scores 61.8% raw R@1 vs E5-base's 49.1% on confusable code pairs. With enrichment, it may close the gap or surpass 92.7%.

**Architecture:** `CQS_EMBEDDING_MODEL` env var already exists. Extend the Embedder to handle different prefix conventions and dimensions per model. Store the model ID in index metadata so reindex is required on model change.

**Tech Stack:** Rust, ONNX Runtime, HuggingFace model hub

---

### Task 1: Model Configuration Registry

**Files:**
- Create: `src/embedder/models.rs`
- Modify: `src/embedder/mod.rs`

Add a model configuration struct that captures per-model differences:

```rust
pub struct ModelConfig {
    /// HuggingFace repo ID (e.g., "intfloat/e5-base-v2")
    pub repo: &'static str,
    /// ONNX model file path within the repo
    pub model_file: &'static str,
    /// Tokenizer file
    pub tokenizer_file: &'static str,
    /// Embedding dimension
    pub dim: usize,
    /// Max sequence length
    pub max_seq_length: usize,
    /// Query prefix (prepended to search queries)
    pub query_prefix: &'static str,
    /// Document prefix (prepended to indexed content)
    pub doc_prefix: &'static str,
}
```

Built-in configs:
- `E5_BASE_V2`: repo="intfloat/e5-base-v2", dim=768, query_prefix="query: ", doc_prefix="passage: "
- `BGE_LARGE_V1_5`: repo="BAAI/bge-large-en-v1.5", dim=1024, query_prefix="Represent this sentence for searching relevant passages: ", doc_prefix=""

Resolve model from `CQS_EMBEDDING_MODEL` env var or config file.

### Task 2: Prefix-Aware Embedding

**Files:**
- Modify: `src/embedder/mod.rs`

Currently `embed_query` prepends "query: " and `embed_documents` prepends "passage: " hardcoded. Change to use `ModelConfig.query_prefix` and `ModelConfig.doc_prefix`.

The Embedder already has runtime dimension detection (#682). Wire it to use `ModelConfig.dim` as the expected default.

### Task 3: ONNX Model Download for BGE-large

**Files:**
- Modify: `src/embedder/mod.rs` (model download logic)

BGE-large-en-v1.5 has ONNX files at `BAAI/bge-large-en-v1.5` on HuggingFace. Verify the ONNX path structure and ensure the download logic handles different repo layouts.

If BGE-large doesn't have pre-built ONNX, we need to export it:
```bash
optimum-cli export onnx --model BAAI/bge-large-en-v1.5 bge-large-onnx/
```

### Task 4: Index Metadata — Model Tracking

**Files:**
- Modify: `src/store/metadata.rs`

Already stores `model_name` in metadata. Verify that:
1. Changing `CQS_EMBEDDING_MODEL` triggers a reindex warning
2. `cqs doctor` checks model consistency
3. The stored model ID matches the configured model

### Task 5: CLI Integration

**Files:**
- Modify: `src/cli/definitions.rs`

Add `--model` flag to `cqs index` and `cqs` (global):
```
--model <MODEL>    Embedding model: "e5-base" (default), "bge-large" [env: CQS_EMBEDDING_MODEL]
```

### Task 6: Documentation

- Update README.md with model selection
- Update CONTRIBUTING.md with model architecture
- Add to CHANGELOG.md

### Task 7: Tests

- Test that BGE-large prefix is applied correctly
- Test model switching detection (warns on mismatch)
- Test that embed_query/embed_documents use the configured prefix
- Hard eval comparison: E5-base vs BGE-large with cqs enrichment (Rust eval)

### Task 8: Eval — BGE-large with Enrichment

This is the key question: does BGE-large + enrichment beat E5-base + enrichment (92.7%)?

Run the Rust hard eval with BGE-large:
1. Export BGE-large to ONNX
2. Set `CQS_EMBEDDING_MODEL=bge-large`
3. `cqs index --force`
4. Run `cargo test --features gpu-index test_hard`

If BGE-large + enrichment > 92.7%, it becomes the recommended model.
If not, it remains an option for users who want 1024-dim embeddings.
