#!/usr/bin/env python3
"""Label the 328 unique real queries from telemetry via the local Gemma 4 server.

Step 1 of the v3-eval build. Reads ~/.cache/cqs/query_log.jsonl, filters to
unique real-looking queries (same filter as analyze_telemetry.py), calls
llm_client.classify() on each, and writes the labeled set to
evals/queries/v3_telemetry_labeled.json.

Writes per-category counts to stdout so we can see where the real-query corpus
is thin and needs synthetic augmentation in step 2 (generate_from_chunks.py).

Usage: python3 evals/label_telemetry_queries.py [--concurrency N]
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sys
import time
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from llm_client import LLMClient  # noqa: E402

LOG_PATH = Path(os.environ.get("CQS_QUERY_LOG", os.path.expanduser("~/.cache/cqs/query_log.jsonl")))
OUT_PATH = Path(__file__).parent / "queries" / "v3_telemetry_labeled.json"
MIN_LEN = 4
MAX_LEN = 200
JUNK_RE = re.compile(r"^\s*(test|foo|bar|baz|qux|xxx)\s*$", re.I)


def looks_real(q: str) -> bool:
    if not q or len(q) < MIN_LEN or len(q) > MAX_LEN:
        return False
    if JUNK_RE.match(q):
        return False
    if not re.search(r"[A-Za-z]{3,}", q):
        return False
    return True


def load_unique_real_queries(path: Path) -> list[dict]:
    first_seen: dict[str, int] = {}
    first_cmd: dict[str, str] = {}
    with path.open() as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            try:
                r = json.loads(ln)
            except json.JSONDecodeError:
                continue
            q = (r.get("query") or "").strip()
            if not looks_real(q):
                continue
            if q not in first_seen:
                first_seen[q] = r.get("ts", 0)
                first_cmd[q] = r.get("cmd", "")
    return [
        {"query": q, "first_seen_ts": first_seen[q], "source_cmd": first_cmd[q]}
        for q in sorted(first_seen)
    ]


async def _label_one(client: LLMClient, sem: asyncio.Semaphore, row: dict, errors: list) -> dict:
    async with sem:
        try:
            row["category"] = await client.classify(row["query"])
        except Exception as e:  # noqa: BLE001
            row["category"] = "unknown"
            row["error"] = f"{type(e).__name__}: {e}"
            errors.append(row["error"])
    return row


async def main(concurrency: int) -> int:
    if not LOG_PATH.exists():
        print(f"log missing: {LOG_PATH}", file=sys.stderr)
        return 1

    rows = load_unique_real_queries(LOG_PATH)
    print(f"unique real queries: {len(rows)}")

    client = LLMClient()
    sem = asyncio.Semaphore(concurrency)
    errors: list[str] = []
    t0 = time.monotonic()
    labeled = await asyncio.gather(*(_label_one(client, sem, r, errors) for r in rows))
    dt = time.monotonic() - t0
    await client.aclose()

    if errors:
        print(f"\nERRORS: {len(errors)} requests failed; first 3:", file=sys.stderr)
        for e in errors[:3]:
            print(f"  {e}", file=sys.stderr)
        if len(errors) >= len(rows) * 0.5:
            print("ABORT: >=50% failure rate, not writing output", file=sys.stderr)
            return 2

    cat_counts = Counter(r["category"] for r in labeled)
    print(f"\nclassification time: {dt:.1f}s ({len(labeled)/dt:.1f} q/s)")
    print("\ncategory distribution:")
    for cat, n in cat_counts.most_common():
        print(f"  {cat:<22} {n:>4}  ({100*n/len(labeled):4.1f}%)")

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUT_PATH.write_text(
        json.dumps(
            {
                "source": str(LOG_PATH),
                "n_queries": len(labeled),
                "classified_at": int(time.time()),
                "model": client.model,
                "category_counts": dict(cat_counts),
                "queries": labeled,
            },
            indent=2,
        )
    )
    print(f"\nwrote {OUT_PATH}")
    return 0


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--concurrency", type=int, default=32)
    args = p.parse_args()
    sys.exit(asyncio.run(main(args.concurrency)))
