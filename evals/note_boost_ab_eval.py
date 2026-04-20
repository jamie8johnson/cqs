#!/usr/bin/env python3
"""A/B eval: note-boost ON (factor=0.15, default) vs OFF (factor=0.0).

The note boost (`scoring.note_boost_factor` in `.cqs.toml`) multiplies a
chunk's RRF-fused score by `1.0 + sentiment * factor` when any note in
`docs/notes.toml` mentions the chunk's file path or name. We've never
isolated whether this helps or hurts retrieval — most notes are
workflow-oriented, not "this chunk answers queries like X."

Method: write a `.cqs.toml` with `note_boost_factor = 0.0` for the OFF
cell, restart the daemon to pick up the new config, run the v3.v2
fixture; then restore the default and run again. Compare R@1/R@5/R@20
on test+dev.

Note boost is computed at scoring time (see src/search/scoring/note_boost.rs)
not at index time, so the index is unchanged between cells.

Run:
    python3 evals/note_boost_ab_eval.py --save /tmp/notes-ab.json

Requires `systemctl --user` available (WSL/Linux user services). Stops/
starts cqs-watch around each config change.
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

REPO_ROOT = Path(__file__).resolve().parent.parent
CQS_TOML = REPO_ROOT / ".cqs.toml"
QUERIES_DIR = REPO_ROOT / "evals" / "queries"


def gold_key(g):
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_at_k(gold, results, k):
    target = gold_key(gold)
    for i, r in enumerate(results[:k]):
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return i + 1
    return None


def run_batch(queries, limit=20):
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/notes-ab.stderr", "ab"),
        text=True, bufsize=1, env=env,
    )
    out = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            cmd = f"search {shlex.quote(q)} --limit {limit} --splade"
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
                out.append(payload.get("results", []))
            except json.JSONDecodeError:
                out.append([])
            if (i + 1) % 25 == 0 or i + 1 == len(queries):
                rate = (i + 1) / (time.monotonic() - t0)
                print(f"  {i+1}/{len(queries)} ({rate:.1f} qps)", file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close(); proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def recall(rows, results, k_list=(1, 5, 20)):
    counts = {f"r{k}": 0 for k in k_list}
    for row, results_i in zip(rows, results):
        gold = row.get("gold_chunk") or {}
        for k in k_list:
            if match_at_k(gold, results_i, k) is not None:
                counts[f"r{k}"] += 1
    counts["n"] = len(rows)
    return counts


def write_cqs_toml(factor: float):
    """Write a .cqs.toml with the given note_boost_factor."""
    body = f"[scoring]\nnote_boost_factor = {factor}\n"
    CQS_TOML.write_text(body)


def restart_daemon():
    subprocess.run(["systemctl", "--user", "stop", "cqs-watch"], check=True)
    subprocess.run(["systemctl", "--user", "start", "cqs-watch"], check=True)
    # daemon load: BGE warm + tokenizer + HNSW mmap. Give it a moment.
    time.sleep(5)


def eval_split(split: str, limit: int):
    src = QUERIES_DIR / f"v3_{split}.v2.json"
    rows = json.loads(src.read_text())["queries"]
    print(f"[eval] {split}: {len(rows)} queries", file=sys.stderr)
    queries = [r["query"] for r in rows]
    results = run_batch(queries, limit=limit)
    return rows, recall(rows, results)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--save", type=Path)
    ap.add_argument("--cell-only", choices=["off", "on"],
                    help="Run only one cell (for re-runs without redoing the other).")
    args = ap.parse_args()

    cqs_toml_existed = CQS_TOML.exists()
    backup = CQS_TOML.read_text() if cqs_toml_existed else None

    report = {"splits": {}}
    cells = ["off", "on"] if args.cell_only is None else [args.cell_only]

    try:
        for cell in cells:
            factor = 0.0 if cell == "off" else 0.15
            print(f"\n[cell] note_boost_factor = {factor} ({cell})", file=sys.stderr)
            write_cqs_toml(factor)
            restart_daemon()
            for split in ("test", "dev"):
                _, counts = eval_split(split, args.limit)
                report["splits"].setdefault(split, {})[cell] = counts
                print(f"  {split} {cell}: R@1={100*counts['r1']/counts['n']:.1f}%  "
                      f"R@5={100*counts['r5']/counts['n']:.1f}%  "
                      f"R@20={100*counts['r20']/counts['n']:.1f}%", file=sys.stderr)
    finally:
        # Restore prior config
        if backup is not None:
            CQS_TOML.write_text(backup)
        elif CQS_TOML.exists():
            CQS_TOML.unlink()
        restart_daemon()

    print("\n" + "=" * 78)
    print("Note boost A/B (note_boost_factor: 0.0 vs 0.15)")
    print("=" * 78)
    print(f"| {'Split':6} | {'Metric':5} | {'OFF (0.0)':10} | {'ON (0.15)':10} | {'Δ (pp)':8} |")
    print(f"|{'-'*8}|{'-'*7}|{'-'*12}|{'-'*12}|{'-'*10}|")
    for split in ("test", "dev"):
        if "off" in report["splits"].get(split, {}) and "on" in report["splits"][split]:
            off = report["splits"][split]["off"]
            on = report["splits"][split]["on"]
            n = on["n"]
            for k in ("r1", "r5", "r20"):
                d = (on[k] - off[k]) / n * 100
                marker = "  " if abs(d) < 0.5 else ("↑↑" if d > 2 else "↑ " if d > 0 else "↓ " if d > -2 else "↓↓")
                print(f"| {split:6} | R@{k[1:]:3} | {100*off[k]/n:9.1f}% | {100*on[k]/n:9.1f}% | "
                      f"{d:+6.1f} {marker}|")

    if args.save:
        args.save.write_text(json.dumps(report, indent=2))
        print(f"\nSaved {args.save}", file=sys.stderr)


if __name__ == "__main__":
    main()
