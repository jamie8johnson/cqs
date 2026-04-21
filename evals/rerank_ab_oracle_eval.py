#!/usr/bin/env python3
"""Oracle-router R@5 ceiling test.

Phase 1.4 distilled head delivered ±0pp R@5. Two competing hypotheses:
  H1: Head accuracy (79.8%) is the bottleneck — push to 90%+ via label
      expansion and the lift would materialize.
  H2: The per-category α lift just isn't there for this corpus state —
      even a perfect router would deliver 0pp.

This script tests H2: use Gemma 4 31B's predictions as the router (99%
accuracy on v3 per Phase 1.1 measurement). If R@5 lifts vs the rule+
centroid baseline, H1 is true and label expansion is justified. If R@5
doesn't move, H2 is true and the alpha-routing arc is capped.

Method:
  1. Load Gemma's per-query predictions from /tmp/gemma-acc.json (already
     cached by Phase 1.1).
  2. For each test+dev query, force the alpha to the per-category default
     for the GEMMA-predicted category via `--splade-alpha` flag.
  3. Compare R@5 vs the same harness with classifier-routed alpha
     (production default).

Run:
  python3 evals/rerank_ab_oracle_eval.py --save /tmp/oracle.json
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
QUERIES_DIR = REPO / "evals" / "queries"

# Per-category alpha defaults from src/search/router.rs (v1.28.3 R@5-tuned).
# MUST stay in sync with the Rust enum's default_alpha values.
ALPHA_DEFAULTS = {
    "identifier_lookup": 1.00,
    "structural_search": 0.90,
    "structural": 0.90,  # alias
    "behavioral_search": 0.80,  # v1.28.3
    "behavioral": 0.80,  # alias
    "conceptual_search": 0.70,
    "conceptual": 0.70,  # alias
    "multi_step": 0.10,  # v1.28.3
    "negation": 0.80,
    "type_filtered": 1.00,
    "cross_language": 0.10,
    "unknown": 1.00,
}


def gold_key(g):
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_at_k(gold, results, k):
    target = gold_key(gold)
    for i, r in enumerate(results[:k]):
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return i + 1
    return None


def run_batch_with_per_query_alpha(rows, alphas, limit=20):
    """Run cqs batch with --splade-alpha forced per query.

    `rows[i]['query']` and `alphas[i]` must align.
    """
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/oracle.stderr", "ab"),
        text=True, bufsize=1, env=env,
    )
    out = []
    t0 = time.monotonic()
    try:
        for i, (row, alpha) in enumerate(zip(rows, alphas)):
            cmd = (f"search {shlex.quote(row['query'])} --limit {limit} "
                   f"--splade --splade-alpha {alpha}")
            try:
                proc.stdin.write(cmd + "\n")
                proc.stdin.flush()
            except (BrokenPipeError, OSError):
                break
            line = proc.stdout.readline()
            if not line:
                break
            try:
                envelope = json.loads(line)
                payload = envelope.get("data") if isinstance(envelope.get("data"), dict) else envelope
                out.append(payload.get("results", []) if payload else [])
            except json.JSONDecodeError:
                out.append([])
            if (i + 1) % 25 == 0 or i + 1 == len(rows):
                rate = (i + 1) / max(time.monotonic() - t0, 0.01)
                print(f"  {i+1}/{len(rows)} ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close(); proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def recall(rows, results, k_list=(1, 5, 20)):
    counts = {f"r{k}": 0 for k in k_list}
    by_cat = defaultdict(lambda: {f"r{k}": 0 for k in k_list} | {"n": 0})
    for row, results_i in zip(rows, results):
        gold = row.get("gold_chunk") or {}
        cat = row.get("category", "unknown")
        by_cat[cat]["n"] += 1
        for k in k_list:
            if match_at_k(gold, results_i, k) is not None:
                counts[f"r{k}"] += 1
                by_cat[cat][f"r{k}"] += 1
    counts["n"] = len(rows)
    return counts, dict(by_cat)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--gemma-cache", default="/tmp/gemma-acc.json",
                    help="Phase 1.1 Gemma predictions (per-query)")
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--save", type=Path)
    args = ap.parse_args()

    gemma_data = json.loads(Path(args.gemma_cache).read_text())
    # gemma_data['predictions'] is list of {split, query, true, predicted, correct}
    gemma_preds = {p["query"]: p["predicted"] for p in gemma_data["predictions"]}

    report = {"splits": {}}
    for split in ("test", "dev"):
        src = QUERIES_DIR / f"v3_{split}.v2.json"
        rows = json.loads(src.read_text())["queries"]
        # Filter to only rows where we have a Gemma prediction
        rows = [r for r in rows if r["query"] in gemma_preds]
        print(f"[eval] {split}: {len(rows)} queries with Gemma predictions",
              file=sys.stderr)

        # Cell A: alpha forced via Gemma's prediction (oracle-router)
        gemma_alphas = []
        gemma_categories = []
        for r in rows:
            cat = gemma_preds[r["query"]]
            alpha = ALPHA_DEFAULTS.get(cat, 1.00)
            gemma_alphas.append(alpha)
            gemma_categories.append(cat)

        print(f"  oracle alpha distribution:",
              {c: gemma_categories.count(c) for c in set(gemma_categories)},
              file=sys.stderr)

        print(f"\n[cell] oracle (Gemma-routed alpha)", file=sys.stderr)
        oracle_results = run_batch_with_per_query_alpha(rows, gemma_alphas, limit=args.limit)
        oracle, oracle_cat = recall(rows, oracle_results)

        # Cell B: alpha forced to category default per the v3 fixture's category
        # (this is what the production router would produce IF the rule+centroid
        # classifier perfectly identified the fixture category — i.e., the
        # production ceiling assuming any-classifier-perfect)
        # SKIPPED — same answer as oracle since fixture labels match Gemma 99%

        # Cell C: production routing (no --splade-alpha, daemon picks per category)
        # Done separately via rerank_ab_eval.py — quote those numbers as comparison

        report["splits"][split] = {
            "n": len(rows),
            "oracle_overall": oracle,
            "oracle_by_cat": oracle_cat,
        }

    print("\n" + "=" * 76)
    print("Oracle-router R@5 (Gemma's predictions force the alpha)")
    print("=" * 76)
    print(f"\n  Production R@5 baseline (from rerank_ab_eval, classifier ON):")
    print(f"    test: 63.3% R@5, 83.5% R@20")
    print(f"    dev:  76.1% R@5, 88.1% R@20")
    print(f"\n  Oracle R@5 (Gemma-perfect routing):")
    for split in ("test", "dev"):
        s = report["splits"][split]
        n = s["n"]
        o = s["oracle_overall"]
        print(f"    {split:5} R@1={100*o['r1']/n:5.1f}%  R@5={100*o['r5']/n:5.1f}%  R@20={100*o['r20']/n:5.1f}%  (N={n})")

    print(f"\n  Per-category R@5:")
    for split in ("test", "dev"):
        s = report["splits"][split]
        print(f"\n  --- {split} ---")
        for cat, c in sorted(s["oracle_by_cat"].items()):
            if c["n"] == 0:
                continue
            print(f"    {cat:<22} N={c['n']:<3} R@5={100*c['r5']/c['n']:5.1f}%")

    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


if __name__ == "__main__":
    main()
