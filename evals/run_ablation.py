#!/usr/bin/env python3
"""Run the dense × sparse ablation matrix on the v2 eval query set.

Cells: {BGE-large, E5-LoRA v9-200k} × {no-sparse, SPLADE}
Reports R@1, R@5, R@20 per cell and per category.
"""

import json
import subprocess
import sys
import time
from collections import defaultdict

QUERY_SET = "evals/queries/v2_300q.json"

def run_search(query, n=20, splade=False):
    """Run a cqs search and return list of result names."""
    cmd = ["cqs", query, "--json", "-n", str(n)]
    if splade:
        cmd.append("--splade")
    result = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60)
    try:
        data = json.loads(result.stdout)
        return [(r["name"], r.get("score", 0)) for r in data.get("results", [])]
    except Exception:
        return []

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

        rank = None
        for i, name in enumerate(names):
            if name == expected or name in acceptable:
                rank = i + 1
                break

        if rank:
            found += 1
            by_cat[cat]["r20"] += 1
        if rank == 1:
            r1 += 1
            by_cat[cat]["r1"] += 1
        if rank and rank <= 5:
            r5 += 1
            by_cat[cat]["r5"] += 1

        if total % 10 == 0:
            print(f"  {label}: {total}/{len(queries)} queries...", file=sys.stderr)

    return {
        "r1": r1, "r5": r5, "r20": found, "n": total,
        "by_cat": dict(by_cat),
    }

def reindex(model=None):
    """Reindex with optional model override. Returns (duration_secs, cache_stats)."""
    cmd = ["cqs", "index"]
    env = None
    if model:
        import os
        env = os.environ.copy()
        env["CQS_EMBEDDING_MODEL"] = model

    # Get cache stats before
    before = subprocess.run(["cqs", "cache", "stats", "--json"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        before_stats = json.loads(before.stdout)
    except:
        before_stats = {}

    start = time.time()
    result = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, timeout=600)
    duration = time.time() - start

    # Get cache stats after
    after = subprocess.run(["cqs", "cache", "stats", "--json"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        after_stats = json.loads(after.stdout)
    except:
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
    with open(QUERY_SET) as f:
        qs = json.load(f)

    train_queries = [q for q in qs["queries"] if q["split"] == "train"]
    print(f"Loaded {len(train_queries)} train queries", file=sys.stderr)

    all_results = {}
    index_times = {}

    # ── Cell 1: BGE-large, no SPLADE (current index) ─────────────────
    print("\n=== Cell 1: BGE-large, no SPLADE ===", file=sys.stderr)
    results = evaluate(train_queries, splade=False, label="BGE-large")
    all_results["BGE-large"] = results

    # ── Cell 2: BGE-large + SPLADE ────────────────────────────────────
    print("\n=== Cell 2: BGE-large + SPLADE ===", file=sys.stderr)
    results = evaluate(train_queries, splade=True, label="BGE-large+SPLADE")
    all_results["BGE-large + SPLADE"] = results

    # ── Reindex with E5-LoRA v9-200k ─────────────────────────────────
    print("\n=== Reindexing with E5-LoRA v9-200k ===", file=sys.stderr)
    duration, before, after = reindex(model="v9-200k")
    index_times["E5-LoRA reindex"] = duration
    print(f"  Reindex took {duration:.1f}s", file=sys.stderr)
    print(f"  Cache: {before.get('total_entries', '?')} → {after.get('total_entries', '?')} entries", file=sys.stderr)

    # ── Cell 3: E5-LoRA v9-200k, no SPLADE ───────────────────────────
    print("\n=== Cell 3: E5-LoRA v9-200k, no SPLADE ===", file=sys.stderr)
    results = evaluate(train_queries, splade=False, label="E5-LoRA-v9-200k")
    all_results["E5-LoRA v9-200k"] = results

    # ── Cell 4: E5-LoRA v9-200k + SPLADE ─────────────────────────────
    print("\n=== Cell 4: E5-LoRA v9-200k + SPLADE ===", file=sys.stderr)
    results = evaluate(train_queries, splade=True, label="E5-LoRA+SPLADE")
    all_results["E5-LoRA v9-200k + SPLADE"] = results

    # ── Reindex back to BGE-large (cache test) ────────────────────────
    print("\n=== Reindexing back to BGE-large (cache test) ===", file=sys.stderr)
    duration, before, after = reindex()  # default model = BGE-large
    index_times["BGE-large reindex (cached)"] = duration
    print(f"  Reindex took {duration:.1f}s", file=sys.stderr)
    print(f"  Cache: {before.get('total_entries', '?')} → {after.get('total_entries', '?')} entries", file=sys.stderr)

    # ── Cell 5: BGE-large again (verify no regression) ────────────────
    print("\n=== Cell 5: BGE-large again (verification) ===", file=sys.stderr)
    results = evaluate(train_queries, splade=False, label="BGE-large (verify)")
    all_results["BGE-large (verify)"] = results

    # ── Report ────────────────────────────────────────────────────────
    print("\n" + "=" * 70)
    print(f"Dense × Sparse Ablation Matrix ({len(train_queries)} train queries, v2 eval)")
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
    except:
        print("  (unavailable)")


if __name__ == "__main__":
    main()
