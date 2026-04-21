#!/usr/bin/env python3
"""Train the fused alpha + classifier head with contrastive ranking.

Architecture (per `docs/plans/2026-04-20-fused-alpha-classifier-head.md`):
    [query_emb | corpus_fingerprint] (2048 dims)
            ↓
    Linear(2048, 64) + ReLU + Dropout(0.1)
            ↓
       ┌─── Linear(64, 8)  → softmax → category
       └─── Linear(64, 1)  → sigmoid → alpha

Loss:
    L = L_cls + λ_α · L_α
    L_cls = CrossEntropy(category_logits, gemma_label)
    L_α   = ContrastiveRanking(alpha, sparse_scores, dense_scores, gold_idx)

Where the ranking loss is:
    combined_i(α) = α · sparse_scores[i] + (1−α) · dense_scores[i]
    L_α = −log( exp(combined_gold(α) / τ) /
                Σ_i exp(combined_i(α) / τ) )

Inputs: shards built by `evals/build_contrastive_shards.py`.
Outputs:
    evals/fused_head/state_dict.pt   PyTorch checkpoint
    evals/fused_head/model.onnx      For cqs Rust ORT inference
    evals/fused_head/run_meta.json   Training config + per-category metrics

Run:
    python3 evals/train_fused_head.py \\
        --shards evals/fused_head/contrastive_shards.npz \\
        --output evals/fused_head
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from collections import Counter
from pathlib import Path

import numpy as np

os.environ.setdefault("WANDB_DISABLED", "true")
os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.utils.data import DataLoader, Dataset

# Must match src/classifier_head.rs::HEAD_CATEGORIES order.
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


class ShardDataset(Dataset):
    """Reads the .npz shard produced by build_contrastive_shards.py."""

    def __init__(self, shards: dict, indices: np.ndarray, fingerprint: np.ndarray):
        self.q_emb = shards["query_emb"][indices].astype(np.float32)
        self.sparse = shards["sparse_scores"][indices].astype(np.float32)
        self.dense = shards["dense_scores"][indices].astype(np.float32)
        self.gold = shards["gold_idx"][indices].astype(np.int64)
        self.cat = shards["category"][indices].astype(np.int64)
        self.fingerprint = fingerprint.astype(np.float32)

    def __len__(self):
        return len(self.q_emb)

    def __getitem__(self, i):
        return {
            "q_emb": torch.from_numpy(self.q_emb[i]),
            "sparse": torch.from_numpy(self.sparse[i]),
            "dense": torch.from_numpy(self.dense[i]),
            "gold": int(self.gold[i]),
            "cat": int(self.cat[i]),
        }


class FusedHead(nn.Module):
    """Shared trunk + classifier head + alpha head."""

    def __init__(self, embed_dim: int = 1024, fingerprint_dim: int = 1024,
                 trunk_hidden: int = 64, n_categories: int = 8,
                 dropout: float = 0.1, fingerprint_dropout: float = 0.2):
        super().__init__()
        self.input_dim = embed_dim + fingerprint_dim
        self.fingerprint_dropout_p = fingerprint_dropout
        self.trunk = nn.Sequential(
            nn.Linear(self.input_dim, trunk_hidden),
            nn.ReLU(),
            nn.Dropout(dropout),
        )
        self.classifier = nn.Linear(trunk_hidden, n_categories)
        self.alpha_head = nn.Linear(trunk_hidden, 1)

    def forward(self, q_emb: torch.Tensor, fingerprint: torch.Tensor) -> tuple:
        # `fingerprint` is the same vector for every example in the batch
        # (single-corpus training); broadcast to (B, F).
        if fingerprint.dim() == 1:
            fp = fingerprint.unsqueeze(0).expand(q_emb.size(0), -1)
        else:
            fp = fingerprint
        # Optional dropout on the fingerprint channel to prevent the trunk
        # from absorbing the constant fingerprint into bias terms.
        if self.training and self.fingerprint_dropout_p > 0:
            mask = (torch.rand(fp.size(0), 1, device=fp.device)
                    > self.fingerprint_dropout_p).float()
            fp = fp * mask
        x = torch.cat([q_emb, fp], dim=-1)
        h = self.trunk(x)
        logits = self.classifier(h)
        alpha = torch.sigmoid(self.alpha_head(h)).squeeze(-1)
        return logits, alpha


def contrastive_ranking_loss(alpha: torch.Tensor, sparse: torch.Tensor,
                              dense: torch.Tensor, gold: torch.Tensor,
                              tau: float = 0.1, normalize: bool = True) -> torch.Tensor:
    """L_α per the spec.

    alpha:  [B] in [0, 1]
    sparse: [B, K] sparse scores per pool candidate
    dense:  [B, K] dense scores per pool candidate
    gold:   [B] gold candidate index (typically 0 by shard layout)
    tau:    softmax temperature
    normalize: if True, z-score each component within each query's pool
        before blending. Without this the unbounded SPLADE scores
        dominate the dense [-1, 1] range, and α collapses to 0.
    """
    if normalize:
        s_mean = sparse.mean(dim=-1, keepdim=True)
        s_std = sparse.std(dim=-1, keepdim=True).clamp_min(1e-6)
        d_mean = dense.mean(dim=-1, keepdim=True)
        d_std = dense.std(dim=-1, keepdim=True).clamp_min(1e-6)
        sparse = (sparse - s_mean) / s_std
        dense = (dense - d_mean) / d_std
    a = alpha.unsqueeze(-1)  # [B, 1]
    combined = a * sparse + (1.0 - a) * dense  # [B, K]
    log_probs = F.log_softmax(combined / tau, dim=-1)
    return -log_probs.gather(1, gold.unsqueeze(-1)).squeeze(-1).mean()


def evaluate(model: FusedHead, loader: DataLoader, fingerprint: torch.Tensor,
             device: torch.device, tau: float) -> dict:
    model.eval()
    total = 0
    correct = 0
    cat_correct = Counter()
    cat_total = Counter()
    rank_metrics = {"r1_alpha_pred": 0, "r1_alpha_grid": 0}
    alphas_collected = []
    with torch.no_grad():
        for batch in loader:
            q_emb = batch["q_emb"].to(device)
            sparse = batch["sparse"].to(device)
            dense = batch["dense"].to(device)
            gold = batch["gold"].to(device)
            cat = batch["cat"].to(device)
            logits, alpha = model(q_emb, fingerprint)
            pred = logits.argmax(dim=-1)
            for c, p in zip(cat.cpu().numpy(), pred.cpu().numpy()):
                cat_total[int(c)] += 1
                if int(c) == int(p):
                    cat_correct[int(c)] += 1
                    correct += 1
                total += 1
            # Ranking metrics: use the same per-query z-score normalization
            # as the loss so the metric tracks what training optimizes.
            s_mean = sparse.mean(dim=-1, keepdim=True)
            s_std = sparse.std(dim=-1, keepdim=True).clamp_min(1e-6)
            d_mean = dense.mean(dim=-1, keepdim=True)
            d_std = dense.std(dim=-1, keepdim=True).clamp_min(1e-6)
            n_sparse = (sparse - s_mean) / s_std
            n_dense = (dense - d_mean) / d_std
            a = alpha.unsqueeze(-1)
            combined_pred = a * n_sparse + (1.0 - a) * n_dense
            r1 = (combined_pred.argmax(dim=-1) == gold).sum().item()
            rank_metrics["r1_alpha_pred"] += r1
            # Ceiling: best per-query alpha from a grid search (oracle).
            grid = torch.linspace(0.0, 1.0, steps=11, device=device).view(1, -1, 1)
            combined_grid = grid * n_sparse.unsqueeze(1) + (1 - grid) * n_dense.unsqueeze(1)
            grid_argmax = combined_grid.argmax(dim=-1)  # [B, 11]
            grid_hits = (grid_argmax == gold.unsqueeze(-1)).any(dim=-1).sum().item()
            rank_metrics["r1_alpha_grid"] += grid_hits
            alphas_collected.append(alpha.cpu().numpy())
    return {
        "cls_acc": correct / max(total, 1),
        "per_cat_acc": {CATEGORIES[c]: cat_correct[c] / cat_total[c]
                        for c in cat_total},
        "r1_alpha_pred": rank_metrics["r1_alpha_pred"] / max(total, 1),
        "r1_alpha_grid": rank_metrics["r1_alpha_grid"] / max(total, 1),
        "alpha_mean": float(np.concatenate(alphas_collected).mean()) if alphas_collected else 0.0,
        "alpha_std": float(np.concatenate(alphas_collected).std()) if alphas_collected else 0.0,
        "n": total,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shards", required=True, type=Path)
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--epochs", type=int, default=100)
    ap.add_argument("--batch-size", type=int, default=64)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--weight-decay", type=float, default=1e-4)
    ap.add_argument("--trunk-hidden", type=int, default=64)
    ap.add_argument("--dropout", type=float, default=0.1)
    ap.add_argument("--fingerprint-dropout", type=float, default=0.2)
    ap.add_argument("--lambda-alpha", type=float, default=1.0)
    ap.add_argument("--tau", type=float, default=0.1)
    ap.add_argument("--val-frac", type=float, default=0.2)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--device", default="cuda" if torch.cuda.is_available() else "cpu")
    args = ap.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    torch.manual_seed(args.seed)
    np.random.seed(args.seed)

    print(f"[load] {args.shards}", file=sys.stderr)
    shards = np.load(args.shards, allow_pickle=True)
    n = len(shards["query_emb"])
    fingerprint = shards["fingerprint"]
    cats = shards["category"]
    print(f"[load] {n} examples", file=sys.stderr)
    print(f"[load] per-category: {dict(Counter(cats.tolist()))}", file=sys.stderr)
    print(f"[load] fingerprint dim={fingerprint.shape[0]}, "
          f"||v||={np.linalg.norm(fingerprint):.4f}", file=sys.stderr)

    # Stratified train/val split by category.
    rng = np.random.default_rng(args.seed)
    train_idx = []
    val_idx = []
    for c in np.unique(cats):
        idx = np.where(cats == c)[0]
        rng.shuffle(idx)
        n_val = max(1, int(args.val_frac * len(idx)))
        val_idx.extend(idx[:n_val].tolist())
        train_idx.extend(idx[n_val:].tolist())
    train_idx = np.array(train_idx)
    val_idx = np.array(val_idx)
    print(f"[split] train={len(train_idx)}, val={len(val_idx)}", file=sys.stderr)

    train_ds = ShardDataset(shards, train_idx, fingerprint)
    val_ds = ShardDataset(shards, val_idx, fingerprint)
    train_loader = DataLoader(train_ds, batch_size=args.batch_size, shuffle=True)
    val_loader = DataLoader(val_ds, batch_size=args.batch_size, shuffle=False)

    device = torch.device(args.device)
    fingerprint_t = torch.from_numpy(fingerprint).to(device)
    model = FusedHead(
        embed_dim=shards["query_emb"].shape[1],
        fingerprint_dim=fingerprint.shape[0],
        trunk_hidden=args.trunk_hidden,
        n_categories=len(CATEGORIES),
        dropout=args.dropout,
        fingerprint_dropout=args.fingerprint_dropout,
    ).to(device)
    n_params = sum(p.numel() for p in model.parameters())
    print(f"[model] FusedHead params={n_params:,}", file=sys.stderr)

    optimizer = torch.optim.AdamW(model.parameters(), lr=args.lr,
                                  weight_decay=args.weight_decay)
    cls_loss_fn = nn.CrossEntropyLoss()

    best_score = -1.0
    best_state = None
    print(f"[train] {args.epochs} epochs, bs={args.batch_size}, lr={args.lr}, "
          f"τ={args.tau}, λ_α={args.lambda_alpha}", file=sys.stderr)
    for epoch in range(args.epochs):
        model.train()
        run_loss_cls = 0.0
        run_loss_alpha = 0.0
        n_batches = 0
        for batch in train_loader:
            q_emb = batch["q_emb"].to(device)
            sparse = batch["sparse"].to(device)
            dense = batch["dense"].to(device)
            gold = batch["gold"].to(device)
            cat = batch["cat"].to(device)
            logits, alpha = model(q_emb, fingerprint_t)
            l_cls = cls_loss_fn(logits, cat)
            l_alpha = contrastive_ranking_loss(alpha, sparse, dense, gold,
                                               tau=args.tau)
            loss = l_cls + args.lambda_alpha * l_alpha
            optimizer.zero_grad()
            loss.backward()
            optimizer.step()
            run_loss_cls += l_cls.item()
            run_loss_alpha += l_alpha.item()
            n_batches += 1

        # Composite val score: classification accuracy + ranking R@1.
        m = evaluate(model, val_loader, fingerprint_t, device, args.tau)
        score = 0.5 * m["cls_acc"] + 0.5 * m["r1_alpha_pred"]
        if score > best_score:
            best_score = score
            best_state = {k: v.cpu().clone() for k, v in model.state_dict().items()}
        if (epoch + 1) % 5 == 0 or epoch == args.epochs - 1:
            print(f"  epoch {epoch+1:>3}/{args.epochs}  "
                  f"L_cls={run_loss_cls/n_batches:.4f} "
                  f"L_α={run_loss_alpha/n_batches:.4f}  "
                  f"val cls_acc={m['cls_acc']:.3f} "
                  f"r1_alpha={m['r1_alpha_pred']:.3f} "
                  f"r1_grid={m['r1_alpha_grid']:.3f} "
                  f"α={m['alpha_mean']:.3f}±{m['alpha_std']:.3f}  "
                  f"best={best_score:.3f}", file=sys.stderr)

    # Restore best, final eval.
    assert best_state is not None
    model.load_state_dict(best_state)
    final = evaluate(model, val_loader, fingerprint_t, device, args.tau)
    print(f"\n[final] best composite score: {best_score:.4f}", file=sys.stderr)
    print(f"[final] val cls_acc={final['cls_acc']:.4f} "
          f"r1_alpha_pred={final['r1_alpha_pred']:.4f} "
          f"r1_alpha_grid_oracle={final['r1_alpha_grid']:.4f}", file=sys.stderr)
    print("[final] per-category cls accuracy:", file=sys.stderr)
    for c, acc in sorted(final["per_cat_acc"].items()):
        print(f"  {c:<22} {acc:5.3f}", file=sys.stderr)
    print(f"[final] alpha distribution: μ={final['alpha_mean']:.3f} "
          f"σ={final['alpha_std']:.3f}", file=sys.stderr)

    # Save state dict.
    state_path = args.output / "state_dict.pt"
    torch.save(best_state, state_path)
    print(f"\n[saved] {state_path}", file=sys.stderr)

    # Export ONNX.
    onnx_path = args.output / "model.onnx"
    model.cpu()
    dummy_q = torch.randn(1, shards["query_emb"].shape[1])
    dummy_fp = torch.randn(1, fingerprint.shape[0])

    class ExportWrapper(nn.Module):
        """Wraps FusedHead so the ONNX graph has fixed-shape inputs (Rust
        callers pass [1, query_dim] and [1, fingerprint_dim] always)."""

        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, q, fp):
            logits, alpha = self.m(q, fp)
            return logits, alpha

    wrapper = ExportWrapper(model).eval()
    torch.onnx.export(
        wrapper,
        (dummy_q, dummy_fp),
        onnx_path,
        input_names=["query_embedding", "corpus_fingerprint"],
        output_names=["category_logits", "alpha"],
        dynamic_axes={
            "query_embedding": {0: "batch"},
            "corpus_fingerprint": {0: "batch"},
            "category_logits": {0: "batch"},
            "alpha": {0: "batch"},
        },
        opset_version=14,
        do_constant_folding=True,
        dynamo=False,
    )
    print(f"[onnx] saved {onnx_path} "
          f"({onnx_path.stat().st_size / 1024:.1f} KB)", file=sys.stderr)

    # Run meta.
    meta = {
        "embed_dim": int(shards["query_emb"].shape[1]),
        "fingerprint_dim": int(fingerprint.shape[0]),
        "trunk_hidden": args.trunk_hidden,
        "n_categories": len(CATEGORIES),
        "categories": CATEGORIES,
        "n_train": int(len(train_idx)),
        "n_val": int(len(val_idx)),
        "best_composite_score": round(best_score, 4),
        "final_cls_acc": round(final["cls_acc"], 4),
        "final_r1_alpha_pred": round(final["r1_alpha_pred"], 4),
        "final_r1_alpha_grid": round(final["r1_alpha_grid"], 4),
        "alpha_mean": round(final["alpha_mean"], 4),
        "alpha_std": round(final["alpha_std"], 4),
        "per_category_cls_acc": final["per_cat_acc"],
        "epochs": args.epochs,
        "batch_size": args.batch_size,
        "lr": args.lr,
        "weight_decay": args.weight_decay,
        "dropout": args.dropout,
        "fingerprint_dropout": args.fingerprint_dropout,
        "tau": args.tau,
        "lambda_alpha": args.lambda_alpha,
        "completed_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }
    (args.output / "run_meta.json").write_text(json.dumps(meta, indent=2))
    print(f"[meta] saved {args.output / 'run_meta.json'}", file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())
