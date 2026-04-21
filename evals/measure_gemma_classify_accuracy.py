#!/usr/bin/env python3
"""Phase 1.1 — measure Gemma 4 31B classifier accuracy on v3 ground truth.

Runs every v3 query (train + dev + test = 544) through `LLMClient.classify`
and compares the predicted category against the fixture's `category` label.
Output: per-category accuracy + confusion matrix + overall.

This is the prerequisite measurement before committing to the distilled
classifier path. Sets expectations: Gemma's accuracy is the ceiling on
what a distilled student can reach (~95-97% retention per DistilBERT
canon). Decision criteria from the v1.28.3 strategic plan:
    ≥80% Gemma  → green light, proceed with distillation
    60-80%      → yellow, distillation still worth trying with tempered
                  expectations
    <60%        → stop, re-prompt-engineer or revisit the 9-way taxonomy

Run:
    python3 evals/measure_gemma_classify_accuracy.py --save /tmp/gemma-acc.json
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
QUERIES_DIR = REPO / "evals" / "queries"

sys.path.insert(0, str(REPO / "evals"))
from llm_client import LLMClient


SPLITS = [
    ("train", QUERIES_DIR / "v3_train.json"),
    ("test",  QUERIES_DIR / "v3_test.v2.json"),
    ("dev",   QUERIES_DIR / "v3_dev.v2.json"),
]


def load_all() -> list[dict]:
    out = []
    for split, path in SPLITS:
        if not path.exists():
            print(f"[skip] {split}: {path} missing", file=sys.stderr)
            continue
        rows = json.loads(path.read_text())["queries"]
        for r in rows:
            if r.get("category"):
                out.append({"split": split, **r})
    return out


async def main_async():
    ap = argparse.ArgumentParser()
    ap.add_argument("--save", type=Path, required=True)
    ap.add_argument("--concurrency", type=int, default=16)
    args = ap.parse_args()

    rows = load_all()
    print(f"[load] {len(rows)} queries with category labels across {len(SPLITS)} splits",
          file=sys.stderr)
    by_cat = defaultdict(int)
    for r in rows:
        by_cat[r["category"]] += 1
    print("[load] per-category counts:", dict(by_cat), file=sys.stderr)

    client = LLMClient()
    sem = asyncio.Semaphore(args.concurrency)
    predictions: list[dict] = [None] * len(rows)

    async def classify_one(i: int, row: dict):
        async with sem:
            pred = await client.classify(row["query"])
        predictions[i] = {
            "split": row["split"],
            "query": row["query"],
            "true": row["category"],
            "predicted": pred,
            "correct": pred == row["category"],
        }
        if (i + 1) % 25 == 0 or i + 1 == len(rows):
            print(f"  {i+1}/{len(rows)}", file=sys.stderr, flush=True)

    t0 = time.monotonic()
    await asyncio.gather(*[classify_one(i, r) for i, r in enumerate(rows)])
    await client.aclose()
    print(f"[done] {len(rows)} classifications in {time.monotonic()-t0:.1f}s",
          file=sys.stderr)

    by_cat_correct = defaultdict(lambda: {"correct": 0, "total": 0})
    confusion = defaultdict(lambda: defaultdict(int))
    overall_correct = 0
    for p in predictions:
        true = p["true"]
        pred = p["predicted"]
        by_cat_correct[true]["total"] += 1
        if p["correct"]:
            by_cat_correct[true]["correct"] += 1
            overall_correct += 1
        confusion[true][pred] += 1

    n = len(predictions)
    overall_pct = 100 * overall_correct / n

    by_split = defaultdict(lambda: {"correct": 0, "total": 0})
    for p in predictions:
        by_split[p["split"]]["total"] += 1
        if p["correct"]:
            by_split[p["split"]]["correct"] += 1

    print("\n" + "=" * 76)
    print(f"Gemma 4 31B classifier accuracy on v3 ({n} queries)")
    print("=" * 76)
    print(f"\n  OVERALL: {overall_correct}/{n} = {overall_pct:.1f}%\n")
    print(f"  Per split:")
    for split in ("train", "test", "dev"):
        s = by_split[split]
        if s["total"]:
            print(f"    {split:5} {s['correct']:>3}/{s['total']:<3} = {100*s['correct']/s['total']:5.1f}%")

    print(f"\n  Per category:")
    print(f"  {'category':<22} {'N':>4} {'correct':>8} {'acc%':>7}")
    print("  " + "-" * 46)
    for cat in sorted(by_cat_correct.keys()):
        c = by_cat_correct[cat]
        if c["total"]:
            print(f"  {cat:<22} {c['total']:>4} {c['correct']:>8} {100*c['correct']/c['total']:>6.1f}%")

    print(f"\n  Confusion matrix (true rows × predicted cols):")
    cats = sorted(set(by_cat_correct.keys()) | set(p for c in confusion.values() for p in c.keys()))
    short = {c: c[:8] for c in cats}
    print(f"  {'true \\ pred':<22} " + " ".join(f"{short[c]:>9}" for c in cats))
    for true_cat in cats:
        if true_cat not in by_cat_correct:
            continue
        row = " ".join(f"{confusion[true_cat].get(p, 0):>9}" for p in cats)
        print(f"  {true_cat:<22} " + row)

    report = {
        "n_queries": n,
        "overall_correct": overall_correct,
        "overall_accuracy_pct": round(overall_pct, 2),
        "by_split": dict(by_split),
        "by_category": {cat: {"correct": c["correct"], "total": c["total"],
                              "accuracy_pct": round(100 * c["correct"] / c["total"], 2)}
                        for cat, c in by_cat_correct.items()},
        "confusion_matrix": {true: dict(preds) for true, preds in confusion.items()},
        "predictions": predictions,
    }
    args.save.write_text(json.dumps(report, indent=2))
    print(f"\n[saved] {args.save}", file=sys.stderr)

    print("\n  Phase 1.1 decision criteria:")
    if overall_pct >= 80:
        print(f"    {overall_pct:.1f}% ≥ 80% → GREEN LIGHT for distilled classifier (Phase 1.4)")
    elif overall_pct >= 60:
        print(f"    {overall_pct:.1f}% in [60%, 80%) → YELLOW: distillation still viable, expect lower lift")
    else:
        print(f"    {overall_pct:.1f}% < 60% → STOP: re-prompt-engineer Gemma classify or revisit 9-way taxonomy")


def main():
    sys.exit(asyncio.run(main_async()))


if __name__ == "__main__":
    main()
