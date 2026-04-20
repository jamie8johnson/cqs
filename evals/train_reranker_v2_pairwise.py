#!/usr/bin/env python3
"""Pairwise margin training for the cqs reranker.

Pointwise BCE on graded labels (Phase 4) couldn't break out of stage-1's
ranking — R@20 unchanged on both splits, but R@5 −5 to −9pp. Diagnosis:
the cross-encoder learned weak absolute scores; ranking signal needs to
come from contrastive pairs, not isolated graded labels.

This script:
  1. Loads the same `reranker_v3_train.graded.jsonl` (label ∈ {0, 0.5, 1}).
  2. Groups rows by `query_idx` so we only build pairs within a query.
  3. For each query, emits pairs (winner, loser) where label[winner] >
     label[loser]. Includes 1.0-vs-0.5, 1.0-vs-0.0, 0.5-vs-0.0.
  4. Trains a UniXcoder cross-encoder with MarginRankingLoss on the
     difference of logits: score(q, winner) > score(q, loser) + margin.

Output is HF-compatible (config.json + model.safetensors + tokenizer
files) so the same `optimum-cli export onnx` step works after.

Usage:
  python3 evals/train_reranker_v2_pairwise.py \\
      --pointwise evals/reranker_v3/reranker_v3_train.graded.jsonl \\
      --output ~/training-data/reranker-v2-cqs-pairwise \\
      --epochs 5 --batch-size 16 --margin 0.3
"""

from __future__ import annotations

import argparse
import json
import os
import random
import sys
import time
from collections import defaultdict
from pathlib import Path

os.environ.setdefault("WANDB_DISABLED", "true")
os.environ.setdefault("WANDB_MODE", "disabled")
os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

import torch
import torch.nn as nn
from torch.utils.data import DataLoader, Dataset
from transformers import (
    AutoModelForSequenceClassification,
    AutoTokenizer,
    get_linear_schedule_with_warmup,
)


def parse_args():
    p = argparse.ArgumentParser()
    p.add_argument("--pointwise", required=True, type=Path)
    p.add_argument("--output", required=True, type=Path)
    p.add_argument("--base", default="microsoft/unixcoder-base")
    p.add_argument("--batch-size", type=int, default=16,
                   help="Pairs per batch — half the pointwise default since each pair runs the model twice.")
    p.add_argument("--lr", type=float, default=2e-5)
    p.add_argument("--epochs", type=int, default=5)
    p.add_argument("--margin", type=float, default=0.3,
                   help="Margin in MarginRankingLoss: enforces score(winner) > score(loser) + margin.")
    p.add_argument("--max-pairs-per-query", type=int, default=20,
                   help="Cap pairs per query so a few high-positive queries don't dominate.")
    p.add_argument("--max-seq-length", type=int, default=512)
    p.add_argument("--warmup-ratio", type=float, default=0.1)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--device", default="cuda" if torch.cuda.is_available() else "cpu")
    p.add_argument("--fp16", action="store_true", default=True)
    return p.parse_args()


def emit_event(path: Path, kind: str, **fields):
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "ts_unix": time.time(), "kind": kind, **fields}
    with path.open("a") as f:
        f.write(json.dumps(rec, default=str) + "\n")
        f.flush()
        os.fsync(f.fileno())


def load_grouped(pointwise: Path) -> dict[int, list[dict]]:
    """Group pointwise rows by query_idx → [{label, content, ...}]."""
    grouped: dict[int, list[dict]] = defaultdict(list)
    with pointwise.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "query" not in r or "content" not in r or "label" not in r:
                continue
            grouped[r["query_idx"]].append(r)
    return grouped


def build_pairs(grouped: dict[int, list[dict]], rng: random.Random,
                cap: int) -> list[tuple[str, str, str]]:
    """For each query, pair every higher-label row with every lower-label row.

    Cap pairs per query so queries with many positives don't dominate.
    Each tuple is (query_text, winner_content, loser_content).
    """
    pairs = []
    skipped_no_pair = 0
    for q_idx, rows in grouped.items():
        if not rows:
            continue
        query = rows[0]["query"]
        # Bucket by label
        buckets: dict[float, list[dict]] = defaultdict(list)
        for r in rows:
            buckets[float(r["label"])].append(r)
        labels_sorted = sorted(buckets.keys(), reverse=True)
        # Pair every (high, low) where high > low
        per_query = []
        for i, high in enumerate(labels_sorted):
            for low in labels_sorted[i + 1 :]:
                for w in buckets[high]:
                    for l in buckets[low]:
                        per_query.append((query, w["content"], l["content"]))
        if not per_query:
            skipped_no_pair += 1
            continue
        rng.shuffle(per_query)
        pairs.extend(per_query[:cap])
    return pairs, skipped_no_pair


class PairDataset(Dataset):
    def __init__(self, pairs: list[tuple[str, str, str]], tokenizer, max_len: int):
        self.pairs = pairs
        self.tok = tokenizer
        self.max_len = max_len

    def __len__(self):
        return len(self.pairs)

    def __getitem__(self, idx):
        q, win, lose = self.pairs[idx]
        return q, win, lose


def collate(batch, tokenizer, max_len):
    qs = [b[0] for b in batch]
    wins = [b[1] for b in batch]
    loses = [b[2] for b in batch]
    win_enc = tokenizer(qs, wins, padding=True, truncation=True,
                        max_length=max_len, return_tensors="pt")
    lose_enc = tokenizer(qs, loses, padding=True, truncation=True,
                         max_length=max_len, return_tensors="pt")
    return win_enc, lose_enc


def main():
    args = parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    events_path = args.output / "events.jsonl"
    emit_event(events_path, "start", argv=sys.argv, args={k: str(v) for k, v in vars(args).items()})

    rng = random.Random(args.seed)
    torch.manual_seed(args.seed)
    if args.device.startswith("cuda"):
        torch.cuda.manual_seed_all(args.seed)

    print(f"[load] {args.pointwise}", file=sys.stderr)
    grouped = load_grouped(args.pointwise)
    print(f"[load] {sum(len(v) for v in grouped.values())} rows across {len(grouped)} queries",
          file=sys.stderr)

    pairs, no_pair = build_pairs(grouped, rng, cap=args.max_pairs_per_query)
    print(f"[pair] {len(pairs)} pairs (skipped {no_pair} queries with single label class)",
          file=sys.stderr)
    emit_event(events_path, "pairs_built", n_pairs=len(pairs), n_queries=len(grouped),
               skipped_single_label=no_pair)
    if not pairs:
        print("[pair] no pairs — aborting", file=sys.stderr)
        return 2

    print(f"[model] loading {args.base}", file=sys.stderr)
    tokenizer = AutoTokenizer.from_pretrained(args.base)
    model = AutoModelForSequenceClassification.from_pretrained(args.base, num_labels=1)
    model.to(args.device)
    emit_event(events_path, "model_loaded", base=args.base, device=args.device)

    ds = PairDataset(pairs, tokenizer, args.max_seq_length)
    loader = DataLoader(
        ds, batch_size=args.batch_size, shuffle=True,
        collate_fn=lambda b: collate(b, tokenizer, args.max_seq_length),
        num_workers=2,
    )

    optimizer = torch.optim.AdamW(model.parameters(), lr=args.lr)
    total_steps = len(loader) * args.epochs
    warmup_steps = max(1, int(total_steps * args.warmup_ratio))
    scheduler = get_linear_schedule_with_warmup(
        optimizer, num_warmup_steps=warmup_steps, num_training_steps=total_steps
    )
    loss_fn = nn.MarginRankingLoss(margin=args.margin)
    scaler = torch.amp.GradScaler("cuda") if args.fp16 and args.device.startswith("cuda") else None

    print(f"[train] pairs={len(pairs)} bs={args.batch_size} lr={args.lr} "
          f"epochs={args.epochs} margin={args.margin} fp16={args.fp16} "
          f"steps/epoch={len(loader)} warmup={warmup_steps}", file=sys.stderr)

    train_t0 = time.monotonic()
    global_step = 0
    for epoch in range(args.epochs):
        epoch_t0 = time.monotonic()
        emit_event(events_path, "epoch_start", epoch=epoch + 1, target=args.epochs)
        model.train()
        running_loss = 0.0
        running_correct = 0
        running_n = 0

        for step, (win_enc, lose_enc) in enumerate(loader):
            win_enc = {k: v.to(args.device) for k, v in win_enc.items()}
            lose_enc = {k: v.to(args.device) for k, v in lose_enc.items()}
            optimizer.zero_grad()

            ctx = (
                torch.amp.autocast("cuda", dtype=torch.float16)
                if scaler is not None else
                torch.amp.autocast(args.device.split(":")[0], enabled=False)
            )
            with ctx:
                win_out = model(**win_enc).logits.squeeze(-1)
                lose_out = model(**lose_enc).logits.squeeze(-1)
                target = torch.ones_like(win_out)
                loss = loss_fn(win_out, lose_out, target)

            if scaler is not None:
                scaler.scale(loss).backward()
                scaler.step(optimizer)
                scaler.update()
            else:
                loss.backward()
                optimizer.step()
            scheduler.step()

            running_loss += loss.item()
            running_correct += (win_out > lose_out).sum().item()
            running_n += win_out.numel()
            global_step += 1

            if global_step % 50 == 0:
                avg_loss = running_loss / 50
                pair_acc = running_correct / max(1, running_n)
                print(f"  [ep{epoch+1} step {step+1}/{len(loader)}] loss={avg_loss:.4f} "
                      f"pair_acc={pair_acc:.3f} lr={scheduler.get_last_lr()[0]:.2e}",
                      file=sys.stderr, flush=True)
                running_loss = 0.0
                running_correct = 0
                running_n = 0

        epoch_secs = time.monotonic() - epoch_t0
        emit_event(events_path, "epoch_done", epoch=epoch + 1, target=args.epochs,
                   secs=round(epoch_secs, 1))
        print(f"[epoch] {epoch+1}/{args.epochs} done in {epoch_secs:.1f}s", file=sys.stderr)

        # Save checkpoint after each epoch
        model.save_pretrained(args.output)
        tokenizer.save_pretrained(args.output)
        meta = {
            "base": args.base,
            "loss": "MarginRankingLoss",
            "margin": args.margin,
            "n_pairs": len(pairs),
            "n_queries": len(grouped),
            "batch_size": args.batch_size,
            "lr": args.lr,
            "epochs_target": args.epochs,
            "epochs_done": epoch + 1,
            "max_seq_length": args.max_seq_length,
            "max_pairs_per_query": args.max_pairs_per_query,
            "fp16": args.fp16,
            "seed": args.seed,
            "device": args.device,
            "pointwise_source": str(args.pointwise),
            "last_epoch_secs": round(epoch_secs, 1),
            "completed_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        }
        (args.output / "run_meta.json").write_text(json.dumps(meta, indent=2))

    wall = time.monotonic() - train_t0
    emit_event(events_path, "training_done", wall_secs=round(wall, 1), interrupted=False)
    print(f"[train] done in {wall:.1f}s ({wall/60:.1f}m)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
