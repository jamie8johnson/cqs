#!/usr/bin/env python3
"""Analyze cqs query-log telemetry to shape the v3 eval sampling strategy.

Reads ~/.cache/cqs/query_log.jsonl, reports command distribution, unique-query
counts, length histogram, and per-command samples. No LLM calls — this is a
cheap scouting pass before we spend compute on labeling.

Usage: python3 evals/analyze_telemetry.py
"""

from __future__ import annotations

import json
import os
import random
import re
import sys
from collections import Counter
from pathlib import Path

LOG_PATH = Path(os.environ.get("CQS_QUERY_LOG", os.path.expanduser("~/.cache/cqs/query_log.jsonl")))

# Cheap "is this a real query" filter.
MIN_LEN = 4
MAX_LEN = 200
JUNK_RE = re.compile(r"^\s*(test|foo|bar|baz|qux|xxx)\s*$", re.I)


def load(path: Path) -> list[dict]:
    rows: list[dict] = []
    with path.open() as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            try:
                rows.append(json.loads(ln))
            except json.JSONDecodeError:
                continue
    return rows


def looks_real(q: str) -> bool:
    if not q or len(q) < MIN_LEN or len(q) > MAX_LEN:
        return False
    if JUNK_RE.match(q):
        return False
    # Reject strings that are 100% punctuation / pipes / etc.
    if not re.search(r"[A-Za-z]{3,}", q):
        return False
    return True


def main() -> int:
    if not LOG_PATH.exists():
        print(f"log missing: {LOG_PATH}", file=sys.stderr)
        return 1

    rows = load(LOG_PATH)
    print(f"total rows         : {len(rows):>6}")

    cmds = Counter(r.get("cmd", "") for r in rows)
    print("\ncommand distribution:")
    for cmd, n in cmds.most_common():
        print(f"  {cmd:<18} {n:>6}  ({100*n/len(rows):4.1f}%)")

    # Queries (any command) — dedupe and filter
    all_queries = [r.get("query", "") or "" for r in rows]
    non_empty = [q for q in all_queries if q]
    uniq = set(non_empty)
    print(f"\nnon-empty queries  : {len(non_empty):>6}")
    print(f"unique queries     : {len(uniq):>6}")

    real = [q for q in uniq if looks_real(q)]
    print(f"unique + real      : {len(real):>6}")

    # Length histogram
    lens = [len(q) for q in real]
    if lens:
        buckets = [0, 10, 20, 40, 80, 160, max(lens) + 1]
        print("\nlength histogram (chars):")
        for lo, hi in zip(buckets[:-1], buckets[1:]):
            n = sum(1 for L in lens if lo <= L < hi)
            bar = "█" * (40 * n // max(lens[:1] and 1, len(lens)))  # rough
            print(f"  [{lo:>3}, {hi:>3})  {n:>5}  {bar}")

    # Per-command unique queries
    per_cmd_unique: dict[str, set] = {}
    for r in rows:
        q = (r.get("query", "") or "").strip()
        cmd = r.get("cmd", "")
        if q and looks_real(q):
            per_cmd_unique.setdefault(cmd, set()).add(q)
    print("\nunique queries per command (filtered):")
    for cmd in sorted(per_cmd_unique, key=lambda c: -len(per_cmd_unique[c])):
        print(f"  {cmd:<18} {len(per_cmd_unique[cmd]):>5}")

    # Sample 6 real queries from each retrieval-flavored command
    retrieval_cmds = ["search", "gather", "context", "scout", "onboard", "impact", "callers", "callees"]
    random.seed(0)
    print("\nsamples (retrieval commands):")
    for cmd in retrieval_cmds:
        bag = sorted(per_cmd_unique.get(cmd, set()))
        if not bag:
            continue
        print(f"\n  {cmd}:")
        for q in random.sample(bag, min(6, len(bag))):
            print(f"    - {q}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
