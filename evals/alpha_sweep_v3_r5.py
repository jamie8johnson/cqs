#!/usr/bin/env python3
"""Per-category SPLADE alpha sweep — targets R@5 (or R@1/R@20) on v3 train.

Sibling to `alpha_sweep_v3.py` (which targets R@1). Same sweep grid, same
checkpointable + resumable shape, but the target metric is configurable
via `--target {r1,r5,r20}` (default `r5`).

Why a separate script: the original `alpha_sweep_v3.py` produced the
v1.26.0 deployed defaults that ship in `src/search/router.rs`; keeping
that file pinned to its R@1 target means we can always re-derive those
defaults. This sibling is the v1.28.x re-sweep against the new R@5
ceiling (chunker doc fallback + windowing fix + classifier flip lifted
R@5 from 63% to 67%; the alphas may now be holding it back).

For each (category, α) pair: run all queries in that category through
`cqs batch` with `--splade-alpha α --limit 20`, count hits at 1/5/20.
Pick the α that maximizes the chosen target metric. Report all three
metrics at each α so the per-category tradeoff is visible.

Outputs:
    evals/queries/v3_alpha_sweep_r5.checkpoint.json  — partial, restart-safe
    evals/queries/v3_alpha_sweep_r5.json             — final, written at end

Run:
    python3 evals/alpha_sweep_v3_r5.py                # target r5
    python3 evals/alpha_sweep_v3_r5.py --target r1    # reproduce v1.26
    python3 evals/alpha_sweep_v3_r5.py --target r20   # max recall depth
    python3 evals/alpha_sweep_v3_r5.py --fresh        # ignore checkpoint
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


def split_paths(split: str) -> tuple[Path, Path, Path]:
    """Resolve (queries, checkpoint, output) for a given split.

    `train` reads v3_train.json (no .v2 sibling since the train split was
    never regenerated). `test` and `dev` read the v2 fixtures so we sweep
    against the same gold the production eval uses.
    """
    if split == "train":
        queries = QUERIES_DIR / "v3_train.json"
    else:
        queries = QUERIES_DIR / f"v3_{split}.v2.json"
    return (
        queries,
        QUERIES_DIR / f"v3_alpha_sweep_r5_{split}.checkpoint.json",
        QUERIES_DIR / f"v3_alpha_sweep_r5_{split}.json",
    )

ALPHAS = [round(0.05 * i, 2) for i in range(21)]

# Currently-deployed defaults from `src/search/router.rs` per-category alpha
# table (these were derived by alpha_sweep_v3.py against R@1).
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

K_LIST = (1, 5, 20)


def gold_key(g: dict) -> tuple:
    return (g.get("origin"), g.get("name"), g.get("line_start"))


def load_checkpoint(path: Path) -> tuple[set, dict]:
    if not path.exists():
        return set(), {}
    try:
        data = json.loads(path.read_text())
        completed = {(c, a) for c, a in data.get("completed_pairs", [])}
        return completed, data.get("results", {})
    except (json.JSONDecodeError, OSError):
        return set(), {}


def save_checkpoint(path: Path, completed: set, results: dict, meta: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    payload = {
        "schema": "v3-alpha-sweep-r5-checkpoint",
        "saved_at": int(time.time()),
        "completed_pairs": [list(p) for p in sorted(completed)],
        "results": results,
        **meta,
    }
    tmp.write_text(json.dumps(payload, indent=2))
    os.replace(tmp, path)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--split", default="train", choices=["train", "test", "dev"],
                   help="which v3 split to sweep (default train)")
    p.add_argument("--fresh", action="store_true", help="ignore existing checkpoint")
    p.add_argument("--target", default="r5", choices=["r1", "r5", "r20"],
                   help="metric to maximize when picking best α (default r5)")
    p.add_argument("--limit", type=int, default=20,
                   help="--limit passed to cqs (must be ≥ max k in K_LIST)")
    args = p.parse_args()

    target_key = args.target
    queries_path, checkpoint_path, out_path = split_paths(args.split)

    data = json.loads(queries_path.read_text())
    rows = [q for q in data["queries"] if q.get("gold_chunk")]
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for q in rows:
        by_cat[q["category"]].append(q)

    total_queries = sum(len(qs) for qs in by_cat.values())
    total_pairs = len(by_cat) * len(ALPHAS)

    print(f"v3 train: {total_queries} queries across {len(by_cat)} categories")
    print(f"sweeping {len(ALPHAS)} alphas → {total_pairs} pairs, target=R@{target_key[1:]}")
    for cat, qs in sorted(by_cat.items(), key=lambda x: -len(x[1])):
        print(f"  {cat:<22} N={len(qs)}")

    if args.fresh and checkpoint_path.exists():
        checkpoint_path.unlink()
    completed, results = load_checkpoint(checkpoint_path)
    if completed:
        print(f"\nresuming: {len(completed)}/{total_pairs} pairs done")

    todo = [
        (cat, a)
        for cat in sorted(by_cat)
        for a in ALPHAS
        if (cat, a) not in completed
    ]
    remaining_queries = sum(len(by_cat[c]) for c, _ in todo)
    print(f"todo: {len(todo)} pairs, ~{remaining_queries} queries")

    # IMPORTANT: classifier OFF so we measure pure α effect on each subset.
    # Otherwise the centroid classifier could shift queries between categories
    # at runtime, biasing the sweep.
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=open("/tmp/alpha-sweep-r5.stderr", "ab"),
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
            cat_hits = {f"r{k}": 0 for k in K_LIST}
            cat_total = 0

            for q in queries:
                cmd = (
                    f"search {shlex.quote(q['query'])} "
                    f"--limit {args.limit} --splade --splade-alpha {alpha}"
                )
                try:
                    proc.stdin.write(cmd + "\n")
                    proc.stdin.flush()
                except (BrokenPipeError, OSError) as e:
                    print(f"\nERROR: cqs batch stdin closed: {e}", file=sys.stderr)
                    raise RuntimeError("cqs batch died") from e

                line = proc.stdout.readline()
                if not line:
                    print(f"\nERROR: cqs batch stdout EOF at ({cat}, {alpha})", file=sys.stderr)
                    raise RuntimeError("cqs batch died")

                try:
                    out = json.loads(line)
                    payload = out.get("data") if isinstance(out.get("data"), dict) else out
                    if payload is None:
                        errors += 1
                        continue
                    res_list = payload.get("results", [])
                    cat_total += 1
                    gold = q["gold_chunk"]
                    target = gold_key(gold)
                    for i, r in enumerate(res_list):
                        if (r.get("file"), r.get("name"), r.get("line_start")) == target:
                            for k in K_LIST:
                                if i + 1 <= k:
                                    cat_hits[f"r{k}"] += 1
                            break
                except (json.JSONDecodeError, KeyError):
                    errors += 1
                except Exception as e:  # noqa: BLE001
                    errors += 1
                    if errors <= 3:
                        print(f"[warn] per-query exception ({cat}, {alpha}): {e}",
                              file=sys.stderr)
                        traceback.print_exc(file=sys.stderr)

                processed += 1
                now = time.monotonic()
                if now - t_last_log >= 5 or processed == remaining_queries:
                    qps = processed / (now - t0) if processed else 0
                    eta_s = (remaining_queries - processed) / qps if qps > 0 else float("inf")
                    print(
                        f"[{processed:>5}/{remaining_queries}] "
                        f"({cat:<20} α={alpha:.2f}) "
                        f"r1={cat_hits['r1']}/{cat_total} "
                        f"r5={cat_hits['r5']}/{cat_total} "
                        f"r20={cat_hits['r20']}/{cat_total} "
                        f"qps={qps:5.2f} eta={eta_s/60:4.1f}m err={errors}",
                        file=sys.stderr, flush=True,
                    )
                    t_last_log = now

            results.setdefault(cat, {})[f"{alpha:.2f}"] = {
                **{f"hits_at_{k}": cat_hits[f"r{k}"] for k in K_LIST},
                "total": cat_total,
                **{f"r{k}": (cat_hits[f"r{k}"] / cat_total if cat_total else 0.0) for k in K_LIST},
            }
            completed.add((cat, alpha))

            # Per-category summary on the last α.
            done_alphas = len([a for (c, a) in completed if c == cat])
            if done_alphas == len(ALPHAS):
                per_alpha = results[cat]
                best_alpha_str, best_rec = max(per_alpha.items(), key=lambda x: x[1][target_key])
                best_alpha = float(best_alpha_str)
                v126_alpha = V126_DEFAULTS.get(cat, 1.0)
                v126_key = f"{v126_alpha:.2f}"
                v126_target = per_alpha.get(v126_key, {}).get(target_key)
                delta = (best_rec[target_key] - v126_target) * 100 if v126_target is not None else float("nan")
                print(
                    f"\n*** {cat:<22} N={len(queries)}  "
                    f"v1.26 α={v126_alpha:.2f} R@{target_key[1:]}={100*(v126_target or 0):.1f}%  "
                    f"→  best α={best_alpha:.2f} R@{target_key[1:]}={100*best_rec[target_key]:.1f}%  "
                    f"Δ={delta:+.1f}pp ***\n",
                    file=sys.stderr, flush=True,
                )

            now = time.monotonic()
            if now - t_last_ckpt >= CHECKPOINT_INTERVAL_S:
                save_checkpoint(
                    checkpoint_path, completed, results,
                    meta={"total_pairs": total_pairs, "errors": errors,
                          "wall_s": now - t0, "target": target_key},
                )
                t_last_ckpt = now

    except Exception as e:  # noqa: BLE001
        print(f"\nFATAL: {type(e).__name__}: {e}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        save_checkpoint(checkpoint_path, completed, results,
                        meta={"errors": errors, "aborted": True, "target": target_key})
        try:
            proc.stdin.close()
        except OSError:
            pass
        proc.kill()
        return 2
    finally:
        save_checkpoint(checkpoint_path, completed, results,
                        meta={"errors": errors, "target": target_key})
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()

    # Final per-category table — show ALL three R@K at the chosen-best α
    # AND at the deployed v1.26 α, plus what the R@1-best α would have been.
    print(f"\n{'='*108}")
    print(f"target=R@{target_key[1:]}  v3 train  (best α picked by R@{target_key[1:]})")
    print(f"\n{'category':<22} {'N':>4}  {'v1.26 α':>8} {'v126 R@1':>9} {'v126 R@5':>9} {'v126 R@20':>10}  "
          f"{'best α':>7} {'new R@1':>9} {'new R@5':>9} {'new R@20':>10}  {'Δ '+target_key:>8}")
    print("-" * 108)
    final_summary = {}
    for cat in sorted(by_cat):
        per_alpha = results.get(cat, {})
        if not per_alpha:
            continue
        best_alpha_str, best_rec = max(per_alpha.items(), key=lambda x: x[1][target_key])
        best_alpha = float(best_alpha_str)
        v126_alpha = V126_DEFAULTS.get(cat, 1.0)
        v126_key = f"{v126_alpha:.2f}"
        v126_rec = per_alpha.get(v126_key, {})
        delta_target = (best_rec[target_key] - v126_rec.get(target_key, 0)) * 100
        print(
            f"  {cat:<20} {best_rec['total']:>4}  "
            f"{v126_alpha:>8.2f} {100*v126_rec.get('r1',0):>8.1f}% {100*v126_rec.get('r5',0):>8.1f}% {100*v126_rec.get('r20',0):>9.1f}%  "
            f"{best_alpha:>7.2f} {100*best_rec['r1']:>8.1f}% {100*best_rec['r5']:>8.1f}% {100*best_rec['r20']:>9.1f}%  "
            f"{delta_target:+7.1f}pp"
        )
        final_summary[cat] = {
            "n": best_rec["total"],
            "v1_26_alpha": v126_alpha,
            "v1_26": {f"r{k}": v126_rec.get(f"r{k}") for k in K_LIST},
            "best_alpha_for_target": best_alpha,
            "best_target": best_rec[target_key],
            "best_all_metrics": {f"r{k}": best_rec[f"r{k}"] for k in K_LIST},
            "delta_target_pp": delta_target,
            "per_alpha": {
                k: {f"r{kk}": v[f"r{kk}"] for kk in K_LIST}
                for k, v in per_alpha.items()
            },
            "per_alpha_counts": {
                k: {**{f"hits_at_{kk}": v[f"hits_at_{kk}"] for kk in K_LIST},
                    "total": v["total"]}
                for k, v in per_alpha.items()
            },
        }

    out_path.write_text(
        json.dumps(
            {
                "schema": "v3-alpha-sweep-r5",
                "target_metric": target_key,
                "created_at": int(time.time()),
                "alphas_swept": ALPHAS,
                "categories": final_summary,
                "errors": errors,
                "wall_s": time.monotonic() - t0,
            },
            indent=2,
        )
    )
    print(f"\nwrote {out_path}")
    print(f"errors: {errors}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
