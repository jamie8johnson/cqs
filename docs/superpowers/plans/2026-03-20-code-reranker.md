# SQ-10: Code Reranker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fine-tune a code-specific cross-encoder reranker on CodeSearchNet data and integrate it into cqs with a configurable model path.

**Architecture:** Python training script (sentence-transformers CrossEncoder), ONNX export, HuggingFace upload, one-line Rust constant change. Plus a new reranker eval harness to measure before/after.

**Tech Stack:** Python (sentence-transformers, optimum), Rust (reranker.rs), ONNX Runtime

**Spec:** `docs/superpowers/specs/2026-03-20-code-reranker-design.md`

---

## File Map

| File | Responsibility |
|------|---------------|
| `~/training-data/train_reranker.py` | Training script: load data, strip prefixes, mine negatives, fine-tune, export ONNX |
| `src/reranker.rs` | Make MODEL_REPO configurable via CQS_RERANKER_MODEL env var |
| `tests/model_eval.rs` | Add reranker eval: embed → top-20 → rerank → measure R@1/MRR |

---

### Task 1: Write training script

**Files:**
- Create: `~/training-data/train_reranker.py`

- [ ] **Step 1: Write the script**

```python
#!/usr/bin/env python3
"""Fine-tune cross-encoder reranker on CodeSearchNet for code search."""

import argparse, json, random
from pathlib import Path
from sentence_transformers import CrossEncoder
from sentence_transformers.cross_encoder.trainer import CrossEncoderTrainer
from sentence_transformers.cross_encoder.training_args import CrossEncoderTrainingArguments
from datasets import Dataset as HFDataset

def load_pairs(csn_path, doc_path, max_samples, seed=42):
    """Load CodeSearchNet + docstring pairs, strip prefixes, add random negatives."""
    positives = []
    all_functions = {}  # language -> [functions]

    # Load CodeSearchNet
    with open(csn_path) as f:
        for line in f:
            d = json.loads(line)
            query = d["query"].removeprefix("query: ")
            positive = d["positive"].removeprefix("passage: ")
            lang = d.get("language", "unknown")
            if len(query) < 15 or len(positive) < 20:
                continue
            positives.append({"query": query, "positive": positive, "language": lang})
            all_functions.setdefault(lang, []).append(positive)

    # Load docstring pairs
    if doc_path and Path(doc_path).exists():
        with open(doc_path) as f:
            for line in f:
                d = json.loads(line)
                query = d["query"].removeprefix("query: ")
                positive = d["positive"].removeprefix("passage: ")
                lang = d.get("language", "unknown")
                if len(query) < 15 or len(positive) < 20:
                    continue
                positives.append({"query": query, "positive": positive, "language": lang})
                all_functions.setdefault(lang, []).append(positive)

    random.seed(seed)
    random.shuffle(positives)
    if max_samples > 0:
        positives = positives[:max_samples]

    # Build training records: positive pair (label=1) + random negative (label=0)
    records = []
    for p in positives:
        # Positive
        records.append({"query": p["query"], "passage": p["positive"][:2000], "label": 1.0})
        # Random same-language negative
        lang_funcs = all_functions.get(p["language"], [])
        if len(lang_funcs) > 1:
            neg = random.choice(lang_funcs)
            while neg == p["positive"] and len(lang_funcs) > 1:
                neg = random.choice(lang_funcs)
            records.append({"query": p["query"], "passage": neg[:2000], "label": 0.0})

    print(f"Loaded {len(records)} training records ({len(positives)} positive, {len(records)-len(positives)} negative)")
    return HFDataset.from_list(records)

def train(args):
    import torch
    print(f"GPU: {torch.cuda.get_device_name(0)}")

    # Load data
    dataset = load_pairs(args.data, args.doc_data, args.max_samples)
    split = dataset.train_test_split(test_size=0.05, seed=42)

    # Load cross-encoder
    model = CrossEncoder("cross-encoder/ms-marco-MiniLM-L-6-v2", num_labels=1, max_length=512)

    # Training args
    training_args = CrossEncoderTrainingArguments(
        output_dir=args.output,
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        per_device_eval_batch_size=args.batch_size,
        learning_rate=args.lr,
        warmup_ratio=0.1,
        fp16=True,
        eval_strategy="steps",
        eval_steps=500,
        save_steps=500,
        save_total_limit=2,
        logging_steps=100,
        report_to="none",
    )

    trainer = CrossEncoderTrainer(
        model=model,
        args=training_args,
        train_dataset=split["train"],
        eval_dataset=split["test"],
    )

    print("Training...")
    trainer.train()
    model.save_pretrained(str(Path(args.output) / "merged"))
    print(f"Model saved to {args.output}/merged")
    return Path(args.output) / "merged"

def export_onnx(merged_path, output_path):
    from optimum.onnxruntime import ORTModelForSequenceClassification
    print(f"Exporting ONNX from {merged_path}...")
    ort_model = ORTModelForSequenceClassification.from_pretrained(str(merged_path), export=True, opset=14)
    ort_model.save_pretrained(str(output_path))
    onnx_file = output_path / "model.onnx"
    if onnx_file.exists():
        print(f"ONNX size: {onnx_file.stat().st_size / 1024 / 1024:.1f} MB")

    # Verify output shape
    import onnxruntime as ort_rt
    sess = ort_rt.InferenceSession(str(onnx_file))
    outputs = sess.get_outputs()
    print(f"Output: {outputs[0].name} shape={outputs[0].shape}")
    assert outputs[0].name == "logits", f"Expected 'logits', got '{outputs[0].name}'"

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data", required=True, help="CodeSearchNet JSONL")
    parser.add_argument("--doc-data", default="", help="Docstring pairs JSONL (supplement)")
    parser.add_argument("--output", default="./code-reranker", help="Output dir")
    parser.add_argument("--epochs", type=int, default=3)
    parser.add_argument("--batch-size", type=int, default=32)
    parser.add_argument("--lr", type=float, default=2e-5)
    parser.add_argument("--max-samples", type=int, default=50000)
    parser.add_argument("--export-onnx", action="store_true")
    args = parser.parse_args()

    merged_path = train(args)
    if args.export_onnx:
        export_onnx(merged_path, Path(args.output) / "onnx")

if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Commit**

```
feat: train_reranker.py — fine-tune cross-encoder on CodeSearchNet
```

---

### Task 2: Train the model

- [ ] **Step 1: Run training**

```bash
cd ~/training-data && ~/miniforge3/envs/cqs-train/bin/python train_reranker.py \
  --data codesearchnet_pairs.jsonl \
  --doc-data docstring_pairs.jsonl \
  --output ./code-reranker \
  --epochs 3 \
  --max-samples 50000 \
  --export-onnx
```

Expected: ~30 min on A6000. ONNX exported to `~/training-data/code-reranker/onnx/`.

- [ ] **Step 2: Verify ONNX output**

```bash
ls ~/training-data/code-reranker/onnx/
# Should contain: model.onnx, tokenizer.json, config.json
```

Verify output shape is `logits [batch, 1]` (the script checks this).

- [ ] **Step 3: Upload to HuggingFace**

```bash
mkdir -p /tmp/reranker-upload/onnx
cp ~/training-data/code-reranker/onnx/model.onnx /tmp/reranker-upload/onnx/
cp ~/training-data/code-reranker/onnx/tokenizer.json /tmp/reranker-upload/
cp ~/training-data/code-reranker/onnx/config.json /tmp/reranker-upload/
hf upload jamie8johnson/code-reranker-v1 /tmp/reranker-upload --repo-type model
```

---

### Task 3: Make MODEL_REPO configurable in reranker.rs

**Files:**
- Modify: `src/reranker.rs`

- [ ] **Step 1: Add env var override**

Replace the constant usage with a function:

```rust
const DEFAULT_MODEL_REPO: &str = "cross-encoder/ms-marco-MiniLM-L-6-v2";

fn model_repo() -> String {
    std::env::var("CQS_RERANKER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_REPO.to_string())
}
```

Update `model_paths()` (or wherever `MODEL_REPO` is used to download the model) to call `model_repo()` instead of using the constant directly.

- [ ] **Step 2: Test**

```bash
CQS_RERANKER_MODEL=jamie8johnson/code-reranker-v1 cargo run --features gpu-index -- --rerank "parse config"
```

Verify it downloads and uses the custom model.

- [ ] **Step 3: Commit**

```
feat(reranker): configurable model via CQS_RERANKER_MODEL env var
```

---

### Task 4: Write reranker eval harness

**Files:**
- Modify: `tests/model_eval.rs`

- [ ] **Step 1: Add test_hard_reranker_comparison**

New test function that:
1. Parses hard eval fixtures (same as existing hard eval)
2. Embeds all chunks with E5-base-v2 (same as existing)
3. For each query, gets top-20 by cosine similarity
4. Loads the reranker (Reranker::new)
5. Rescores top-20 with the cross-encoder
6. Re-sorts by cross-encoder score
7. Measures R@1, MRR, NDCG@10 on the reranked list
8. Prints comparison: embedding-only vs reranked

Mark as `#[ignore]` (same as other model eval tests — slow, downloads models).

- [ ] **Step 2: Run baseline (web-trained reranker)**

```bash
cargo test --features gpu-index --test model_eval -- test_hard_reranker_comparison --ignored --nocapture
```

Record R@1, MRR, per-language MRR. This is the "before" number.

- [ ] **Step 3: Run with code-trained reranker**

```bash
CQS_RERANKER_MODEL=jamie8johnson/code-reranker-v1 \
cargo test --features gpu-index --test model_eval -- test_hard_reranker_comparison --ignored --nocapture
```

Compare against baseline. Record in research log.

- [ ] **Step 4: Commit**

```
test: add reranker eval harness for before/after comparison
```

---

### Task 5: Evaluate and decide

- [ ] **Step 1: Record results in research log**

```bash
cat >> ~/training-data/RESEARCH_LOG.md << 'EOF'
## Experiment 5: SQ-10 Code Reranker

**Config:** [fill in]
**Baseline (web-trained):** R@1=X%, MRR=X
**Code-trained:** R@1=X%, MRR=X
**Per-language:** [fill in]
**Decision:** [ship / iterate / abandon]
EOF
```

- [ ] **Step 2: If improved — update DEFAULT_MODEL_REPO**

Change the default in `reranker.rs` from `ms-marco-MiniLM-L-6-v2` to `jamie8johnson/code-reranker-v1`.

- [ ] **Step 3: If improved — consider making reranking default for --json**

Separate PR if decided.

---

## Task Parallelism

| Phase | Tasks | Notes |
|-------|-------|-------|
| 1 | 1 | Write training script |
| 2 | 2 | Train + export + upload (~30 min) |
| 3 (parallel) | 3, 4 | Rust integration + eval harness (independent files) |
| 4 | 5 | Evaluate and decide |
