#!/usr/bin/env python3
"""Regenerate `v3_test.json` against the current index (Tier 1.1).

The R@5 audit (`docs/audit-r5-failure-modes.md`) found a 13pp gap between
strict and permissive R@5 caused entirely by v3 fixture drift: chunks
that were the original gold targets have moved (refactor → new line),
been renamed (rename refactor → new id), or were carve-outs from now-
deleted worktrees. The gap inflates the apparent miss rate without
reflecting any retrieval regression.

This script regenerates the gold for each query so future levers can be
measured against a stable baseline:

  Strategy A — strict resolves: keep gold as-is.
  Strategy B — basename-equivalent resolves (origin moved, same file
               name + function name + line_start): update origin only.
  Strategy C — name-only resolves (function still exists somewhere with
               the same name and chunk_type): update origin AND
               line_start to the current location, mark migration in
               metadata so we can audit later.
  Strategy D — no match in current index: mark `unresolved` with a
               diagnostic. Do NOT silently drop. (User can decide whether
               to re-judge via Gemma, or accept they're now genuinely
               missing.)

Output:
  evals/queries/v3_test.v2.json    — regenerated fixture (drop-in
                                     replacement format)
  evals/queries/v3_test.diff.json  — per-query change record (audit log)

Run:
  python3 evals/regenerate_v3_test.py
  # or A/B against a different split:
  python3 evals/regenerate_v3_test.py --split dev

The script is conservative: it never invents gold for queries that
genuinely have no match — those go to the unresolved bucket so a human
(or Gemma) can decide. It also never fabricates `judges` blocks or
re-runs the dual-judge pipeline; it only updates the gold pointer.
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


def load_split(split: str) -> dict:
    path = QUERIES_DIR / f"v3_{split}.json"
    return json.loads(path.read_text())


def gold_key(g: dict) -> tuple:
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def basename_of(path: str | None) -> str:
    if not path:
        return ""
    return path.replace("\\", "/").split("/")[-1]


def find_gold_in_results(gold: dict, results: list[dict]) -> tuple[str, dict | None]:
    """Returns (match_kind, matching_result_or_None).

    match_kind ∈ {strict, basename, name, none}.
    """
    target = gold_key(gold)
    target_name = gold.get("name")
    target_basename = basename_of(gold.get("origin"))
    target_lstart = gold.get("line_start")
    target_chunk_type = gold.get("chunk_type")

    # Strict: exact (origin, name, line_start)
    for r in results:
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return "strict", r

    # Basename: same file basename + name + line_start (origin path drift only)
    for r in results:
        if (
            basename_of(r.get("file")) == target_basename
            and r.get("name") == target_name
            and r.get("line_start") == target_lstart
        ):
            return "basename", r

    # Name + chunk_type: same function name, same kind. Higher confidence
    # than name-only because we don't accidentally collapse a struct named
    # `Result` onto a function named `Result`.
    if target_name:
        for r in results:
            if r.get("name") == target_name and r.get("chunk_type") == target_chunk_type:
                return "name", r
        # Fallback: name only (lower confidence but still recoverable signal)
        for r in results:
            if r.get("name") == target_name:
                return "name", r

    return "none", None


def run_search(query: str, k: int = 50) -> list[dict]:
    """Run a single cqs search via the daemon. Returns results list."""
    cmd = ["cqs", "--json", "-n", str(k), "--", query]
    try:
        r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                           text=True, timeout=60)
        out = json.loads(r.stdout)
        return out.get("results", [])
    except (subprocess.TimeoutExpired, json.JSONDecodeError) as e:
        print(f"  ! search failed: {e}", file=sys.stderr)
        return []


def run_batch(queries: list[str], k: int = 50) -> list[list[dict]]:
    """Batch the queries through `cqs batch` for speed (3-19ms via daemon)."""
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open("/tmp/regen.stderr", "ab"),
        text=True,
        bufsize=1,
        env=env,
    )

    results = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            cmd = f"search {shlex.quote(q)} --limit {k} --splade"
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
                out = json.loads(line)
                results.append(out.get("results", []))
            except json.JSONDecodeError:
                results.append([])

            if (i + 1) % 20 == 0 or i + 1 == len(queries):
                rate = (i + 1) / (time.monotonic() - t0)
                print(f"  {i+1}/{len(queries)} queries ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()

    return results


def updated_gold_chunk(original: dict, match: dict) -> dict:
    """Return a new gold_chunk dict with origin/line_start refreshed from `match`.

    Other fields (name, chunk_type, language) are kept from the original
    when the match agrees, falling back to the match for fields the
    original lacks.
    """
    return {
        "id": match.get("id") or f"{match.get('file')}:{match.get('line_start')}:regen",
        "name": match.get("name") or original.get("name"),
        "origin": match.get("file"),
        "language": match.get("language") or original.get("language"),
        "chunk_type": match.get("chunk_type") or original.get("chunk_type"),
        "line_start": match.get("line_start"),
        "line_end": match.get("line_end"),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--split", default="test", choices=["test", "dev"])
    ap.add_argument("--k", type=int, default=50,
                    help="Top-K candidates to fetch per query. Default 50 — wider than R@20 audit so name fallback has more chances.")
    args = ap.parse_args()

    src = load_split(args.split)
    rows = src["queries"]
    print(f"Loaded {len(rows)} queries from v3_{args.split}.json", file=sys.stderr)

    queries = [r["query"] for r in rows]

    print(f"Running {len(queries)} searches (k={args.k})...", file=sys.stderr)
    all_results = run_batch(queries, k=args.k)

    if len(all_results) != len(rows):
        print(f"!! Result count mismatch: got {len(all_results)} for {len(rows)} queries",
              file=sys.stderr)
        return 1

    # Per-query resolution
    resolved_rows = []
    diff_records = []
    counts = {"strict": 0, "basename": 0, "name": 0, "none": 0}
    for q, results in zip(rows, all_results):
        gold = q.get("gold_chunk")
        if not gold:
            # No gold to begin with — keep query as-is, mark unjudged.
            resolved_rows.append(q)
            diff_records.append({
                "query": q["query"],
                "match_kind": "no_gold",
                "action": "keep_as_is",
            })
            continue

        match_kind, match = find_gold_in_results(gold, results)
        counts[match_kind] += 1

        if match_kind == "strict":
            resolved_rows.append(q)
            diff_records.append({
                "query": q["query"],
                "match_kind": "strict",
                "action": "no_change",
            })
        elif match_kind in ("basename", "name") and match is not None:
            new_gold = updated_gold_chunk(gold, match)
            new_q = {**q, "gold_chunk": new_gold}
            # Annotate metadata so we can audit later which queries were migrated
            metadata = dict(new_q.get("metadata") or {})
            metadata["regenerated_2026_04_17"] = {
                "match_kind": match_kind,
                "old_origin": gold.get("origin"),
                "old_line_start": gold.get("line_start"),
                "new_origin": new_gold.get("origin"),
                "new_line_start": new_gold.get("line_start"),
            }
            new_q["metadata"] = metadata
            resolved_rows.append(new_q)
            diff_records.append({
                "query": q["query"],
                "match_kind": match_kind,
                "action": "updated",
                "old": {"origin": gold.get("origin"), "line_start": gold.get("line_start")},
                "new": {"origin": new_gold.get("origin"), "line_start": new_gold.get("line_start")},
            })
        else:
            # No match — mark unresolved but keep query in the set.
            new_q = {**q, "_unresolved": True}
            resolved_rows.append(new_q)
            diff_records.append({
                "query": q["query"],
                "match_kind": "none",
                "action": "unresolved",
                "gold_chunk": gold,
            })

    # Compose output
    out = {**src, "queries": resolved_rows}
    out["regenerated_at"] = time.strftime("%Y-%m-%d")
    out["regenerated_against"] = f"current index (k={args.k})"
    out["regenerated_counts"] = counts

    out_path = QUERIES_DIR / f"v3_{args.split}.v2.json"
    diff_path = QUERIES_DIR / f"v3_{args.split}.diff.json"

    out_path.write_text(json.dumps(out, indent=2))
    diff_path.write_text(json.dumps({
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "source": f"v3_{args.split}.json",
        "k": args.k,
        "counts": counts,
        "records": diff_records,
    }, indent=2))

    n = len(rows)
    print(f"\n=== Regeneration summary ({args.split}) ===", file=sys.stderr)
    print(f"  Total queries:      {n}", file=sys.stderr)
    print(f"  strict (no change): {counts['strict']:>3} ({100*counts['strict']/n:.1f}%)", file=sys.stderr)
    print(f"  basename (origin):  {counts['basename']:>3} ({100*counts['basename']/n:.1f}%)", file=sys.stderr)
    print(f"  name (origin+line): {counts['name']:>3} ({100*counts['name']/n:.1f}%)", file=sys.stderr)
    print(f"  unresolved:         {counts['none']:>3} ({100*counts['none']/n:.1f}%)", file=sys.stderr)
    print(f"\nFresh fixture → {out_path}", file=sys.stderr)
    print(f"Diff record   → {diff_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
