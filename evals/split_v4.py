#!/usr/bin/env python3
"""Split the v4 generated synthetic queries into v4_test.v2 / v4_dev.v2 splits.

Held out from v3 gold chunks AND from Phase 1.3 seed chunks (per the
--exclude-chunks pass to generate_from_chunks.py). Each category split
50/50 into test + dev.

Run:
    python3 evals/split_v4.py \\
        --src evals/queries/v4_generated.json \\
        --test-out evals/queries/v4_test.v2.json \\
        --dev-out evals/queries/v4_dev.v2.json
"""

from __future__ import annotations

import argparse
import json
import random
from collections import Counter, defaultdict
from pathlib import Path


CATEGORIES = [
    "identifier_lookup", "structural_search", "behavioral_search",
    "conceptual_search", "multi_step", "negation",
    "type_filtered", "cross_language",
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True, type=Path)
    ap.add_argument("--test-out", required=True, type=Path)
    ap.add_argument("--dev-out", required=True, type=Path)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    raw = json.loads(args.src.read_text())
    rows = [r for r in raw.get("queries", []) if r.get("matched", False)]
    print(f"loaded {len(rows)} matched=True queries from {args.src}")

    by_cat = defaultdict(list)
    for r in rows:
        by_cat[r["category"]].append(r)
    print("per-category counts:")
    for c, lst in sorted(by_cat.items()):
        print(f"  {c:<22} {len(lst)}")

    rng = random.Random(args.seed)
    test_queries = []
    dev_queries = []
    for cat in CATEGORIES:
        lst = list(by_cat.get(cat, []))
        rng.shuffle(lst)
        half = len(lst) // 2
        test_queries.extend(lst[:half])
        dev_queries.extend(lst[half:])

    # Strip per-row metadata to mirror v3 schema (just query + category + gold_chunk).
    def strip(r):
        return {
            "query": r["query"],
            "category": r["category"],
            "gold_chunk": r["gold_chunk"],
        }
    test_queries = [strip(r) for r in test_queries]
    dev_queries = [strip(r) for r in dev_queries]

    args.test_out.write_text(json.dumps({"queries": test_queries}, indent=2))
    args.dev_out.write_text(json.dumps({"queries": dev_queries}, indent=2))
    print(f"\nwrote {args.test_out} ({len(test_queries)} queries)")
    print(f"wrote {args.dev_out} ({len(dev_queries)} queries)")
    print("test per-category:", dict(Counter(r["category"] for r in test_queries)))
    print("dev  per-category:", dict(Counter(r["category"] for r in dev_queries)))


if __name__ == "__main__":
    main()
