#!/usr/bin/env python3
"""Stratified sample of labeled telemetry queries for hand-audit.

Reads evals/queries/v3_telemetry_labeled.json, samples up to N per category,
prints category + query so a human can flag miscategorizations.

Usage: python3 evals/spot_check_labels.py [--per-cat N] [--seed S]
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from collections import defaultdict
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--per-cat", type=int, default=3)
    p.add_argument("--seed", type=int, default=0)
    p.add_argument("--in", dest="inp", type=Path, default=Path(__file__).parent / "queries" / "v3_telemetry_labeled.json")
    args = p.parse_args()

    data = json.loads(args.inp.read_text())
    by_cat: dict[str, list[str]] = defaultdict(list)
    for row in data["queries"]:
        by_cat[row["category"]].append(row["query"])

    random.seed(args.seed)
    total = 0
    for cat in sorted(by_cat, key=lambda c: -len(by_cat[c])):
        bag = by_cat[cat]
        k = min(args.per_cat, len(bag))
        if not k:
            continue
        sample = random.sample(bag, k)
        print(f"\n=== {cat} (N={len(bag)}, showing {k}) ===")
        for q in sample:
            print(f"  - {q}")
        total += k
    print(f"\ntotal sampled: {total}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
