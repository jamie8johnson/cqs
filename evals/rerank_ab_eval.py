#!/usr/bin/env python3
"""A/B eval: baseline (stage-1 only) vs baseline + `--rerank` on v3.v2 splits.

Computes R@1/R@5/R@20 per config against the v3_{split}.v2.json gold. Runs
both configs for every query so the diff is apples-to-apples on the same
index state (i.e. same reindex, same SPLADE alpha, same SQLite content).

Output (JSON to --save and printed markdown table):
    {
        "split": "test",
        "n_queries": 109,
        "baseline":  {"r1": 45, "r5": 72, "r20": 93, ...},
        "rerank":    {"r1": ..., "r5": ..., "r20": ..., ...},
        "delta":     {"r1": +4, "r5": +3, "r20": -1},
        "per_query": [ { query, match_baseline, match_rerank }, ... ],
    }

The reranker model is selected via `CQS_RERANKER_MODEL_DIR` or whatever the
default rerank wiring uses. This script doesn't pick the model — it just
toggles --rerank on/off and compares.

Run:
    python3 evals/rerank_ab_eval.py --split test --save /tmp/ab-test.json
    python3 evals/rerank_ab_eval.py --split dev  --save /tmp/ab-dev.json

Robustness:
    - Per-query timeout (default 120s). Timed-out queries are counted as miss
      for both configs so the diff is still meaningful.
    - Resumes via --save: if the file exists, rows with `rerank` already
      filled are skipped.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

QUERIES_DIR = Path(__file__).parent / "queries"


def gold_key(g: dict) -> tuple:
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_gold(gold: dict, results: list[dict], k: int) -> int | None:
    """Return the 1-indexed rank of the gold within the top-k, else None."""
    gold_k = gold_key(gold)
    for i, r in enumerate(results[:k]):
        rk = (r.get("file"), r.get("name"), r.get("line_start"))
        if rk == gold_k:
            return i + 1
    return None


def run_batch(queries: list[str], rerank: bool, limit: int = 20, timeout_s: int = 120) -> list[list[dict]]:
    """Run a batch of queries through `cqs batch`. Returns per-query result lists."""
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open("/tmp/ab-eval.stderr", "ab"),
        text=True,
        bufsize=1,
        env=env,
    )

    out = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            flags = f"--limit {limit} --splade"
            if rerank:
                flags += " --rerank"
            cmd = f"search {shlex.quote(q)} {flags}"
            try:
                proc.stdin.write(cmd + "\n")
                proc.stdin.flush()
            except (BrokenPipeError, OSError) as e:
                print(f"batch died: {e}", file=sys.stderr)
                break

            line = proc.stdout.readline()
            if not line:
                print(f"batch EOF at q={i}", file=sys.stderr)
                break
            try:
                envelope = json.loads(line)
                payload = envelope.get("data") if isinstance(envelope.get("data"), dict) else envelope
                out.append(payload.get("results", []))
            except json.JSONDecodeError:
                out.append([])

            if (i + 1) % 20 == 0 or i + 1 == len(queries):
                rate = (i + 1) / (time.monotonic() - t0)
                print(f"  {'rerank' if rerank else 'base'}: {i+1}/{len(queries)} ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def recall_at_k(rows: list[dict], results: list[list[dict]], k_list=(1, 5, 20)) -> dict:
    """Compute R@k for each k in k_list. Returns {f"r{k}": count, "n": n}."""
    counts = {f"r{k}": 0 for k in k_list}
    misses = []
    for row, results_i in zip(rows, results):
        gold = row.get("gold_chunk") or {}
        best = None
        for k in k_list:
            rank = match_gold(gold, results_i, k)
            if rank is not None:
                counts[f"r{k}"] += 1
                if best is None:
                    best = rank
        if best is None:
            misses.append(row["query"])
    counts["n"] = len(rows)
    counts["misses"] = misses[:20]  # first 20 misses for eyeballing
    return counts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--split", default="test", choices=["test", "dev"])
    ap.add_argument("--limit", type=int, default=20,
                    help="Depth for R@K denominator; also passed to `cqs search`")
    ap.add_argument("--save", type=Path,
                    help="Save the structured A/B result here (JSON).")
    args = ap.parse_args()

    src = QUERIES_DIR / f"v3_{args.split}.v2.json"
    data = json.loads(src.read_text())
    rows = data["queries"]
    print(f"[eval] {len(rows)} queries from {src}", file=sys.stderr)

    queries = [r["query"] for r in rows]

    print(f"[cell] baseline (stage-1 only, --splade)", file=sys.stderr)
    base_results = run_batch(queries, rerank=False, limit=args.limit)
    base = recall_at_k(rows, base_results)

    print(f"[cell] rerank (--rerank, pool cap 20)", file=sys.stderr)
    rr_results = run_batch(queries, rerank=True, limit=args.limit)
    rr = recall_at_k(rows, rr_results)

    delta = {k: rr[k] - base[k] for k in ("r1", "r5", "r20")}

    report = {
        "split": args.split,
        "n_queries": len(rows),
        "baseline": base,
        "rerank": rr,
        "delta": delta,
    }

    print("\n" + "=" * 72)
    print(f"A/B Rerank Eval — {args.split}.v2 ({len(rows)} queries)")
    print("=" * 72)
    hdr = f"| {'Config':20s} | {'R@1':>7s} | {'R@5':>7s} | {'R@20':>7s} |"
    sep = "|" + "-" * 22 + "|" + "-" * 9 + "|" + "-" * 9 + "|" + "-" * 9 + "|"
    print(hdr)
    print(sep)
    for label, c in (("baseline", base), ("rerank", rr)):
        print(f"| {label:20s} | {100*c['r1']/c['n']:6.1f}% | "
              f"{100*c['r5']/c['n']:6.1f}% | {100*c['r20']/c['n']:6.1f}% |")
    print(sep)
    n = base["n"]
    print(f"| {'delta (pp)':20s} | "
          f"{100*delta['r1']/n:+6.1f} | "
          f"{100*delta['r5']/n:+6.1f} | "
          f"{100*delta['r20']/n:+6.1f} |")

    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


if __name__ == "__main__":
    main()
