#!/usr/bin/env python3
"""Fine-tune a code-specific cross-encoder reranker on v3 pool triples.

Base: cross-encoder/ms-marco-MiniLM-L-6-v2 (22M params, ~1ms inference/pair).
Fine-tunes with binary cross-entropy on (query, chunk, label) triples from
build_reranker_train.py.

After training: exports to ONNX and writes the bundle to
~/.local/share/cqs/reranker-v2/{onnx/model.onnx, tokenizer.json, config files}.
cqs's reranker loads from this path when CQS_RERANKER_MODEL points here.

Usage:
    conda activate cqs-train
    python3 evals/train_reranker.py
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import time
from pathlib import Path

import torch
from sentence_transformers import CrossEncoder, InputExample
from sentence_transformers.cross_encoder.evaluation import CEBinaryClassificationEvaluator
from torch.utils.data import DataLoader

BASE_MODEL = os.environ.get("RERANKER_BASE", "cross-encoder/ms-marco-MiniLM-L-6-v2")
DATA_DIR = Path(__file__).parent
TRAIN_PATH = DATA_DIR / "reranker_v2_train.jsonl"
DEV_PATH = DATA_DIR / "reranker_v2_dev.jsonl"

OUT_DIR = Path("~/.local/share/cqs/reranker-v2").expanduser()
OUT_ONNX_DIR = OUT_DIR / "onnx"

EPOCHS = int(os.environ.get("RERANKER_EPOCHS", "3"))
BATCH_SIZE = int(os.environ.get("RERANKER_BATCH", "32"))
LR = float(os.environ.get("RERANKER_LR", "2e-5"))
WARMUP_FRAC = 0.1
MAX_LEN = 512  # MiniLM handles up to 512 tokens


def load_triples(path: Path) -> list[InputExample]:
    examples = []
    with path.open() as f:
        for line in f:
            r = json.loads(line)
            # Truncate chunks to avoid tokenizer warnings — MiniLM's 512-token
            # cap means long chunks get clipped anyway.
            content = r["content"][:3000]
            examples.append(InputExample(texts=[r["query"], content], label=float(r["label"])))
    return examples


def main() -> int:
    print(f"base model: {BASE_MODEL}")
    print(f"epochs={EPOCHS} batch={BATCH_SIZE} lr={LR}")

    train_examples = load_triples(TRAIN_PATH)
    dev_examples = load_triples(DEV_PATH)
    print(f"train examples: {len(train_examples)}")
    print(f"dev examples  : {len(dev_examples)}")

    device = "cuda" if torch.cuda.is_available() else "cpu"
    print(f"device: {device}")

    # num_labels=1 = binary regression (0/1 scores). Default tokenizer max_length
    # is model-specific; explicit max_length prevents unexpected truncation.
    model = CrossEncoder(BASE_MODEL, num_labels=1, max_length=MAX_LEN, device=device)

    train_loader = DataLoader(train_examples, shuffle=True, batch_size=BATCH_SIZE)

    # Binary classification evaluator — uses sklearn's average_precision_score
    # as the primary metric. For reranking, AP on hold-out is a reasonable proxy
    # for "does the trained model rank positives above negatives?"
    evaluator = CEBinaryClassificationEvaluator.from_input_examples(
        dev_examples, name="v3_dev"
    )

    n_steps = len(train_loader) * EPOCHS
    warmup_steps = int(n_steps * WARMUP_FRAC)
    print(f"n_steps={n_steps} warmup={warmup_steps}")

    t0 = time.monotonic()
    checkpoint_dir = Path(__file__).parent / "reranker_v2_checkpoint"
    if checkpoint_dir.exists():
        shutil.rmtree(checkpoint_dir)

    model.fit(
        train_dataloader=train_loader,
        evaluator=evaluator,
        epochs=EPOCHS,
        optimizer_params={"lr": LR},
        warmup_steps=warmup_steps,
        output_path=str(checkpoint_dir),
        save_best_model=True,
        evaluation_steps=max(1, len(train_loader) // 2),
    )
    print(f"training wall: {time.monotonic()-t0:.1f}s")

    # Reload best checkpoint (save_best_model=True wrote the best epoch there).
    model = CrossEncoder(str(checkpoint_dir), num_labels=1, max_length=MAX_LEN, device=device)

    # Sanity check: measure accuracy on dev.
    print("\n=== dev sanity check ===")
    scores = []
    labels = []
    for ex in dev_examples:
        s = model.predict(ex.texts, show_progress_bar=False)
        scores.append(float(s))
        labels.append(int(ex.label))
    from sklearn.metrics import average_precision_score, roc_auc_score
    ap = average_precision_score(labels, scores)
    auc = roc_auc_score(labels, scores)
    print(f"dev AP: {ap:.4f}")
    print(f"dev AUC: {auc:.4f}")

    # Export to ONNX via torch.onnx (optimum has a transformers-version
    # incompatibility we'd rather not fight).
    print(f"\nexporting to ONNX at {OUT_ONNX_DIR}")
    OUT_ONNX_DIR.mkdir(parents=True, exist_ok=True)

    from transformers import AutoModelForSequenceClassification, AutoTokenizer
    hf_model = AutoModelForSequenceClassification.from_pretrained(str(checkpoint_dir))
    hf_model.eval()
    tok = AutoTokenizer.from_pretrained(str(checkpoint_dir))

    # Multi-row dummy so ONNX dynamic-axis propagation sees a non-trivial
    # batch dim — otherwise internal buffers can bake in batch=1 and fail
    # at inference when cqs passes a batch of 12 candidates.
    dummy = tok(
        ["query a", "query b"], ["passage one", "passage two"],
        return_tensors="pt", padding="max_length", truncation=True, max_length=MAX_LEN,
    )

    # Single-file ONNX: use_external_data=False keeps the ~90 MB MiniLM
    # weights inside model.onnx. cqs's reranker loader expects one file.
    export_path = OUT_ONNX_DIR / "model.onnx"
    # Clean any stale external-data files from a prior failed export.
    for stale in OUT_ONNX_DIR.glob("model.onnx.data"):
        stale.unlink()
    torch.onnx.export(
        hf_model,
        (dummy["input_ids"], dummy["attention_mask"], dummy.get("token_type_ids")),
        str(export_path),
        input_names=["input_ids", "attention_mask", "token_type_ids"],
        output_names=["logits"],
        dynamic_axes={
            "input_ids": {0: "batch", 1: "seq"},
            "attention_mask": {0: "batch", 1: "seq"},
            "token_type_ids": {0: "batch", 1: "seq"},
            "logits": {0: "batch"},
        },
        opset_version=17,
        do_constant_folding=True,
        export_params=True,
    )
    tok.save_pretrained(str(OUT_DIR))

    # Copy config files too so HF libs can re-load if needed.
    for f in ["config.json", "special_tokens_map.json", "tokenizer_config.json"]:
        src = Path(checkpoint_dir) / f
        if src.exists():
            shutil.copy2(src, OUT_DIR / f)

    onnx_size_mb = (OUT_ONNX_DIR / "model.onnx").stat().st_size / 2**20
    print(f"\nexport complete:")
    print(f"  {OUT_DIR}")
    print(f"    onnx/model.onnx  ({onnx_size_mb:.1f} MB)")
    print(f"    tokenizer.json + config files")
    print(f"  dev AP: {ap:.4f}")
    print(f"  dev AUC: {auc:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
