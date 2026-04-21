#!/usr/bin/env python3
"""Single-cell baseline measurement on v4 (test + dev) for the long-chunk
doc-aware windowing lever.

Compares against PR #1069's pre-fix v4 baseline (test R@5=48.9%, R@20=63.2%;
dev R@5=49.9%, R@20=62.6%). Daemon is whatever is currently running — no
env overrides, no head toggles.
"""

from __future__ import annotations
import argparse, json, shlex, subprocess, sys, time
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


def run_batch(queries, limit=20):
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/baseline-v4.stderr", "ab"),
        text=True, bufsize=1,
    )
    out = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            cmd = f"search {shlex.quote(q)} --limit {limit} --splade"
            try:
                proc.stdin.write(cmd + "\n"); proc.stdin.flush()
            except (BrokenPipeError, OSError): break
            line = proc.stdout.readline()
            if not line: break
            try:
                env = json.loads(line)
                payload = env.get("data") if isinstance(env.get("data"), dict) else env
                out.append(payload.get("results", []) if payload else [])
            except json.JSONDecodeError:
                out.append([])
            if (i+1) % 100 == 0 or i+1 == len(queries):
                rate = (i+1) / max(time.monotonic()-t0, 0.01)
                print(f"  {i+1}/{len(queries)} ({rate:.1f} qps)", file=sys.stderr, flush=True)
    finally:
        try: proc.stdin.close(); proc.wait(timeout=5)
        except Exception: proc.kill()
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--save", type=Path)
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--prefix", default="v4")
    args = ap.parse_args()

    report = {"prefix": args.prefix, "splits": {}}
    for split in ("test", "dev"):
        src = QUERIES_DIR / f"{args.prefix}_{split}.v2.json"
        rows = json.loads(src.read_text())["queries"]
        print(f"\n=== {split} ({len(rows)} queries) ===", file=sys.stderr)
        results = run_batch([r["query"] for r in rows], limit=args.limit)
        overall = {"r1": 0, "r5": 0, "r20": 0, "n": len(rows)}
        by_cat = defaultdict(lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0})
        per_query = []
        for row, res in zip(rows, results):
            gold = row.get("gold_chunk") or {}
            cat = row.get("category", "unknown")
            by_cat[cat]["n"] += 1
            ranks = {}
            for k in (1, 5, 20):
                rank = match_at_k(gold, res, k)
                if rank is not None:
                    overall[f"r{k}"] += 1
                    by_cat[cat][f"r{k}"] += 1
                    ranks[f"r{k}"] = rank
            per_query.append({
                "query": row["query"], "category": cat,
                "gold_id": gold.get("id"), "ranks": ranks,
            })
        report["splits"][split] = {
            "n": len(rows), "overall": overall,
            "by_cat": dict(by_cat), "per_query": per_query,
        }

    print("\n" + "=" * 76)
    print(f"Baseline v{args.prefix} (single cell, no head overrides)")
    print("=" * 76)
    print("PR #1069 pre-fix baseline: test R@5=48.9% / dev R@5=49.9%")
    for split in ("test", "dev"):
        s = report["splits"][split]; n = s["n"]; o = s["overall"]
        print(f"\n--- {split} (N={n}) ---")
        print(f"  R@1={100*o['r1']/n:.1f}%  R@5={100*o['r5']/n:.1f}%  R@20={100*o['r20']/n:.1f}%")
        print(f"  per-category R@5:")
        for cat in sorted(s["by_cat"]):
            c = s["by_cat"][cat]; cn = c["n"]
            if cn:
                print(f"    {cat:<22} N={cn:<3} R@5={100*c['r5']/cn:5.1f}%")

    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


if __name__ == "__main__":
    main()
