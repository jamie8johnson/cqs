#!/usr/bin/env python3
"""Per-category SPLADE alpha sweep on v3 train split (resumable).

For each category (including Unknown), sweep α from 0.00 to 1.00 in 0.05
steps, measure R@1 on queries of that category. Pick the α that maximizes
R@1. Report new optima alongside v1.26.0 deployed defaults.

Uses `cqs batch` with `--splade-alpha X` forced per line (bypasses the
per-category router so we measure the pure effect of α on each subset).

Observability:
  - Progress log every N pairs (configurable via --progress-every)
  - Category completion summary with best α + Δ vs v1.26.0
  - Final per-category table

Robustness:
  - Main loop wraps each query in try/except; one failure doesn't abort
  - stdout EOF → save checkpoint, exit nonzero (caller can re-run)
  - Any JSON parse error counted but doesn't abort

Resumability:
  - Checkpoint after each (category, α) pair completes
  - On restart, loads checkpoint and skips already-done pairs
  - Pair granularity: worst case we redo ~50 queries (one α-slice of one category)

Outputs:
    evals/queries/v3_alpha_sweep.checkpoint.json  — partial, restart-safe
    evals/queries/v3_alpha_sweep.json             — final, written at end
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
import traceback
from collections import defaultdict
from pathlib import Path

QUERIES_DIR = Path(__file__).parent / "queries"
TRAIN_PATH = QUERIES_DIR / "v3_train.json"
CHECKPOINT_PATH = QUERIES_DIR / "v3_alpha_sweep.checkpoint.json"
OUT_PATH = QUERIES_DIR / "v3_alpha_sweep.json"

ALPHAS = [round(0.05 * i, 2) for i in range(21)]

V126_DEFAULTS = {
    "identifier_lookup": 1.00,
    "structural_search": 0.90,
    "conceptual_search": 0.70,
    "behavioral_search": 0.00,
    "negation": 0.80,
    "multi_step": 1.00,
    "type_filtered": 1.00,
    "cross_language": 1.00,
    "unknown": 1.00,
}


def load_checkpoint(path: Path) -> tuple[set, dict]:
    """Return (completed_pairs, results) from an existing checkpoint, or empty."""
    if not path.exists():
        return set(), {}
    try:
        data = json.loads(path.read_text())
        completed = {(c, a) for c, a in data.get("completed_pairs", [])}
        results = data.get("results", {})
        # Re-key nested alpha map strings back to floats on lookup, but keep as str in JSON.
        return completed, results
    except (json.JSONDecodeError, OSError):
        return set(), {}


def save_checkpoint(
    path: Path, completed: set, results: dict, meta: dict
) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    payload = {
        "schema": "v3-alpha-sweep-checkpoint",
        "saved_at": int(time.time()),
        "completed_pairs": [list(p) for p in sorted(completed)],
        "results": results,
        **meta,
    }
    tmp.write_text(json.dumps(payload, indent=2))
    os.replace(tmp, path)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--fresh", action="store_true", help="ignore existing checkpoint")
    p.add_argument("--progress-every", type=int, default=50, help="log every N queries")
    args = p.parse_args()

    data = json.loads(TRAIN_PATH.read_text())
    rows = [q for q in data["queries"] if q.get("gold_chunk")]
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for q in rows:
        by_cat[q["category"]].append(q)

    total_queries = sum(len(qs) for qs in by_cat.values())
    total_pairs = len(by_cat) * len(ALPHAS)

    print(f"v3 train: {total_queries} queries across {len(by_cat)} categories")
    print(f"sweeping {len(ALPHAS)} alphas → {total_pairs} (category, α) pairs")
    for cat, qs in sorted(by_cat.items(), key=lambda x: -len(x[1])):
        print(f"  {cat:<22} N={len(qs)}")

    # Load checkpoint (or start fresh).
    if args.fresh and CHECKPOINT_PATH.exists():
        CHECKPOINT_PATH.unlink()
    completed, results = load_checkpoint(CHECKPOINT_PATH)
    if completed:
        print(f"\nresuming from checkpoint: {len(completed)}/{total_pairs} pairs done")

    # Only the pairs NOT yet done.
    todo = [
        (cat, a)
        for cat in sorted(by_cat)
        for a in ALPHAS
        if (cat, a) not in completed
    ]
    remaining_queries = sum(len(by_cat[c]) for c, _ in todo)
    print(f"todo: {len(todo)} pairs, ~{remaining_queries} queries")
    if not todo:
        print("nothing to do — writing final output from checkpoint.")

    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/alpha-sweep.stderr", "ab"),
        text=True, bufsize=1, env=env,
    )

    t0 = time.monotonic()
    t_last_log = t0
    t_last_ckpt = t0
    CHECKPOINT_INTERVAL_S = 15.0
    processed = 0
    errors = 0

    try:
        for cat, alpha in todo:
            queries = by_cat[cat]
            cat_hits = 0
            cat_total = 0
            for q in queries:
                cmd = f"search {shlex.quote(q['query'])} --limit 1 --splade-alpha {alpha}"
                try:
                    proc.stdin.write(cmd + "\n")
                    proc.stdin.flush()
                except (BrokenPipeError, OSError) as e:
                    print(f"\nERROR: cqs batch stdin closed: {e}", file=sys.stderr)
                    raise RuntimeError("cqs batch died") from e

                line = proc.stdout.readline()
                if not line:
                    print(f"\nERROR: cqs batch stdout EOF at pair ({cat}, {alpha})", file=sys.stderr)
                    raise RuntimeError("cqs batch died")

                try:
                    out = json.loads(line)
                    if "error" in out:
                        errors += 1
                        continue
                    gold = q["gold_chunk"]
                    gold_key = (gold.get("origin"), gold.get("name"), gold.get("line_start"))
                    res_list = out.get("results", [])
                    cat_total += 1
                    if res_list:
                        top = res_list[0]
                        if (top.get("file"), top.get("name"), top.get("line_start")) == gold_key:
                            cat_hits += 1
                except (json.JSONDecodeError, KeyError):
                    errors += 1
                except Exception as e:  # noqa: BLE001
                    errors += 1
                    print(f"[warn] per-query exception ({cat}, {alpha}): {e}", file=sys.stderr)
                    if errors <= 3:
                        traceback.print_exc(file=sys.stderr)

                processed += 1
                now = time.monotonic()
                if now - t_last_log >= 5 or processed == remaining_queries:
                    qps = processed / (now - t0)
                    eta_s = (remaining_queries - processed) / qps if qps > 0 else float("inf")
                    done_pairs = len(completed)
                    print(
                        f"[{processed:>5}/{remaining_queries}] "
                        f"pair=({cat:<20}, α={alpha:.2f}) "
                        f"cat_so_far={cat_hits}/{cat_total} "
                        f"pairs_done={done_pairs}/{total_pairs} "
                        f"qps={qps:5.2f} eta={eta_s/60:4.1f}m errors={errors}",
                        file=sys.stderr, flush=True,
                    )
                    t_last_log = now

            # Record this (cat, alpha) result.
            results.setdefault(cat, {})[f"{alpha:.2f}"] = {
                "hits_at_1": cat_hits,
                "total": cat_total,
                "r1": cat_hits / cat_total if cat_total else 0.0,
            }
            completed.add((cat, alpha))

            # Every time we finish the last α for a category, print the summary.
            done_alphas = len([a for (c, a) in completed if c == cat])
            if done_alphas == len(ALPHAS):
                per_alpha = results[cat]
                best_alpha_str, best_rec = max(per_alpha.items(), key=lambda x: x[1]["r1"])
                best_alpha = float(best_alpha_str)
                v126_alpha = V126_DEFAULTS.get(cat, 1.0)
                v126_key = f"{v126_alpha:.2f}"
                v126_r1 = per_alpha.get(v126_key, {}).get("r1")
                delta = (best_rec["r1"] - v126_r1) * 100 if v126_r1 is not None else float("nan")
                print(
                    f"\n*** category done: {cat:<22} N={len(queries)}  "
                    f"v1.26 α={v126_alpha:.2f} R@1={100*(v126_r1 or 0):.1f}%  "
                    f"→  best α={best_alpha:.2f} R@1={100*best_rec['r1']:.1f}%  "
                    f"Δ={delta:+.1f}pp ***\n",
                    file=sys.stderr, flush=True,
                )

            # Checkpoint after each (cat, alpha).
            now = time.monotonic()
            if now - t_last_ckpt >= CHECKPOINT_INTERVAL_S:
                save_checkpoint(
                    CHECKPOINT_PATH, completed, results,
                    meta={"total_pairs": total_pairs, "errors": errors, "wall_s": now - t0},
                )
                t_last_ckpt = now

    except Exception as e:  # noqa: BLE001
        print(f"\nFATAL in main loop: {type(e).__name__}: {e}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        # Save what we have before exiting.
        save_checkpoint(CHECKPOINT_PATH, completed, results, meta={"errors": errors, "aborted": True})
        try:
            proc.stdin.close()
        except OSError:
            pass
        proc.kill()
        return 2
    finally:
        save_checkpoint(CHECKPOINT_PATH, completed, results, meta={"errors": errors})
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()

    # Final summary.
    print(f"\n{'='*92}")
    print(f"{'category':<22} {'N':>4}  {'v1.26 α':>8}  {'v1.26 R@1':>10}  {'best α':>7}  {'best R@1':>9}  {'Δ':>7}")
    print("-" * 92)
    final_summary = {}
    for cat in sorted(by_cat):
        per_alpha = results.get(cat, {})
        if not per_alpha:
            continue
        best_alpha_str, best_rec = max(per_alpha.items(), key=lambda x: x[1]["r1"])
        best_alpha = float(best_alpha_str)
        v126_alpha = V126_DEFAULTS.get(cat, 1.0)
        v126_key = f"{v126_alpha:.2f}"
        v126_r1 = per_alpha.get(v126_key, {}).get("r1", float("nan"))
        delta = (best_rec["r1"] - v126_r1) * 100 if v126_r1 == v126_r1 else float("nan")
        print(
            f"  {cat:<20} {best_rec['total']:>4}  {v126_alpha:>8.2f}  "
            f"{100*(v126_r1 if v126_r1==v126_r1 else 0):>9.1f}%  "
            f"{best_alpha:>7.2f}  {100*best_rec['r1']:>8.1f}%  {delta:+5.1f}pp"
        )
        final_summary[cat] = {
            "n": best_rec["total"],
            "v1_26_alpha": v126_alpha,
            "v1_26_r1": v126_r1 if v126_r1 == v126_r1 else None,
            "best_alpha": best_alpha,
            "best_r1": best_rec["r1"],
            "delta_pp": delta if delta == delta else None,
            "per_alpha_r1": {k: v["r1"] for k, v in per_alpha.items()},
            "per_alpha_counts": {k: {"hits_at_1": v["hits_at_1"], "total": v["total"]} for k, v in per_alpha.items()},
        }

    OUT_PATH.write_text(
        json.dumps(
            {
                "schema": "v3-alpha-sweep",
                "created_at": int(time.time()),
                "alphas_swept": ALPHAS,
                "categories": final_summary,
                "errors": errors,
                "wall_s": time.monotonic() - t0,
            },
            indent=2,
        )
    )
    print(f"\nwrote {OUT_PATH}")
    print(f"errors: {errors}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
