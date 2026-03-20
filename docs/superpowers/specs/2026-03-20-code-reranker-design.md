# SQ-10: Fine-Tuned Code Reranker Design Spec

## Purpose

Replace the web-trained cross-encoder reranker (`ms-marco-MiniLM-L-6-v2`) with a code-trained one. Cross-encoders read (query, document) pairs side-by-side and can pick up discriminating details that single-vector embeddings miss — exactly what the hard eval's confusable function pairs need.

## Base Model

`cross-encoder/ms-marco-MiniLM-L-6-v2` (22M params). If results disappoint, try `ms-marco-MiniLM-L-12-v2` (33M params).

**Critical:** Must use `num_labels=1` (BCEWithLogitsLoss). Output shape `[batch, 1]` raw logits. The existing `src/reranker.rs` applies sigmoid externally and expects this exact shape. `num_labels=2` would produce `[batch, 2]` and silently break scoring.

## Training Data

**Primary:** 50k CodeSearchNet pairs (from `~/training-data/codesearchnet_pairs.jsonl`). 6 languages: Python, Java, Go, Ruby, JavaScript, PHP.

**Supplement:** 7.5k docstring pairs (from `~/training-data/docstring_pairs.jsonl`). Covers Rust, Scala, Kotlin, Swift — languages missing from CodeSearchNet.

**Total:** ~57.5k pairs. Scale to 200k-500k if pilot shows improvement.

**IMPORTANT:** Strip `"query: "` and `"passage: "` prefixes from training data during loading. These were added for the embedding training but don't exist at reranker inference time.

## Negative Mining

In-batch negatives are too easy for cross-encoders (they see both texts side-by-side — random functions are trivially distinguishable). Need hard negatives plus random for variety.

**Approach (V1 — random same-language):** For each positive (docstring, function) pair, sample 1-3 random functions from the same language as negatives. Cheap to compute, provides reasonable difficulty.

**Approach (V2 — if V1 insufficient):** For each positive pair, use E5-base-v2 bi-encoder to find top-10 most similar functions by cosine similarity (excluding the positive). Take top-3 as hard negatives. These are functions the embedding model thinks match but don't.

## Training Config

```python
CrossEncoder(
    "cross-encoder/ms-marco-MiniLM-L-6-v2",
    num_labels=1,
    max_length=512,
)
```

- Epochs: 3 (with early stopping on validation loss)
- Batch size: 32
- Learning rate: 2e-5
- Warmup: 10%
- Validation split: 5%
- Loss: BCEWithLogitsLoss (automatic with num_labels=1)
- Labels: 1.0 for positive pairs, 0.0 for negative pairs

## ONNX Export

```python
from optimum.onnxruntime import ORTModelForSequenceClassification

model = ORTModelForSequenceClassification.from_pretrained(
    "./code-reranker/merged",
    export=True,
    opset=14,  # matches existing model
)
model.save_pretrained("./code-reranker/onnx")
```

**Verify after export:**
- Input names: `input_ids`, `attention_mask`, `token_type_ids` (all i64)
- Output name: `logits` with shape `[batch, 1]` (f32)
- Tokenizer exported alongside (`tokenizer.json`)
- File layout matches what `reranker.rs` expects

## Integration

**Make MODEL_REPO configurable:**
```rust
// src/reranker.rs
const DEFAULT_MODEL_REPO: &str = "cross-encoder/ms-marco-MiniLM-L-6-v2";

fn model_repo() -> String {
    std::env::var("CQS_RERANKER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_REPO.to_string())
}
```

This allows A/B testing and rollback without recompiling. After eval confirms improvement, update the default.

**Upload:** Upload the ONNX directory contents so the repo root has `model.onnx`, `tokenizer.json`, and `config.json` directly (no `onnx/` prefix). Then update `MODEL_FILE` in `reranker.rs` from `"onnx/model.onnx"` to `"model.onnx"` and `TOKENIZER_FILE` from `"tokenizer.json"` to `"tokenizer.json"` (already correct) when using the custom repo. Alternatively, upload with the `onnx/` structure preserved to match the existing layout.

**Simplest approach:** Mirror the existing repo layout. Create `onnx/` subdirectory in the HuggingFace repo:
```bash
mkdir -p /tmp/reranker-upload/onnx
cp ./code-reranker/onnx/model.onnx /tmp/reranker-upload/onnx/
cp ./code-reranker/onnx/tokenizer.json /tmp/reranker-upload/
cp ./code-reranker/onnx/config.json /tmp/reranker-upload/
hf upload jamie8johnson/code-reranker-v1 /tmp/reranker-upload --repo-type model
```

**Expected repo file list:** `onnx/model.onnx`, `tokenizer.json`, `config.json` (matches existing ms-marco repo layout).

## Eval

**Must write reranker eval harness first** (doesn't exist). The current `model_eval.rs` tests embedding quality only — no reranking step.

**Reranker eval flow:**
1. Embed all fixture chunks with E5-base-v2 (existing)
2. For each query, get top-20 by cosine similarity (embedding recall)
3. Rescore top-20 with the cross-encoder (reranking)
4. Measure R@1, MRR, NDCG@10 on the reranked list

**Baseline:** Run with the web-trained model first.
**After:** Run with the code-trained model.
**Compare:** Per-language breakdown, focus on TypeScript (weakest at 0.758 MRR).

## Risk: Rust/TypeScript Gap

CodeSearchNet has no Rust or TypeScript. Our eval is primarily these languages. Mitigations:
- Docstring pairs supplement adds Rust coverage
- Cross-encoders generalize better than bi-encoders (they read actual code tokens, not compressed vectors)
- If the gap matters, supplement with Rust/TS pairs from our 19 training repos

## Not In Scope (V1)

- Hard negative mining via bi-encoder (use random same-language negatives for V1)
- Making reranking default for `--json` (wait for eval results)
- Multi-model reranker ensemble
- Distilling the reranker into the embedding model
