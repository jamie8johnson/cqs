#!/usr/bin/env python3
"""A/B eval: distilled query classifier head OFF vs ON.

Phase 1.4b — measures whether the retrained head (trained on v3 + 3833
synthetic Gemma-labeled queries, val acc 88.1%) lifts production R@5 over
the rule+centroid baseline (which already ships in v1.28.2).

Both cells use centroid ON (v1.28.2+ default). The only difference is
`CQS_DISTILLED_CLASSIFIER` (0 vs 1). Bypasses the daemon
(`CQS_NO_DAEMON=1`) so the env flips fresh on every query — the head's
load decision is `OnceLock`-cached per process.

Run:
    python3 evals/distilled_head_ab_eval.py --save /tmp/distilled-ab.json
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

QUERIES_DIR = Path(__file__).resolve().parent / "queries"


def gold_key(g):
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_at_k(gold, results, k):
    target = gold_key(gold)
    for i, r in enumerate(results[:k]):
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return i + 1
    return None


def run_one(query: str, head_on: bool, limit: int, timeout: int = 60):
    env = {
        **os.environ,
        "CQS_NO_DAEMON": "1",
        "CQS_CENTROID_CLASSIFIER": "1",  # baseline (v1.28.2+ default)
        "CQS_DISTILLED_CLASSIFIER": "1" if head_on else "0",
    }
    cmd = ["cqs", "--json", "-n", str(limit), "--splade", "--", query]
    try:
        r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                           text=True, timeout=timeout, env=env)
    except subprocess.TimeoutExpired:
        return []
    try:
        envelope = json.loads(r.stdout)
        payload = envelope.get("data") if isinstance(envelope.get("data"), dict) else envelope
        return payload.get("results", []) if payload else []
    except (json.JSONDecodeError, AttributeError):
        return []


def eval_cell(rows, head_on: bool, limit: int, label: str):
    print(f"\n[cell] head {label} ({len(rows)} queries)", file=sys.stderr)
    overall = {"r1": 0, "r5": 0, "r20": 0, "n": 0}
    by_cat = defaultdict(lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0})
    t0 = time.monotonic()
    for i, row in enumerate(rows):
        results = run_one(row["query"], head_on, limit)
        gold = row.get("gold_chunk") or {}
        cat = row.get("category", "unknown")
        overall["n"] += 1
        by_cat[cat]["n"] += 1
        for k in (1, 5, 20):
            if match_at_k(gold, results, k) is not None:
                overall[f"r{k}"] += 1
                by_cat[cat][f"r{k}"] += 1
        if (i + 1) % 25 == 0 or i + 1 == len(rows):
            rate = (i + 1) / max(time.monotonic() - t0, 0.01)
            print(f"  {label}: {i+1}/{len(rows)} ({rate:.2f} qps)",
                  file=sys.stderr, flush=True)
    return overall, dict(by_cat)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--save", type=Path)
    ap.add_argument("--limit", type=int, default=20)
    args = ap.parse_args()

    report = {"splits": {}}
    for split in ("test", "dev"):
        src = QUERIES_DIR / f"v3_{split}.v2.json"
        rows = json.loads(src.read_text())["queries"]
        print(f"\n=== split {split} ({len(rows)} queries) ===", file=sys.stderr)

        off_overall, off_by_cat = eval_cell(rows, head_on=False, limit=args.limit, label="OFF")
        on_overall, on_by_cat = eval_cell(rows, head_on=True, limit=args.limit, label="ON")

        report["splits"][split] = {
            "n": len(rows),
            "off": {"overall": off_overall, "by_cat": off_by_cat},
            "on": {"overall": on_overall, "by_cat": on_by_cat},
        }

    print("\n" + "=" * 76)
    print("Distilled Head A/B (centroid ON in both cells)")
    print("=" * 76)
    for split in ("test", "dev"):
        s = report["splits"][split]
        n = s["n"]
        off = s["off"]["overall"]; on = s["on"]["overall"]
        print(f"\n--- {split} (N={n}) ---")
        print(f"  {'config':<8} {'R@1':>7} {'R@5':>7} {'R@20':>7}")
        for label, c in (("OFF", off), ("ON", on)):
            print(f"  {label:<8} {100*c['r1']/n:6.1f}% {100*c['r5']/n:6.1f}% {100*c['r20']/n:6.1f}%")
        d1 = 100*(on["r1"]-off["r1"])/n
        d5 = 100*(on["r5"]-off["r5"])/n
        d20 = 100*(on["r20"]-off["r20"])/n
        print(f"  {'Δ pp':<8} {d1:+6.1f}  {d5:+6.1f}  {d20:+6.1f}")

        print(f"\n  per-category R@5 (ON vs OFF):")
        cats = sorted(set(s["off"]["by_cat"]) | set(s["on"]["by_cat"]))
        for cat in cats:
            o = s["off"]["by_cat"].get(cat, {"r5": 0, "n": 0})
            n_on = s["on"]["by_cat"].get(cat, {"r5": 0, "n": 0})
            cn = max(o["n"], n_on["n"], 1)
            off_r5 = 100*o["r5"]/cn
            on_r5 = 100*n_on["r5"]/cn
            d = on_r5 - off_r5
            print(f"    {cat:<22} N={cn:<3} OFF={off_r5:5.1f}%  ON={on_r5:5.1f}%  Δ={d:+5.1f}pp")

    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


if __name__ == "__main__":
    main()
