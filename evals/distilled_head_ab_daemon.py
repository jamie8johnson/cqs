#!/usr/bin/env python3
"""Distilled head A/B via daemon mode (~1 min total instead of ~80 min CLI).

OnceLock caches the head load decision per daemon process, so we restart
the daemon between cells. Both cells use centroid ON.

Run:
    python3 evals/distilled_head_ab_daemon.py --save /tmp/distilled-ab-1.4b.json
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

QUERIES_DIR = Path(__file__).resolve().parent / "queries"


def gold_key(g):
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_at_k(gold, results, k):
    target = gold_key(gold)
    for i, r in enumerate(results[:k]):
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return i + 1
    return None


def restart_daemon_with_env(head_on: bool):
    """Restart cqs-watch with the requested env so OnceLock picks up the
    distilled head decision fresh."""
    # Render the env into a unit-style override so systemctl actually loads it.
    env_drop = Path("/tmp/cqs-watch-override.conf")
    env_drop.write_text(
        "[Service]\n"
        "Environment=\"CQS_CENTROID_CLASSIFIER=1\"\n"
        f"Environment=\"CQS_DISTILLED_CLASSIFIER={'1' if head_on else '0'}\"\n"
    )
    drop_dir = Path.home() / ".config/systemd/user/cqs-watch.service.d"
    drop_dir.mkdir(parents=True, exist_ok=True)
    (drop_dir / "ab-override.conf").write_text(env_drop.read_text())
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "--user", "restart", "cqs-watch"], check=True)
    time.sleep(3)  # give the daemon time to load model + warm sockets


def cleanup_overrides():
    drop = Path.home() / ".config/systemd/user/cqs-watch.service.d/ab-override.conf"
    if drop.exists():
        drop.unlink()
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=False)
    subprocess.run(["systemctl", "--user", "restart", "cqs-watch"], check=False)


def run_batch(queries: list[str], limit: int = 20) -> list[list[dict]]:
    """Run queries through `cqs batch` (uses daemon)."""
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/distilled-ab-daemon.stderr", "ab"),
        text=True, bufsize=1,
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
                out.append(payload.get("results", []) if payload else [])
            except json.JSONDecodeError:
                out.append([])
            if (i + 1) % 25 == 0 or i + 1 == len(queries):
                rate = (i + 1) / max(time.monotonic() - t0, 0.01)
                print(f"  {i+1}/{len(queries)} ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close(); proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def eval_split(split: str, limit: int):
    src = QUERIES_DIR / f"v3_{split}.v2.json"
    rows = json.loads(src.read_text())["queries"]
    print(f"\n=== split {split} ({len(rows)} queries) ===", file=sys.stderr)
    queries = [r["query"] for r in rows]

    results = {}
    for label, head_on in [("OFF", False), ("ON", True)]:
        print(f"\n[restart] daemon with CQS_DISTILLED_CLASSIFIER={'1' if head_on else '0'}",
              file=sys.stderr)
        restart_daemon_with_env(head_on)
        print(f"[cell] head {label}", file=sys.stderr)
        cell_results = run_batch(queries, limit=limit)
        overall = {"r1": 0, "r5": 0, "r20": 0, "n": len(rows)}
        by_cat = defaultdict(lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0})
        for row, results_i in zip(rows, cell_results):
            gold = row.get("gold_chunk") or {}
            cat = row.get("category", "unknown")
            by_cat[cat]["n"] += 1
            for k in (1, 5, 20):
                if match_at_k(gold, results_i, k) is not None:
                    overall[f"r{k}"] += 1
                    by_cat[cat][f"r{k}"] += 1
        results[label] = {"overall": overall, "by_cat": dict(by_cat)}
    return {"n": len(rows), **{k.lower(): v for k, v in results.items()}}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--save", type=Path)
    ap.add_argument("--limit", type=int, default=20)
    args = ap.parse_args()

    try:
        report = {"splits": {}}
        for split in ("test", "dev"):
            report["splits"][split] = eval_split(split, args.limit)

        print("\n" + "=" * 76)
        print("Distilled Head A/B (centroid ON in both cells)")
        print("=" * 76)
        for split in ("test", "dev"):
            s = report["splits"][split]
            n = s["n"]
            off, on = s["off"]["overall"], s["on"]["overall"]
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
    finally:
        print("\n[cleanup] removing daemon override + restarting clean", file=sys.stderr)
        cleanup_overrides()


if __name__ == "__main__":
    main()
