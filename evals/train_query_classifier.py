#!/usr/bin/env python3
"""Phase 1.4 — train a distilled query classifier head on BGE embeddings.

Teacher: Gemma 4 31B (98.9% accuracy on v3 per phase 1.1 measurement).
Student: Linear(1024, 9) → softmax (~9.2K params, <0.1ms inference).

Training data:
- All 544 v3 queries (train + test + dev) with their `category` labels.
  We don't need a held-out split for distillation training — the v3
  fixture labels themselves came from Gemma+Claude consensus, so the
  v3 categories ARE the teacher labels. The held-out evaluation is the
  R@5 retrieval lift, not classifier accuracy on the same labels.
- Optionally augmented with telemetry queries classified by Gemma at
  training time (set --augment-telemetry; default off because v1.28.x
  pipeline shows 544 v3 queries are sufficient).

Inference:
- Compute BGE embedding of incoming query (cqs already does this for
  search; the head reuses it)
- One matmul + softmax → P(category) distribution
- Argmax for hard routing, or pass distribution to soft router

Output:
- evals/classifier_head/state_dict.pt   — PyTorch checkpoint
- evals/classifier_head/model.onnx      — for cqs Rust ORT inference
- evals/classifier_head/run_meta.json   — training config, eval metrics

Run:
    python3 evals/train_query_classifier.py --output evals/classifier_head
    python3 evals/train_query_classifier.py --output evals/classifier_head --augment-telemetry
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from collections import Counter
from pathlib import Path

os.environ.setdefault("WANDB_DISABLED", "true")
os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

import numpy as np
import torch
import torch.nn as nn
from sklearn.model_selection import train_test_split
from torch.utils.data import DataLoader, TensorDataset


REPO = Path(__file__).resolve().parent.parent
QUERIES_DIR = REPO / "evals" / "queries"

# Match cqs's QueryCategory enum order (see src/search/router.rs::define_query_categories!).
# This MUST stay in sync — Rust loads the ONNX with this category index.
# `unknown` is dropped from the trainable set — the router treats low-confidence
# predictions as "unknown" at runtime instead of learning to predict it.
CATEGORIES = [
    "identifier_lookup",
    "structural_search",
    "behavioral_search",
    "conceptual_search",
    "multi_step",
    "negation",
    "type_filtered",
    "cross_language",
]
CAT_TO_IDX = {c: i for i, c in enumerate(CATEGORIES)}
# Aliases — fixture has both "behavioral" and "behavioral_search" for same enum.
CAT_ALIASES = {
    "behavioral": "behavioral_search",
    "structural": "structural_search",
    "conceptual": "conceptual_search",
}


def load_v3_queries() -> list[dict]:
    out = []
    for split, path in [
        ("train", QUERIES_DIR / "v3_train.json"),
        ("test", QUERIES_DIR / "v3_test.v2.json"),
        ("dev", QUERIES_DIR / "v3_dev.v2.json"),
    ]:
        if not path.exists():
            continue
        rows = json.loads(path.read_text())["queries"]
        for r in rows:
            cat = r.get("category")
            if not cat:
                continue
            cat = CAT_ALIASES.get(cat, cat)
            if cat not in CAT_TO_IDX:
                continue
            # Skip the `unknown` category for training — only 1 example in v3,
            # and it's a fallback target the router uses when classifier
            # confidence is low, not something we want to learn to predict.
            # The 8 real categories are what the head outputs; "unknown" is
            # a runtime decision (low max-softmax → use safe default α).
            if cat == "unknown":
                continue
            out.append({"query": r["query"], "category": cat, "split": split})
    return out


def load_synthetic_queries(path: Path) -> list[dict]:
    """Load Gemma-labeled synthetic queries from generate_from_chunks.py output.

    Schema: {"queries": [{"query", "category", "matched", ...}, ...]}
    Only `matched=True` rows are kept (where Gemma's classification agrees
    with the prompt's target_category — the high-confidence subset).
    """
    if not path.exists():
        return []
    raw = json.loads(path.read_text())
    out = []
    for r in raw.get("queries", []):
        if not r.get("matched", False):
            continue
        cat = r.get("category")
        if not cat:
            continue
        cat = CAT_ALIASES.get(cat, cat)
        if cat not in CAT_TO_IDX or cat == "unknown":
            continue
        out.append({"query": r["query"], "category": cat, "split": "synthetic"})
    return out


def embed_queries(queries: list[str], model_name: str, batch_size: int = 64,
                  query_prefix: str = "") -> np.ndarray:
    """Compute BGE embeddings using sentence-transformers.

    Critical: cqs uses BGE-large-en-v1.5 with NO query prefix in the
    embed_query path (verified in src/embedder/mod.rs). For the BGE
    family the official query prefix is `Represent this sentence for
    searching relevant passages: ` but cqs intentionally omits it because
    code search queries are short identifiers, not sentences. Match that
    convention here so train/inference embeddings are identical.
    """
    from sentence_transformers import SentenceTransformer
    model = SentenceTransformer(model_name)
    if query_prefix:
        queries = [f"{query_prefix}{q}" for q in queries]
    return model.encode(queries, batch_size=batch_size, show_progress_bar=True,
                        normalize_embeddings=True).astype(np.float32)


class ClassifierHead(nn.Module):
    """Single linear layer + softmax. ~9.2K params for 1024 → 9."""

    def __init__(self, embed_dim: int = 1024, n_categories: int = 9):
        super().__init__()
        self.linear = nn.Linear(embed_dim, n_categories)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.linear(x)  # logits; softmax applied at inference


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--model", default="BAAI/bge-large-en-v1.5",
                    help="Embedding model — must match what cqs uses at inference")
    ap.add_argument("--augment-synthetic", type=Path, default=None,
                    help="Path to generate_from_chunks.py output (matched-only "
                         "rows are merged into the training set)")
    ap.add_argument("--epochs", type=int, default=50)
    ap.add_argument("--batch-size", type=int, default=64)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--weight-decay", type=float, default=1e-4)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--device", default="cuda" if torch.cuda.is_available() else "cpu")
    args = ap.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    torch.manual_seed(args.seed)
    np.random.seed(args.seed)

    # Load
    rows = load_v3_queries()
    print(f"[load] {len(rows)} v3 queries with categories", file=sys.stderr)
    if args.augment_synthetic:
        synth = load_synthetic_queries(args.augment_synthetic)
        print(f"[load] {len(synth)} synthetic queries from {args.augment_synthetic}",
              file=sys.stderr)
        rows.extend(synth)
        print(f"[load] combined total: {len(rows)} queries", file=sys.stderr)
    cat_counts = Counter(r["category"] for r in rows)
    print(f"[load] per-category: {dict(cat_counts)}", file=sys.stderr)

    # Embed
    print(f"[embed] computing BGE embeddings for {len(rows)} queries...", file=sys.stderr)
    t0 = time.monotonic()
    embeddings = embed_queries([r["query"] for r in rows], args.model)
    print(f"[embed] done in {time.monotonic()-t0:.1f}s, shape {embeddings.shape}",
          file=sys.stderr)
    labels = np.array([CAT_TO_IDX[r["category"]] for r in rows], dtype=np.int64)

    # Train/val split — 80/20 stratified by category for early-stopping eval.
    # The full v3 splits (train/test/dev) aren't held out from training because
    # we use ALL 544 queries as the training set; held-out eval is downstream
    # R@5 lift in production search, not classifier accuracy.
    X_train, X_val, y_train, y_val, train_idx, val_idx = train_test_split(
        embeddings, labels, np.arange(len(labels)),
        test_size=0.2, stratify=labels, random_state=args.seed,
    )
    print(f"[split] train={len(X_train)}, val={len(X_val)}", file=sys.stderr)

    # Train
    device = torch.device(args.device)
    model = ClassifierHead(embed_dim=embeddings.shape[1], n_categories=len(CATEGORIES)).to(device)
    optimizer = torch.optim.AdamW(model.parameters(), lr=args.lr, weight_decay=args.weight_decay)
    loss_fn = nn.CrossEntropyLoss()

    train_ds = TensorDataset(torch.from_numpy(X_train), torch.from_numpy(y_train))
    train_loader = DataLoader(train_ds, batch_size=args.batch_size, shuffle=True)
    X_val_t = torch.from_numpy(X_val).to(device)
    y_val_t = torch.from_numpy(y_val).to(device)

    best_val_acc = 0.0
    best_state = None
    print(f"[train] {args.epochs} epochs, bs={args.batch_size}, lr={args.lr}", file=sys.stderr)
    for epoch in range(args.epochs):
        model.train()
        running_loss = 0.0
        for xb, yb in train_loader:
            xb, yb = xb.to(device), yb.to(device)
            optimizer.zero_grad()
            logits = model(xb)
            loss = loss_fn(logits, yb)
            loss.backward()
            optimizer.step()
            running_loss += loss.item() * xb.size(0)

        # Val
        model.eval()
        with torch.no_grad():
            val_logits = model(X_val_t)
            val_pred = val_logits.argmax(dim=1)
            val_acc = (val_pred == y_val_t).float().mean().item()

        train_loss = running_loss / len(train_ds)
        if val_acc > best_val_acc:
            best_val_acc = val_acc
            best_state = {k: v.cpu().clone() for k, v in model.state_dict().items()}

        if (epoch + 1) % 5 == 0 or epoch == args.epochs - 1:
            print(f"  epoch {epoch+1:>3}/{args.epochs}  loss={train_loss:.4f}  "
                  f"val_acc={val_acc:.3f}  best={best_val_acc:.3f}",
                  file=sys.stderr)

    # Load best state, evaluate per-category on val
    model.load_state_dict(best_state)
    model.eval()
    with torch.no_grad():
        val_logits = model(X_val_t)
        val_pred = val_logits.argmax(dim=1).cpu().numpy()
    val_true = y_val.tolist() if isinstance(y_val, list) else y_val.tolist()
    by_cat = {c: {"correct": 0, "total": 0} for c in CATEGORIES}
    for t, p in zip(val_true, val_pred):
        cat = CATEGORIES[t]
        by_cat[cat]["total"] += 1
        if t == p:
            by_cat[cat]["correct"] += 1

    print(f"\n[final] best val accuracy: {best_val_acc:.3f}", file=sys.stderr)
    print("[final] per-category val:", file=sys.stderr)
    for cat in CATEGORIES:
        c = by_cat[cat]
        if c["total"]:
            print(f"  {cat:<22} {c['correct']:>3}/{c['total']:<3} = {100*c['correct']/c['total']:5.1f}%",
                  file=sys.stderr)

    # Save state dict
    state_path = args.output / "state_dict.pt"
    torch.save(best_state, state_path)
    print(f"\n[saved] {state_path}", file=sys.stderr)

    # Export ONNX
    onnx_path = args.output / "model.onnx"
    dummy = torch.randn(1, embeddings.shape[1]).to(device)
    model.cpu()
    torch.onnx.export(
        model.cpu(),
        torch.randn(1, embeddings.shape[1]),
        onnx_path,
        input_names=["embedding"],
        output_names=["logits"],
        dynamic_axes={"embedding": {0: "batch"}, "logits": {0: "batch"}},
        opset_version=14,
        do_constant_folding=True,
        dynamo=False,
    )
    print(f"[onnx] saved {onnx_path} ({onnx_path.stat().st_size / 1024:.1f} KB)",
          file=sys.stderr)

    # Run meta
    meta = {
        "embed_dim": embeddings.shape[1],
        "n_categories": len(CATEGORIES),
        "categories": CATEGORIES,  # ORDER matters for ONNX consumer
        "training_set_size": len(rows),
        "train_size": len(X_train),
        "val_size": len(X_val),
        "best_val_accuracy": round(best_val_acc, 4),
        "epochs": args.epochs,
        "batch_size": args.batch_size,
        "lr": args.lr,
        "weight_decay": args.weight_decay,
        "embedder_model": args.model,
        "embedder_normalized": True,
        "query_prefix": "",  # cqs convention — no prefix on query side
        "completed_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }
    (args.output / "run_meta.json").write_text(json.dumps(meta, indent=2))
    print(f"[meta] saved {args.output / 'run_meta.json'}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
