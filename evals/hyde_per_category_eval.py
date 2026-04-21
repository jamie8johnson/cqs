#!/usr/bin/env python3
"""Query-time HyDE per-category eval on v3.v2.

For each query, ask Gemma 4 31B (vLLM) to generate a synthetic Rust code
chunk that would answer the query, then search using that synthetic code
as the query string. Compare R@1/R@5/R@20 vs the baseline (real query)
per category.

v2-era data (single config, all queries) suggested HyDE helps
structural / type_filtered / multi_step but hurts conceptual /
behavioral / negation. This script re-validates per-category on v3.v2 so
we can decide whether to wire per-category routing into production.

Method:
    1. For each v3_{split}.v2 query, classify (already in fixture metadata).
    2. Generate synthetic code via vLLM (cached blake3 → SQLite).
    3. Run baseline search with original query.
    4. Run HyDE search with synthetic code as query.
    5. Per-category R@K table for both.

Caveats:
    - We pass the synthetic code as a "query" to cqs, which applies the
      BGE query prefix ("Represent this sentence for searching relevant
      passages: "). Strictly we'd want the passage prefix here. The
      mismatch may cap the upside — a positive result is still meaningful;
      a negative or zero result needs the prefix-fix follow-up.
    - Daemon must NOT be using a custom reranker for this measurement
      (default off). Centroid classifier ON is the v1.28.2 default.

Run:
    python3 evals/hyde_per_category_eval.py --split test --save /tmp/hyde-test.json
    python3 evals/hyde_per_category_eval.py --split dev  --save /tmp/hyde-dev.json
"""

from __future__ import annotations

import argparse
import asyncio
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

sys.path.insert(0, str(REPO / "evals"))
from llm_client import LLMClient


HYDE_SYSTEM = (
    "You generate hypothetical code that would answer a code-search query. "
    "Given a query, write a short, realistic CODE CHUNK (function, struct, "
    "method, or constant) that, if it existed in a codebase, would be the "
    "ideal answer. Use Rust by default; switch to Python/TS/Go etc. only if "
    "the query explicitly mentions another language.\n"
    "\n"
    "Output ONLY the code, no commentary, no markdown fences, no language "
    "tag. Keep it under 25 lines. Include a short doc comment if natural. "
    "Use realistic identifier names — what a developer would actually write."
)


async def generate_hyde(client: LLMClient, query: str) -> str:
    user = f"Query: {query}\n\nCode chunk:"
    raw = await client._chat(
        HYDE_SYSTEM, user, role="hyde_v3_query_time",
        max_tokens=400, temperature=0.0,
    )
    text = raw.strip()
    # Strip accidental fences
    if text.startswith("```"):
        text = text.split("\n", 1)[1] if "\n" in text else text
        text = text.rsplit("```", 1)[0]
    return text.strip()


def gold_key(g):
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_at_k(gold, results, k):
    target = gold_key(gold)
    for i, r in enumerate(results[:k]):
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return i + 1
    return None


def run_batch_searches(queries, limit=20, splade=True):
    """Send N queries through `cqs batch`. Returns per-query result lists.

    The same `cqs batch` process handles all queries — daemon stays warm.
    """
    env = {**os.environ}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/hyde-eval.stderr", "ab"),
        text=True, bufsize=1, env=env,
    )
    out = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            cmd_parts = ["search", shlex.quote(q), f"--limit {limit}"]
            if splade:
                cmd_parts.append("--splade")
            cmd = " ".join(cmd_parts)
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
            if (i + 1) % 20 == 0 or i + 1 == len(queries):
                rate = (i + 1) / max(time.monotonic() - t0, 0.01)
                print(f"    {i+1}/{len(queries)} ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close(); proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def per_category_recall(rows, results, k_list=(1, 5, 20)):
    by_cat = defaultdict(lambda: {**{f"r{k}": 0 for k in k_list}, "n": 0})
    overall = {**{f"r{k}": 0 for k in k_list}, "n": 0}
    for row, results_i in zip(rows, results):
        gold = row.get("gold_chunk") or {}
        cat = row.get("category", "unknown")
        by_cat[cat]["n"] += 1
        overall["n"] += 1
        for k in k_list:
            if match_at_k(gold, results_i, k) is not None:
                by_cat[cat][f"r{k}"] += 1
                overall[f"r{k}"] += 1
    return overall, dict(by_cat)


async def main_async():
    ap = argparse.ArgumentParser()
    ap.add_argument("--split", default="test", choices=["test", "dev"])
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--save", type=Path)
    ap.add_argument("--concurrency", type=int, default=8,
                    help="vLLM concurrent requests for HyDE generation")
    ap.add_argument("--prefix", default="v3",
                    help="Fixture prefix: <prefix>_{split}.v2.json (default v3)")
    args = ap.parse_args()

    src = QUERIES_DIR / f"{args.prefix}_{args.split}.v2.json"
    rows = json.loads(src.read_text())["queries"]
    queries = [r["query"] for r in rows]
    print(f"[load] {len(rows)} queries from {src.name}", file=sys.stderr)

    print(f"[hyde] generating synthetic code via Gemma (concurrency={args.concurrency})",
          file=sys.stderr)
    client = LLMClient()
    sem = asyncio.Semaphore(args.concurrency)
    hyde_docs: list[str] = [""] * len(queries)

    async def gen_one(i, q):
        async with sem:
            doc = await generate_hyde(client, q)
        hyde_docs[i] = doc
        if (i + 1) % 20 == 0 or i + 1 == len(queries):
            print(f"    {i+1}/{len(queries)} HyDE docs", file=sys.stderr, flush=True)

    t0 = time.monotonic()
    await asyncio.gather(*[gen_one(i, q) for i, q in enumerate(queries)])
    await client.aclose()
    print(f"[hyde] done in {time.monotonic()-t0:.1f}s", file=sys.stderr)

    print(f"\n[search] baseline (real query, --splade)", file=sys.stderr)
    base_results = run_batch_searches(queries, args.limit, splade=True)
    base_overall, base_cat = per_category_recall(rows, base_results)

    print(f"\n[search] hyde (synthetic code as query, --splade)", file=sys.stderr)
    # Flatten newlines — cqs batch parses one command per line, so multi-line
    # synthetic code shatters into junk tokens. Replace with spaces; the BGE
    # tokenizer doesn't care about line breaks for query encoding.
    hyde_docs_flat = [d.replace("\n", " ").replace("\r", " ").strip() for d in hyde_docs]
    hyde_results = run_batch_searches(hyde_docs_flat, args.limit, splade=True)
    hyde_overall, hyde_cat = per_category_recall(rows, hyde_results)

    print("\n" + "=" * 80)
    print(f"Query-time HyDE per-category — v3_{args.split}.v2 ({len(rows)} queries)")
    print("=" * 80)
    print(f"| {'':25} | {'N':3} | {'BASE R@5':>9} | {'HYDE R@5':>9} | {'Δ R@5':>7} | {'Δ R@1':>7} | {'Δ R@20':>8} |")
    print(f"|{'-'*27}|{'-'*5}|{'-'*11}|{'-'*11}|{'-'*9}|{'-'*9}|{'-'*10}|")
    cats = sorted(set(base_cat) | set(hyde_cat))
    for cat in cats:
        b = base_cat.get(cat, {"r1": 0, "r5": 0, "r20": 0, "n": 0})
        h = hyde_cat.get(cat, {"r1": 0, "r5": 0, "r20": 0, "n": 0})
        n = max(b["n"], h["n"])
        if n == 0:
            continue
        bp = lambda k: 100 * b[k] / n
        hp = lambda k: 100 * h[k] / n
        d5 = hp("r5") - bp("r5")
        d1 = hp("r1") - bp("r1")
        d20 = hp("r20") - bp("r20")
        marker = "↑↑" if d5 > 5 else "↑ " if d5 > 1 else "↓↓" if d5 < -5 else "↓ " if d5 < -1 else "  "
        print(f"| {cat:25} | {n:3} | {bp('r5'):8.1f}% | {hp('r5'):8.1f}% | "
              f"{d5:+6.1f} {marker}| {d1:+6.1f}  | {d20:+7.1f}  |")
    n = base_overall["n"]
    print(f"|{'-'*27}|{'-'*5}|{'-'*11}|{'-'*11}|{'-'*9}|{'-'*9}|{'-'*10}|")
    bp = lambda k: 100 * base_overall[k] / n
    hp = lambda k: 100 * hyde_overall[k] / n
    print(f"| {'OVERALL':25} | {n:3} | {bp('r5'):8.1f}% | {hp('r5'):8.1f}% | "
          f"{hp('r5')-bp('r5'):+6.1f}   | {hp('r1')-bp('r1'):+6.1f}  | "
          f"{hp('r20')-bp('r20'):+7.1f}  |")

    report = {
        "split": args.split,
        "n_queries": len(rows),
        "baseline": {"overall": base_overall, "by_cat": base_cat},
        "hyde": {"overall": hyde_overall, "by_cat": hyde_cat},
        "hyde_docs_sample": [
            {"query": queries[i], "doc": hyde_docs[i][:300]}
            for i in range(min(5, len(queries)))
        ],
    }
    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


def main():
    sys.exit(asyncio.run(main_async()))


if __name__ == "__main__":
    main()
