#!/usr/bin/env python3
"""A/B eval: centroid classifier OFF (env=0, default) vs ON (env=1).

The centroid classifier (`src/search/router.rs`) categorizes each query
into one of ~9 buckets and applies category-specific routing weights.
It's opt-in via `CQS_CENTROID_CLASSIFIER=1` and every eval script we
ship currently sets it to 0 — meaning we've never measured whether it
helps. v3.v2 has per-query category labels which makes a per-category
breakdown trivial.

Method: bypass the daemon (use CLI mode via CQS_NO_DAEMON=1) so the
env var is honored fresh on each query. Both cells hit the same on-disk
index. Compare R@1/R@5/R@20 overall AND per category, on test+dev.

The CLI path is slower than batch (~2s startup × N queries) but exact
isolation matters more than throughput here. ~7 min per cell on 109
queries × 2 splits.

Run:
    python3 evals/classifier_ab_eval.py --save /tmp/classifier-ab.json
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


def run_one(query: str, classifier_on: bool, limit: int, timeout: int = 60):
    env = {
        **os.environ,
        "CQS_NO_DAEMON": "1",
        "CQS_CENTROID_CLASSIFIER": "1" if classifier_on else "0",
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


def eval_cell(rows, classifier_on: bool, limit: int, label: str):
    print(f"\n[cell] classifier {label} ({len(rows)} queries)", file=sys.stderr)
    overall = {"r1": 0, "r5": 0, "r20": 0, "n": 0}
    by_cat = defaultdict(lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0})
    t0 = time.monotonic()
    for i, row in enumerate(rows):
        results = run_one(row["query"], classifier_on, limit)
        gold = row.get("gold_chunk") or {}
        cat = row.get("category", "unknown")
        overall["n"] += 1
        by_cat[cat]["n"] += 1
        for k in (1, 5, 20):
            if match_at_k(gold, results, k) is not None:
                overall[f"r{k}"] += 1
                by_cat[cat][f"r{k}"] += 1
        if (i + 1) % 10 == 0 or i + 1 == len(rows):
            elapsed = time.monotonic() - t0
            rate = (i + 1) / max(elapsed, 0.01)
            print(f"  {i+1}/{len(rows)} ({rate:.2f} qps)", file=sys.stderr, flush=True)
    return overall, dict(by_cat)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--save", type=Path)
    ap.add_argument("--splits", default="test,dev")
    args = ap.parse_args()

    report = {"splits": {}}
    splits = args.splits.split(",")

    for split in splits:
        src = QUERIES_DIR / f"v3_{split}.v2.json"
        rows = json.loads(src.read_text())["queries"]

        off_overall, off_cat = eval_cell(rows, classifier_on=False, limit=args.limit, label="OFF")
        on_overall, on_cat = eval_cell(rows, classifier_on=True, limit=args.limit, label="ON")

        report["splits"][split] = {
            "off": {"overall": off_overall, "by_cat": off_cat},
            "on":  {"overall": on_overall,  "by_cat": on_cat},
        }

    print("\n" + "=" * 78)
    print("Centroid classifier A/B (CQS_CENTROID_CLASSIFIER: 0 vs 1)")
    print("=" * 78)
    print(f"| {'Split':6} | {'Metric':5} | {'OFF':10} | {'ON':10} | {'Δ (pp)':8} |")
    print(f"|{'-'*8}|{'-'*7}|{'-'*12}|{'-'*12}|{'-'*10}|")
    for split in splits:
        off = report["splits"][split]["off"]["overall"]
        on = report["splits"][split]["on"]["overall"]
        n = on["n"]
        for k in ("r1", "r5", "r20"):
            d = (on[k] - off[k]) / n * 100
            marker = "  " if abs(d) < 0.5 else ("↑↑" if d > 2 else "↑ " if d > 0 else "↓ " if d > -2 else "↓↓")
            print(f"| {split:6} | R@{k[1:]:3} | {100*off[k]/n:9.1f}% | {100*on[k]/n:9.1f}% | "
                  f"{d:+6.1f} {marker}|")

    print("\nPer-category breakdown (R@5 only, both splits combined):")
    print(f"| {'Category':25} | {'N':4} | {'OFF':6} | {'ON':6} | {'Δ pp':6} |")
    print(f"|{'-'*27}|{'-'*6}|{'-'*8}|{'-'*8}|{'-'*8}|")
    cat_totals = defaultdict(lambda: {"off_r5": 0, "on_r5": 0, "n": 0})
    for split in splits:
        for cat, c in report["splits"][split]["off"]["by_cat"].items():
            cat_totals[cat]["off_r5"] += c["r5"]
            cat_totals[cat]["n"] += c["n"]
        for cat, c in report["splits"][split]["on"]["by_cat"].items():
            cat_totals[cat]["on_r5"] += c["r5"]
    for cat in sorted(cat_totals.keys()):
        t = cat_totals[cat]
        if t["n"] == 0:
            continue
        off_pct = 100 * t["off_r5"] / t["n"]
        on_pct = 100 * t["on_r5"] / t["n"]
        d = on_pct - off_pct
        print(f"| {cat:25} | {t['n']:4d} | {off_pct:5.1f}% | {on_pct:5.1f}% | {d:+5.1f} |")

    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


if __name__ == "__main__":
    main()
