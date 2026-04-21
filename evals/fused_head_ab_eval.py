#!/usr/bin/env python3
"""3-cell A/B for the fused alpha + classifier head.

Cells:
  1. Baseline: rule + centroid (v1.28.3 production). Both heads OFF.
  2. Distilled head (Phase 1.4b): CQS_DISTILLED_CLASSIFIER=1.
  3. Fused head: CQS_FUSED_HEAD=1.

All cells use the daemon — restart between cells so OnceLock picks up
the new env. Cleanup unlinks the systemd override on exit.

Run:
    python3 evals/fused_head_ab_eval.py --save /tmp/fused-ab.json
"""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

QUERIES_DIR = Path(__file__).resolve().parent / "queries"
OVERRIDE_PATH = Path.home() / ".config/systemd/user/cqs-watch.service.d/ab-override.conf"


def gold_key(g):
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def match_at_k(gold, results, k):
    target = gold_key(gold)
    for i, r in enumerate(results[:k]):
        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
            return i + 1
    return None


def restart_daemon_with_env(distilled: bool = False, fused: bool = False):
    """Write systemd override + restart daemon with the requested env."""
    OVERRIDE_PATH.parent.mkdir(parents=True, exist_ok=True)
    lines = ["[Service]", 'Environment="CQS_CENTROID_CLASSIFIER=1"']
    if distilled:
        lines.append('Environment="CQS_DISTILLED_CLASSIFIER=1"')
    if fused:
        lines.append('Environment="CQS_FUSED_HEAD=1"')
    OVERRIDE_PATH.write_text("\n".join(lines) + "\n")
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "--user", "restart", "cqs-watch"], check=True)
    time.sleep(3)


def cleanup_overrides():
    if OVERRIDE_PATH.exists():
        OVERRIDE_PATH.unlink()
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=False)
    subprocess.run(["systemctl", "--user", "restart", "cqs-watch"], check=False)


def run_batch(queries: list[str], limit: int = 20) -> list[list[dict]]:
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/fused-ab-daemon.stderr", "ab"),
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
                print(f"    {i+1}/{len(queries)} ({rate:.1f} qps)",
                      file=sys.stderr, flush=True)
    finally:
        try:
            proc.stdin.close(); proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def eval_split(split: str, limit: int, prefix: str = "v3"):
    src = QUERIES_DIR / f"{prefix}_{split}.v2.json"
    rows = json.loads(src.read_text())["queries"]
    queries = [r["query"] for r in rows]
    print(f"\n=== split {split} ({len(rows)} queries) ===", file=sys.stderr)

    cells = {}
    for label, distilled, fused in [
        ("baseline", False, False),
        ("distilled", True, False),
        ("fused", False, True),
    ]:
        print(f"\n[restart] daemon for cell '{label}' "
              f"(distilled={distilled}, fused={fused})", file=sys.stderr)
        restart_daemon_with_env(distilled=distilled, fused=fused)
        print(f"[cell] {label}", file=sys.stderr)
        cell_results = run_batch(queries, limit=limit)
        overall = {"r1": 0, "r5": 0, "r20": 0, "n": len(rows)}
        by_cat = defaultdict(lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0})
        for row, res in zip(rows, cell_results):
            gold = row.get("gold_chunk") or {}
            cat = row.get("category", "unknown")
            by_cat[cat]["n"] += 1
            for k in (1, 5, 20):
                if match_at_k(gold, res, k) is not None:
                    overall[f"r{k}"] += 1
                    by_cat[cat][f"r{k}"] += 1
        cells[label] = {"overall": overall, "by_cat": dict(by_cat)}
    return {"n": len(rows), **cells}


def print_table(report):
    print("\n" + "=" * 80)
    print("Fused Head 3-Cell A/B")
    print("=" * 80)
    for split in ("test", "dev"):
        s = report["splits"][split]
        n = s["n"]
        print(f"\n--- {split} (N={n}) ---")
        print(f"  {'config':<12} {'R@1':>7} {'R@5':>7} {'R@20':>7}")
        bl = s["baseline"]["overall"]
        for label in ("baseline", "distilled", "fused"):
            c = s[label]["overall"]
            r1 = 100 * c["r1"] / n
            r5 = 100 * c["r5"] / n
            r20 = 100 * c["r20"] / n
            if label == "baseline":
                print(f"  {label:<12} {r1:6.1f}% {r5:6.1f}% {r20:6.1f}%")
            else:
                d1 = 100 * (c["r1"] - bl["r1"]) / n
                d5 = 100 * (c["r5"] - bl["r5"]) / n
                d20 = 100 * (c["r20"] - bl["r20"]) / n
                print(f"  {label:<12} {r1:6.1f}% {r5:6.1f}% {r20:6.1f}%   "
                      f"Δ={d1:+5.1f}/{d5:+5.1f}/{d20:+5.1f}")

        print(f"\n  per-category R@5:")
        cats = sorted(set(s["baseline"]["by_cat"].keys()))
        for cat in cats:
            bl_c = s["baseline"]["by_cat"].get(cat, {"r5": 0, "n": 0})
            ds_c = s["distilled"]["by_cat"].get(cat, {"r5": 0, "n": 0})
            fs_c = s["fused"]["by_cat"].get(cat, {"r5": 0, "n": 0})
            cn = max(bl_c["n"], ds_c["n"], fs_c["n"], 1)
            bl_r5 = 100 * bl_c["r5"] / cn
            ds_r5 = 100 * ds_c["r5"] / cn
            fs_r5 = 100 * fs_c["r5"] / cn
            print(f"    {cat:<22} N={cn:<3} "
                  f"BL={bl_r5:5.1f}%  DS={ds_r5:5.1f}% (Δ{ds_r5-bl_r5:+4.1f})  "
                  f"FS={fs_r5:5.1f}% (Δ{fs_r5-bl_r5:+4.1f})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--save", type=Path)
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--prefix", default="v3",
                    help="Fixture prefix: <prefix>_test.v2.json + <prefix>_dev.v2.json (default v3)")
    args = ap.parse_args()

    try:
        report = {"splits": {}, "prefix": args.prefix}
        for split in ("test", "dev"):
            report["splits"][split] = eval_split(split, args.limit, prefix=args.prefix)
        print_table(report)
        if args.save:
            args.save.write_text(json.dumps(report, indent=2))
            print(f"\nSaved {args.save}", file=sys.stderr)
    finally:
        print("\n[cleanup] unlinking override + restarting clean", file=sys.stderr)
        cleanup_overrides()


if __name__ == "__main__":
    main()
