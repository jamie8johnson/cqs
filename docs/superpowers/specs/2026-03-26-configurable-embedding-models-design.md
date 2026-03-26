# Configurable Embedding Models

**Date:** 2026-03-26
**Status:** Approved

## Problem

cqs hardcodes E5-base-v2 as the embedding model. Users cannot swap to a different model without modifying source code. Testing showed BGE-large-en-v1.5 scores 61.8% raw R@1 vs E5-base's 49.1% on confusable code — with enrichment, it may surpass 92.7%. The product should support model selection.

## Design

### ModelConfig

A struct capturing everything cqs needs to use an embedding model:

```rust
pub struct ModelConfig {
    /// Short name for display/config (e.g., "e5-base", "bge-large")
    pub name: &'static str,
    /// HuggingFace repo ID
    pub repo: String,
    /// ONNX model file path within the repo
    pub onnx_path: String,
    /// Tokenizer file path within the repo
    pub tokenizer_path: String,
    /// Embedding dimension
    pub dim: usize,
    /// Max sequence length (tokens)
    pub max_seq_length: usize,
    /// Prefix prepended to search queries
    pub query_prefix: String,
    /// Prefix prepended to indexed documents
    pub doc_prefix: String,
}
```

### Built-in Presets

| Name | Repo | Dim | Query Prefix | Doc Prefix |
|------|------|-----|-------------|-----------|
| `e5-base` (default) | `intfloat/e5-base-v2` | 768 | `query: ` | `passage: ` |
| `bge-large` | `BAAI/bge-large-en-v1.5` | 1024 | `Represent this sentence for searching relevant passages: ` | (empty) |

### Resolution Priority

1. `CQS_EMBEDDING_MODEL` env var (name or "custom")
2. `[embedding]` section in config file (`cqs.toml` or `.cqs/config.toml`)
3. Default: `e5-base`

For preset names, the built-in config is used. For `model = "custom"`, all fields must be specified in the config file.

### Config File Format

```toml
[embedding]
# Preset name OR "custom"
model = "bge-large"

# Only needed for model = "custom":
# repo = "my-org/my-model"
# onnx_path = "onnx/model.onnx"
# tokenizer = "tokenizer.json"
# dim = 768
# max_seq_length = 512
# query_prefix = "query: "
# doc_prefix = "passage: "
```

### Index Metadata

The model name is stored in index metadata (`model_name` key, already exists). On load:
- If configured model doesn't match indexed model → warn and suggest `cqs index --force`
- `cqs doctor` checks model consistency
- `cqs stats` shows the indexed model name

### Embedder Changes

- `Embedder::new()` accepts `ModelConfig` instead of using hardcoded constants
- `embed_query()` prepends `config.query_prefix` instead of hardcoded `"query: "`
- `embed_documents()` prepends `config.doc_prefix` instead of hardcoded `"passage: "`
- Runtime dimension detection (#682) validates against `config.dim`
- Model download uses `config.repo` + `config.onnx_path`

### Export Helper

New CLI command: `cqs export-model --repo <hf-repo> --output <dir>`

Shells out to Python (requires `conda activate cqs-train`):
1. Downloads PyTorch model via sentence-transformers
2. Exports to ONNX via optimum-cli
3. Writes `model.toml` with config fields pre-filled

This is a convenience tool, not required. Users can export manually.

### Enrichment Pipeline

The enrichment pipeline (NL generation, contrastive summaries, HyDE, call graph context) is model-agnostic. It produces text that gets embedded. Only the prefix changes per model. No enrichment code changes needed.

### What We're NOT Doing

- No PyTorch runtime fallback — ONNX only
- No mixed-model indexes — full reindex on model change
- No automatic ONNX export during `cqs index` — separate workflow
- No model fine-tuning integration — LoRA stays in research repo
- No model benchmarking in cqs — eval scripts stay in training repo

## File Changes

| File | Change |
|------|--------|
| New: `src/embedder/models.rs` | `ModelConfig` struct, built-in presets, resolution logic |
| `src/embedder/mod.rs` | Accept `ModelConfig`, use for prefix/dim/repo/paths |
| `src/config.rs` | Parse `[embedding]` section |
| `src/store/metadata.rs` | Store/validate model ID |
| `src/cli/definitions.rs` | `--model` flag |
| New: `src/cli/commands/export_model.rs` | Export helper |
| `src/cli/dispatch.rs` | Route export-model command |
| README.md, CONTRIBUTING.md, CHANGELOG.md | Documentation |

## Testing

- Unit: `ModelConfig` resolution (env > config > default)
- Unit: prefix application in embed_query/embed_documents
- Unit: dimension validation with non-768 model
- Integration: index with E5-base, switch config to BGE-large, verify mismatch warning
- Integration: reindex with BGE-large, verify search works
- Eval: BGE-large + enrichment R@1 (Rust hard eval — the key question)

## Success Criteria

- Users can switch models via env var or config file
- `cqs index --force` reindexes with the new model
- Mismatch detection prevents searching with wrong-model embeddings
- BGE-large works end-to-end (download, index, search)
- At least one non-default model tested on hard eval with enrichment
