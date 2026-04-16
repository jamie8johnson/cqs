#!/usr/bin/env python3
"""Merge v3 eval sources into a single dataset with train/dev/test splits.

Sources (read from evals/queries/):
  v3_telemetry_labeled.json   — 328 real queries from telemetry, classified
  v3_generated_scale.json     — ~500 LLM-generated queries targeting thin cats
  v3_generated_round1b.json   — optional, earlier 60-query smoke (deduped on merge)

Writes:
  evals/queries/v3_train.json  (600 queries)
  evals/queries/v3_dev.json    (200 queries)
  evals/queries/v3_test.json   (200 queries)
  evals/queries/v3_all.json    (all merged, for debugging / gold-validation pass)

Categories are stratified across splits so the dev/test sets mirror the train
distribution. Source (telemetry vs generated) is preserved; split does NOT
balance source type — we want the distribution the model will see in production.

Queries WITHOUT a gold_chunk (all telemetry queries, some generated
if the validator was disabled) get passed through. A separate script
(validate_gold.py) runs cqs search + LLM-validate to fill them in.

Usage: python3 evals/merge_v3.py
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
TELEMETRY_SRC = QUERIES_DIR / "v3_telemetry_labeled.json"
GENERATED_SRCS = [
    QUERIES_DIR / "v3_generated_scale.json",
    QUERIES_DIR / "v3_generated_round1b.json",
]
OUT_ALL = QUERIES_DIR / "v3_all.json"
OUT_TRAIN = QUERIES_DIR / "v3_train.json"
OUT_DEV = QUERIES_DIR / "v3_dev.json"
OUT_TEST = QUERIES_DIR / "v3_test.json"

# Target split sizes.
N_TRAIN = 600
N_DEV = 200
N_TEST = 200


def _normalize(raw: dict, source: str) -> dict | None:
    """Convert a raw row from either source into the unified v3 schema."""
    q = (raw.get("query") or "").strip()
    cat = raw.get("category")
    if not q or not cat:
        return None
    entry: dict = {
        "query": q,
        "category": cat,
        "source": source,
        "gold_chunk": raw.get("gold_chunk"),
        "metadata": {},
    }
    if source == "telemetry":
        entry["metadata"]["first_seen_ts"] = raw.get("first_seen_ts")
        entry["metadata"]["source_cmd"] = raw.get("source_cmd")
    else:
        # generated
        entry["metadata"]["target_category"] = raw.get("target_category")
        entry["metadata"]["matched"] = raw.get("matched", None)
    return entry


def load_source(path: Path, source: str) -> list[dict]:
    if not path.exists():
        return []
    data = json.loads(path.read_text())
    rows = data.get("queries") or []
    out: list[dict] = []
    for r in rows:
        norm = _normalize(r, source)
        if norm:
            out.append(norm)
    return out


def dedupe_by_query(entries: list[dict]) -> list[dict]:
    """First occurrence wins. Order matters: pass telemetry in first so real
    queries trump generated duplicates with the same text."""
    seen: set[str] = set()
    out: list[dict] = []
    for e in entries:
        key = e["query"]
        if key in seen:
            continue
        seen.add(key)
        out.append(e)
    return out


def stratified_split(
    entries: list[dict], n_train: int, n_dev: int, n_test: int, seed: int = 0
) -> tuple[list[dict], list[dict], list[dict]]:
    """Split by category so dev/test mirror the overall distribution."""
    rng = random.Random(seed)
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for e in entries:
        by_cat[e["category"]].append(e)
    total = sum(len(v) for v in by_cat.values())
    if total < n_train + n_dev + n_test:
        print(
            f"WARN: total {total} < target {n_train+n_dev+n_test}. "
            "Proportional split will produce smaller sets.",
            file=sys.stderr,
        )

    train: list[dict] = []
    dev: list[dict] = []
    test: list[dict] = []
    target_total = n_train + n_dev + n_test
    train_frac = n_train / target_total
    dev_frac = n_dev / target_total

    for cat, rows in by_cat.items():
        rng.shuffle(rows)
        m = len(rows)
        n_tr = round(m * train_frac)
        n_dv = round(m * dev_frac)
        n_te = m - n_tr - n_dv
        train.extend(rows[:n_tr])
        dev.extend(rows[n_tr : n_tr + n_dv])
        test.extend(rows[n_tr + n_dv :])
    rng.shuffle(train)
    rng.shuffle(dev)
    rng.shuffle(test)
    return train, dev, test


def category_breakdown(entries: list[dict]) -> list[tuple[str, int]]:
    c = Counter(e["category"] for e in entries)
    return c.most_common()


def write(path: Path, entries: list[dict], split_name: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": "v3",
                "split": split_name,
                "created_at": int(time.time()),
                "n": len(entries),
                "category_counts": dict(Counter(e["category"] for e in entries)),
                "source_counts": dict(Counter(e["source"] for e in entries)),
                "queries": entries,
            },
            indent=2,
        )
    )


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--seed", type=int, default=0)
    args = p.parse_args()

    telemetry = load_source(TELEMETRY_SRC, "telemetry")
    generated: list[dict] = []
    for src in GENERATED_SRCS:
        rows = load_source(src, "generated")
        generated.extend(rows)

    print(f"telemetry loaded : {len(telemetry):>4}")
    print(f"generated loaded : {len(generated):>4}")

    merged = dedupe_by_query(telemetry + generated)
    print(f"after dedupe     : {len(merged):>4}")

    print("\ncategory breakdown (merged):")
    for cat, n in category_breakdown(merged):
        print(f"  {cat:<22} {n:>4}")

    print("\nsource breakdown:")
    for src, n in Counter(e["source"] for e in merged).most_common():
        print(f"  {src:<22} {n:>4}")

    # Write full merged set (useful for gold-validation pass).
    write(OUT_ALL, merged, "all")
    print(f"\nwrote {OUT_ALL} ({len(merged)} rows)")

    train, dev, test = stratified_split(merged, N_TRAIN, N_DEV, N_TEST, seed=args.seed)
    print(
        f"\nsplits: train={len(train)}  dev={len(dev)}  test={len(test)}  "
        f"(sum={len(train)+len(dev)+len(test)})"
    )
    write(OUT_TRAIN, train, "train")
    write(OUT_DEV, dev, "dev")
    write(OUT_TEST, test, "test")
    print(f"wrote {OUT_TRAIN}\nwrote {OUT_DEV}\nwrote {OUT_TEST}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
