#!/usr/bin/env python3
"""Reshape v3 pools + splits into reranker training triples.

For each split (train/dev/test): produce (query, chunk_content, label)
triples where label=1.0 for the gold chunk and label=0.0 for hard
negatives drawn from the same retrieval pool. The pool members are
the hardest negatives possible — cqs already surfaced them as relevant.

Output schema (JSONL, one per line):
    {"query": str, "content": str, "label": float,
     "query_idx": int, "gold": bool, "chunk_name": str, "chunk_file": str}

Written files:
    evals/reranker_v2_train.jsonl
    evals/reranker_v2_dev.jsonl
    evals/reranker_v2_test.jsonl
"""

from __future__ import annotations

import json
import random
import sys
from collections import Counter
from pathlib import Path

QUERIES_DIR = Path(__file__).parent / "queries"
POOLS_PATH = QUERIES_DIR / "v3_pools.json"
SPLITS = [
    ("train", QUERIES_DIR / "v3_train.json"),
    ("dev", QUERIES_DIR / "v3_dev.json"),
    ("test", QUERIES_DIR / "v3_test.json"),
]
OUT_DIR = Path(__file__).parent

NEGATIVES_PER_POSITIVE = 6  # 1 positive + 6 negatives = 7-way pair group


def _chunk_key(r: dict) -> tuple:
    return (r.get("file") or r.get("origin"), r.get("name"), r.get("line_start"))


def _result_content(result: dict) -> str:
    # Must match cqs's rerank path exactly: `unified_text` passes
    # `chunk.content.as_str()` — no signature prefix. Training on
    # (query, content) and inferring on (query, signature+content)
    # produces a catastrophic input-shape mismatch.
    return result.get("content") or result.get("preview") or result.get("signature") or ""


def main() -> int:
    pools = json.loads(POOLS_PATH.read_text())
    pools_by_query: dict[str, dict] = {p["query"]: p for p in pools["pools"]}

    rng = random.Random(0)
    summary: dict[str, int] = {}

    for name, split_path in SPLITS:
        if not split_path.exists():
            print(f"skipping {name} — missing file", file=sys.stderr)
            continue
        data = json.loads(split_path.read_text())
        rows = data["queries"]

        out_path = OUT_DIR / f"reranker_v2_{name}.jsonl"
        n_examples = 0
        n_queries = 0
        n_missing_pool = 0
        n_missing_gold = 0
        cat_counts: Counter = Counter()

        with out_path.open("w") as f:
            for idx, entry in enumerate(rows):
                query = entry["query"]
                gold = entry.get("gold_chunk")
                if not gold:
                    n_missing_gold += 1
                    continue
                pool_entry = pools_by_query.get(query)
                if not pool_entry or not pool_entry.get("pool"):
                    n_missing_pool += 1
                    continue

                gold_key = (gold.get("origin"), gold.get("name"), gold.get("line_start"))
                pool = pool_entry["pool"]

                # Find positive in pool (the pool member matching gold).
                positive = None
                negatives: list[dict] = []
                for p in pool:
                    r = p["result"]
                    if _chunk_key(r) == gold_key:
                        positive = r
                    else:
                        negatives.append(r)

                if positive is None:
                    # Gold not in the pool (should be rare for high_confidence split).
                    # Synthesize positive from the gold_chunk itself — we don't have
                    # its full content though, so skip.
                    n_missing_pool += 1
                    continue

                pos_content = _result_content(positive)
                if not pos_content:
                    continue

                # Emit the positive.
                f.write(json.dumps({
                    "query": query,
                    "content": pos_content,
                    "label": 1.0,
                    "query_idx": idx,
                    "gold": True,
                    "chunk_name": positive.get("name"),
                    "chunk_file": positive.get("file"),
                }) + "\n")
                n_examples += 1

                # Sample hard negatives from the pool.
                rng.shuffle(negatives)
                for neg in negatives[:NEGATIVES_PER_POSITIVE]:
                    neg_content = _result_content(neg)
                    if not neg_content:
                        continue
                    f.write(json.dumps({
                        "query": query,
                        "content": neg_content,
                        "label": 0.0,
                        "query_idx": idx,
                        "gold": False,
                        "chunk_name": neg.get("name"),
                        "chunk_file": neg.get("file"),
                    }) + "\n")
                    n_examples += 1

                n_queries += 1
                cat_counts[entry.get("category", "unknown")] += 1

        summary[name] = n_examples
        print(
            f"{name}: {n_queries} queries → {n_examples} examples "
            f"(missing_pool={n_missing_pool} missing_gold={n_missing_gold})"
        )
        print(f"  per-category: {dict(cat_counts.most_common())}")
        print(f"  wrote {out_path}")

    print("\ntotal:", summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
