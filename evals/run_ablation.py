#!/usr/bin/env python3
"""Run the dense × sparse ablation matrix on the v2 eval query set.

Usage:
    python3 evals/run_ablation.py                              # full 2×2 matrix
    python3 evals/run_ablation.py --config bge-large           # BGE-large only
    python3 evals/run_ablation.py --config bge-large+splade    # BGE + SPLADE only
    python3 evals/run_ablation.py --config e5-lora             # E5-LoRA only
    python3 evals/run_ablation.py --config e5-lora+splade      # E5-LoRA + SPLADE only

Available configs: bge-large, bge-large+splade, e5-lora, e5-lora+splade
Default (no --config): all four cells + verification cell.
"""

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from collections import defaultdict

QUERY_SET = "evals/queries/v2_300q.json"

VALID_CONFIGS = {
    "bge-large",
    "bge-large+splade",
    "e5-lora",
    "e5-lora+splade",
}


def parse_args():
    p = argparse.ArgumentParser(description="Run v2 eval ablation matrix")
    p.add_argument(
        "--config",
        action="append",
        dest="configs",
        choices=sorted(VALID_CONFIGS),
        help="Which configs to run (repeatable). Default: all.",
    )
    p.add_argument(
        "--split",
        default="train",
        choices=["train", "test", "all"],
        help="Query split to evaluate. Default: train.",
    )
    args = p.parse_args()
    if not args.configs:
        args.configs = sorted(VALID_CONFIGS)
    return args


# Per-query timeout. SPLADE queries pay the full SpladeIndex build cost on
# every invocation (load all sparse rows → HashMap → inverted index). With
# SPLADE-Code 0.6B at threshold 1.6 (7.58M rows), this is ~45s per query.
# Non-SPLADE queries settle around 7s. 300s leaves headroom for worst-case
# queries without letting genuine hangs wedge the eval.
CQS_TIMEOUT_SECS = int(os.environ.get("CQS_EVAL_TIMEOUT_SECS", "300"))


class BatchRunner:
    """Persistent cqs batch process. One ONNX load, all queries streamed."""

    def __init__(self):
        self.proc = subprocess.Popen(
            ["cqs", "batch"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    def search(self, query, n=20, splade=False):
        if self.proc.poll() is not None:
            return []
        cmd = f'search {shlex.quote(query)} --limit {n}'
        if splade:
            cmd += " --splade"
        try:
            self.proc.stdin.write(cmd + "\n")
            self.proc.stdin.flush()
            line = self.proc.stdout.readline()
            if not line:
                return []
            data = json.loads(line)
            return [(r["name"], r.get("score", 0)) for r in data.get("results", [])]
        except Exception:
            return []

    def close(self):
        if self.proc.poll() is None:
            try:
                self.proc.stdin.write("quit\n")
                self.proc.stdin.flush()
                self.proc.wait(timeout=5)
            except Exception:
                self.proc.kill()


_batch_runner = None


def get_batch_runner():
    global _batch_runner
    if _batch_runner is None or _batch_runner.proc.poll() is not None:
        _batch_runner = BatchRunner()
    return _batch_runner


def run_search(query, n=20, splade=False):
    """Run a cqs search via batch mode. Single process, env vars work."""
    return get_batch_runner().search(query, n=n, splade=splade)

def evaluate(queries, splade=False, label=""):
    """Evaluate queries and return per-query results."""
    r1 = r5 = found = total = 0
    by_cat = defaultdict(lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0})

    for q in queries:
        total += 1
        cat = q["category"]
        by_cat[cat]["n"] += 1

        results = run_search(q["query"], n=20, splade=splade)
        names = [r[0] for r in results]

        expected = q["primary_answer"]["name"]
        acceptable = [a["name"] for a in q.get("acceptable_answers", [])]
        all_valid = {expected} | set(acceptable)

        hit_at = None
        for i, name in enumerate(names):
            if name in all_valid:
                hit_at = i + 1
                break

        if hit_at is not None:
            found += 1
            by_cat[cat]["r20"] += 1
            if hit_at <= 5:
                r5 += 1
                by_cat[cat]["r5"] += 1
            if hit_at <= 1:
                r1 += 1
                by_cat[cat]["r1"] += 1

        if total % 10 == 0:
            print(f"  {label}: {total}/{len(queries)} queries...", file=sys.stderr)

    return {
        "r1": r1, "r5": r5, "r20": found, "n": total,
        "by_cat": dict(by_cat),
    }

def reindex(model=None):
    """Reindex with optional model override. Returns (duration_secs, before_stats, after_stats)."""
    cmd = ["cqs", "index"]
    env = None
    if model:
        env = os.environ.copy()
        env["CQS_EMBEDDING_MODEL"] = model

    # Get cache stats before
    before = subprocess.run(["cqs", "cache", "stats", "--json"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        before_stats = json.loads(before.stdout)
    except Exception:
        before_stats = {}

    start = time.time()
    result = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, timeout=600)
    duration = time.time() - start

    # Get cache stats after
    after = subprocess.run(["cqs", "cache", "stats", "--json"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        after_stats = json.loads(after.stdout)
    except Exception:
        after_stats = {}

    return duration, before_stats, after_stats


def format_results(results, label):
    """Format results as a table row."""
    r1_pct = results["r1"] / results["n"] * 100
    r5_pct = results["r5"] / results["n"] * 100
    r20_pct = results["r20"] / results["n"] * 100
    return f"| {label:35s} | {r1_pct:5.1f}% | {r5_pct:5.1f}% | {r20_pct:5.1f}% | {results['n']:3d} |"

def format_category_table(all_results):
    """Format per-category breakdown."""
    cats = sorted(set(
        cat for r in all_results.values() for cat in r["by_cat"]
    ))
    lines = []
    lines.append(f"| {'Config':35s} | {'Category':20s} | {'R@1':>6s} | {'R@5':>6s} |  {'N':>3s} |")
    lines.append(f"|{'-'*37}|{'-'*22}|{'-'*8}|{'-'*8}|{'-'*6}|")
    for label, results in all_results.items():
        for cat in cats:
            c = results["by_cat"].get(cat, {"r1": 0, "r5": 0, "n": 0})
            if c["n"] == 0:
                continue
            r1 = c["r1"] / c["n"] * 100
            r5 = c["r5"] / c["n"] * 100
            lines.append(f"| {label:35s} | {cat:20s} | {r1:5.1f}% | {r5:5.1f}% | {c['n']:4d} |")
    return "\n".join(lines)


def main():
    args = parse_args()

    with open(QUERY_SET) as f:
        qs = json.load(f)

    if args.split == "all":
        queries = qs["queries"]
    else:
        queries = [q for q in qs["queries"] if q["split"] == args.split]
    print(f"Loaded {len(queries)} {args.split} queries", file=sys.stderr)
    print(f"Configs: {', '.join(args.configs)}", file=sys.stderr)

    all_results = {}
    index_times = {}
    needs_e5 = any("e5" in c for c in args.configs)
    needs_bge = any("bge" in c for c in args.configs)
    current_model = "bge-large"  # assume BGE-large is currently indexed

    def run_cell(label, splade, results_dict):
        """Run one eval cell and print results immediately."""
        print(f"\n=== {label} ===", file=sys.stderr)
        results = evaluate(queries, splade=splade, label=label)
        results_dict[label] = results
        # Print metrics immediately
        r1 = results["r1"] / results["n"] * 100
        r5 = results["r5"] / results["n"] * 100
        r20 = results["r20"] / results["n"] * 100
        print(f"  → R@1={r1:.1f}%  R@5={r5:.1f}%  R@20={r20:.1f}%  (N={results['n']})", file=sys.stderr)
        # Per-category summary for top categories
        cats_sorted = sorted(results["by_cat"].items(), key=lambda x: -x[1]["n"])
        for cat, c in cats_sorted[:5]:
            if c["n"] > 0:
                cr1 = c["r1"] / c["n"] * 100
                print(f"     {cat:25s} R@1={cr1:5.1f}%  (N={c['n']})", file=sys.stderr)
        return results

    # ── BGE-large cells ──────────────────────────────────────────────
    if "bge-large" in args.configs:
        run_cell("BGE-large", splade=False, results_dict=all_results)

    if "bge-large+splade" in args.configs:
        run_cell("BGE-large + SPLADE", splade=True, results_dict=all_results)

    # ── E5-LoRA cells (requires reindex) ─────────────────────────────
    if needs_e5:
        print("\n=== Reindexing with E5-LoRA v9-200k ===", file=sys.stderr)
        duration, before, after = reindex(model="v9-200k")
        index_times["E5-LoRA reindex"] = duration
        current_model = "e5-lora"
        print(f"  Reindex took {duration:.1f}s", file=sys.stderr)
        print(f"  Cache: {before.get('total_entries', '?')} → {after.get('total_entries', '?')} entries", file=sys.stderr)

        if "e5-lora" in args.configs:
            run_cell("E5-LoRA v9-200k", splade=False, results_dict=all_results)

        if "e5-lora+splade" in args.configs:
            run_cell("E5-LoRA v9-200k + SPLADE", splade=True, results_dict=all_results)

    # ── Restore BGE-large if we switched ─────────────────────────────
    if current_model != "bge-large":
        print("\n=== Restoring BGE-large index ===", file=sys.stderr)
        duration, before, after = reindex()
        index_times["BGE-large restore"] = duration
        print(f"  Reindex took {duration:.1f}s", file=sys.stderr)

    # ── Report ────────────────────────────────────────────────────────
    print("\n" + "=" * 70)
    print(f"Dense × Sparse Ablation ({len(queries)} {args.split} queries, v2 eval)")
    print("=" * 70)
    print()
    print(f"| {'Config':35s} | {'R@1':>6s} | {'R@5':>6s} | {'R@20':>6s} | {'N':>3s} |")
    print(f"|{'-'*37}|{'-'*8}|{'-'*8}|{'-'*8}|{'-'*6}|")
    for label, results in all_results.items():
        print(format_results(results, label))

    print()
    print("Per-category breakdown:")
    print()
    print(format_category_table(all_results))

    if index_times:
        print()
        print("Index times:")
        for label, t in index_times.items():
            print(f"  {label}: {t:.1f}s")

    print()
    print("Cache stats:")
    cache = subprocess.run(["cqs", "cache", "stats", "--json"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        cs = json.loads(cache.stdout)
        print(f"  Entries: {cs['total_entries']}, Size: {cs['total_size_mb']} MB, Models: {cs['unique_models']}")
    except Exception:
        print("  (unavailable)")


if __name__ == "__main__":
    try:
        main()
    finally:
        if _batch_runner is not None:
            _batch_runner.close()
