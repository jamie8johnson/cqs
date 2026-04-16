#!/usr/bin/env python3
"""Build consensus gold-chunk dataset from two judge runs.

Combines validate_gold.py outputs from two different LLM backends (Claude
Haiku + Gemma 4 31B) into three confidence tiers:

  - **high_confidence**: both judges verified AND picked the same gold chunk.
    These are the safest examples for an eval set — two independent models
    agree on the same answer.
  - **single_judge**: only one judge verified (other failed or got a
    different gold). Usable but less defensible. Worth a hand-spot-check
    pass before shipping.
  - **failed**: neither judge could verify a gold chunk. Drop from v3.

Then re-runs the train/dev/test split (600/200/200, stratified by category)
on the high_confidence set and writes:
  evals/queries/v3_consensus.json     — full record with tier tags
  evals/queries/v3_train.json         — overwrites with consensus data
  evals/queries/v3_dev.json
  evals/queries/v3_test.json

Usage: python3 evals/consensus_v3.py
"""

from __future__ import annotations

import argparse
import json
import random
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

QUERIES_DIR = Path(__file__).parent / "queries"
DEFAULT_A = QUERIES_DIR / "v3_validated_claude.json"
DEFAULT_B = QUERIES_DIR / "v3_validated_vllm.json"

OUT_CONSENSUS = QUERIES_DIR / "v3_consensus.json"
OUT_TRAIN = QUERIES_DIR / "v3_train.json"
OUT_DEV = QUERIES_DIR / "v3_dev.json"
OUT_TEST = QUERIES_DIR / "v3_test.json"

N_TRAIN_TARGET = 600
N_DEV_TARGET = 200
N_TEST_TARGET = 200


def gold_id(entry: dict) -> tuple | None:
    g = entry.get("gold_chunk")
    if not g:
        return None
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def merge_one(a: dict, b: dict, judge_a: str, judge_b: str) -> dict:
    """Combine two judge entries into a consensus record."""
    a_ok = bool(a.get("gold_verified"))
    b_ok = bool(b.get("gold_verified"))
    a_gold = gold_id(a) if a_ok else None
    b_gold = gold_id(b) if b_ok else None

    out = {
        "query": a.get("query") or b.get("query"),
        "category": a.get("category") or b.get("category"),
        "source": a.get("source") or b.get("source"),
        "metadata": a.get("metadata") or b.get("metadata") or {},
        "judges": {
            judge_a: {
                "verified": a_ok,
                "gold_chunk": a.get("gold_chunk"),
                "gold_rank": a.get("gold_rank"),
                "gold_appearances": a.get("gold_appearances"),
                "note": a.get("gold_validation_note"),
            },
            judge_b: {
                "verified": b_ok,
                "gold_chunk": b.get("gold_chunk"),
                "gold_rank": b.get("gold_rank"),
                "gold_appearances": b.get("gold_appearances"),
                "note": b.get("gold_validation_note"),
            },
        },
        "pool_size": a.get("pool_size") if a.get("pool_size") is not None else b.get("pool_size"),
    }

    if a_ok and b_ok and a_gold == b_gold:
        out["tier"] = "high_confidence"
        out["gold_chunk"] = a.get("gold_chunk")  # both agreed; either works
        out["gold_chunk_source"] = "consensus"
    elif a_ok and b_ok and a_gold != b_gold:
        out["tier"] = "disagreement"
        # Default: prefer judge A (Claude). This is editable downstream.
        out["gold_chunk"] = a.get("gold_chunk")
        out["gold_chunk_source"] = f"defaulted_to_{judge_a}_pending_review"
    elif a_ok and not b_ok:
        out["tier"] = "single_judge"
        out["gold_chunk"] = a.get("gold_chunk")
        out["gold_chunk_source"] = f"only_{judge_a}_verified"
    elif b_ok and not a_ok:
        out["tier"] = "single_judge"
        out["gold_chunk"] = b.get("gold_chunk")
        out["gold_chunk_source"] = f"only_{judge_b}_verified"
    else:
        out["tier"] = "failed"
        out["gold_chunk"] = None
        out["gold_chunk_source"] = "neither_verified"

    return out


def stratified_split(entries: list[dict], n_train: int, n_dev: int, n_test: int, seed: int = 0):
    rng = random.Random(seed)
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for e in entries:
        by_cat[e["category"]].append(e)
    total = sum(len(v) for v in by_cat.values())
    target_total = n_train + n_dev + n_test
    train_frac = n_train / target_total
    dev_frac = n_dev / target_total
    if total < target_total:
        print(
            f"WARN: high_confidence total {total} < target {target_total}; "
            f"split will be proportionally smaller",
            file=sys.stderr,
        )
    train, dev, test = [], [], []
    for rows in by_cat.values():
        rng.shuffle(rows)
        m = len(rows)
        n_tr = round(m * train_frac)
        n_dv = round(m * dev_frac)
        train.extend(rows[:n_tr])
        dev.extend(rows[n_tr : n_tr + n_dv])
        test.extend(rows[n_tr + n_dv :])
    rng.shuffle(train)
    rng.shuffle(dev)
    rng.shuffle(test)
    return train, dev, test


def write_split(path: Path, entries: list[dict], split_name: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": "v3-consensus",
                "split": split_name,
                "created_at": int(time.time()),
                "n": len(entries),
                "category_counts": dict(Counter(e["category"] for e in entries)),
                "tier_counts": dict(Counter(e["tier"] for e in entries)),
                "queries": entries,
            },
            indent=2,
        )
    )


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--a", type=Path, default=DEFAULT_A)
    p.add_argument("--b", type=Path, default=DEFAULT_B)
    p.add_argument("--judge-a", default="claude")
    p.add_argument("--judge-b", default="gemma")
    p.add_argument("--seed", type=int, default=0)
    args = p.parse_args()

    if not args.a.exists() or not args.b.exists():
        print(f"missing input(s): a={args.a.exists()} b={args.b.exists()}", file=sys.stderr)
        return 1

    a_data = json.loads(args.a.read_text())
    b_data = json.loads(args.b.read_text())
    a_rows = {r["query"]: r for r in a_data.get("queries", [])}
    b_rows = {r["query"]: r for r in b_data.get("queries", [])}

    common = sorted(set(a_rows) & set(b_rows))
    only_a = set(a_rows) - set(b_rows)
    only_b = set(b_rows) - set(a_rows)
    print(f"judge a ({args.judge_a}): {len(a_rows)} entries")
    print(f"judge b ({args.judge_b}): {len(b_rows)} entries")
    print(f"common               : {len(common)}")
    if only_a or only_b:
        print(f"only_a={len(only_a)} only_b={len(only_b)} (queries skipped from consensus)")

    merged = [merge_one(a_rows[q], b_rows[q], args.judge_a, args.judge_b) for q in common]
    tier_counts = Counter(e["tier"] for e in merged)
    print(f"\ntier distribution:")
    for tier in ["high_confidence", "single_judge", "disagreement", "failed"]:
        n = tier_counts.get(tier, 0)
        print(f"  {tier:<18} {n:>4}  ({100*n/len(merged):4.1f}%)")

    OUT_CONSENSUS.parent.mkdir(parents=True, exist_ok=True)
    OUT_CONSENSUS.write_text(
        json.dumps(
            {
                "schema_version": "v3-consensus-full",
                "created_at": int(time.time()),
                "n": len(merged),
                "judges": [args.judge_a, args.judge_b],
                "tier_counts": dict(tier_counts),
                "queries": merged,
            },
            indent=2,
        )
    )
    print(f"\nwrote {OUT_CONSENSUS} ({len(merged)} rows)")

    # Splits use ONLY high_confidence entries — the cleanest signal.
    high_conf = [e for e in merged if e["tier"] == "high_confidence"]
    train, dev, test = stratified_split(high_conf, N_TRAIN_TARGET, N_DEV_TARGET, N_TEST_TARGET, seed=args.seed)
    write_split(OUT_TRAIN, train, "train")
    write_split(OUT_DEV, dev, "dev")
    write_split(OUT_TEST, test, "test")
    print(f"\nsplits (high_confidence only):")
    print(f"  train={len(train)}  dev={len(dev)}  test={len(test)}  sum={len(train)+len(dev)+len(test)}")
    print(f"  per-category in train:")
    for cat, n in Counter(e["category"] for e in train).most_common():
        print(f"    {cat:<22} {n:>4}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
