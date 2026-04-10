#!/usr/bin/env python3
"""Generic parameter sweep harness for cqs eval.

Runs the v2 165-query train split eval once per parameter value, with
either a CLI flag or an environment variable varied between runs. Captures
overall R@1 / R@5 / R@20 plus per-category R@1 and writes results to a
TSV file that can be appended to research/enrichment.md.

Designed for one-knob-at-a-time sweeps, not full grid search. Compose
sweeps by running the script multiple times.

Usage examples:

    # Sweep CQS_TYPE_BOOST values (defaults to dense, no SPLADE)
    python3 evals/run_sweep.py \\
        --env CQS_TYPE_BOOST \\
        --values 1.0,1.05,1.1,1.15,1.2,1.3,1.5 \\
        --out /tmp/sweep_type_boost.tsv

    # Sweep --splade-alpha (CLI flag, with --splade enabled)
    python3 evals/run_sweep.py \\
        --cli-flag --splade-alpha \\
        --values 0.0,0.1,0.3,0.5,0.7,0.9,1.0 \\
        --extra-flags --splade \\
        --out /tmp/sweep_splade_alpha.tsv

    # Sweep with bypass on (no Phase 5 routing) for routing-vs-no-routing comparison
    CQS_DISABLE_BASE_INDEX=1 python3 evals/run_sweep.py \\
        --env CQS_TYPE_BOOST \\
        --values 1.0,1.2,1.5 \\
        --out /tmp/sweep_type_boost_no_routing.tsv

The --out file is appended to (not overwritten) so successive sweep runs
build up a single research log. Schema (tab-separated):

    timestamp  param  value  config  r1  r5  r20  n  cat_<name>_r1...

Sweeps are sequential — they can't run concurrently because each query
spawns a fresh `cqs` subprocess that contends for GPU and the same
.cqs/ index files.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

QUERY_SET = Path("evals/queries/v2_300q.json")


def load_queries(split: str) -> list[dict]:
    with open(QUERY_SET) as f:
        qs = json.load(f)
    if split == "all":
        return qs["queries"]
    return [q for q in qs["queries"] if q["split"] == split]


def run_search(query: str, extra_args: list[str], env: dict, n: int = 20) -> list[str]:
    """Run a single cqs query with the configured extra args + env, return result names."""
    cmd = ["cqs", query, "--json", "-n", str(n)] + extra_args
    try:
        result = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            env=env,
        )
        data = json.loads(result.stdout)
        return [r["name"] for r in data.get("results", [])]
    except Exception:
        return []


def evaluate_one_cell(
    queries: list[dict],
    extra_args: list[str],
    env: dict,
    label: str,
) -> dict:
    """Run all queries in one configuration. Returns overall + per-category metrics."""
    r1 = r5 = r20 = total = 0
    by_cat: dict[str, dict[str, int]] = defaultdict(
        lambda: {"r1": 0, "r5": 0, "r20": 0, "n": 0}
    )

    for q in queries:
        total += 1
        cat = q["category"]
        by_cat[cat]["n"] += 1

        names = run_search(q["query"], extra_args, env)

        expected = q["primary_answer"]["name"]
        acceptable = [a["name"] for a in q.get("acceptable_answers", [])]
        all_valid = {expected} | set(acceptable)

        hit_at: int | None = None
        for i, name in enumerate(names):
            if name in all_valid:
                hit_at = i + 1
                break

        if hit_at is not None:
            r20 += 1
            by_cat[cat]["r20"] += 1
            if hit_at <= 5:
                r5 += 1
                by_cat[cat]["r5"] += 1
            if hit_at <= 1:
                r1 += 1
                by_cat[cat]["r1"] += 1

        if total % 25 == 0:
            print(
                f"  [{label}] {total}/{len(queries)} queries...",
                file=sys.stderr,
                flush=True,
            )

    return {
        "n": total,
        "r1": r1,
        "r5": r5,
        "r20": r20,
        "by_cat": dict(by_cat),
    }


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Parameter sweep harness for cqs eval",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )

    knob = p.add_mutually_exclusive_group(required=True)
    knob.add_argument(
        "--env",
        metavar="VAR_NAME",
        help="Environment variable to sweep (e.g. CQS_TYPE_BOOST)",
    )
    knob.add_argument(
        "--cli-flag",
        metavar="FLAG_NAME",
        help="CLI flag to sweep (e.g. --splade-alpha). The flag must accept a value.",
    )

    p.add_argument(
        "--values",
        required=True,
        help="Comma-separated values to sweep over (e.g. 0.0,0.1,0.3,0.5,0.7,0.9,1.0)",
    )
    p.add_argument(
        "--extra-flags",
        nargs="*",
        default=[],
        help="Additional CLI flags applied on every cell (e.g. --splade)",
    )
    p.add_argument(
        "--split",
        default="train",
        choices=["train", "test", "all"],
        help="Query split to evaluate (default: train)",
    )
    p.add_argument(
        "--out",
        type=Path,
        required=True,
        help="TSV file to APPEND results to (created if missing)",
    )
    p.add_argument(
        "--label",
        default="",
        help="Optional config label included in each row (e.g. 'phase5+splade-code')",
    )
    return p.parse_args()


def write_tsv_header_if_needed(path: Path, categories: list[str]) -> None:
    if path.exists():
        return
    cat_cols = [f"cat_{c}_r1" for c in categories] + [f"cat_{c}_n" for c in categories]
    cols = ["timestamp", "param", "value", "config", "r1", "r5", "r20", "n"] + cat_cols
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\t".join(cols) + "\n")


def append_tsv_row(
    path: Path,
    timestamp: str,
    param: str,
    value: str,
    config: str,
    metrics: dict,
    categories: list[str],
) -> None:
    n = metrics["n"]
    if n == 0:
        return
    base = [
        timestamp,
        param,
        value,
        config,
        f"{metrics['r1'] / n:.4f}",
        f"{metrics['r5'] / n:.4f}",
        f"{metrics['r20'] / n:.4f}",
        str(n),
    ]
    cat_r1 = []
    cat_n = []
    for c in categories:
        cell = metrics["by_cat"].get(c, {"r1": 0, "n": 0})
        cn = cell["n"]
        cat_r1.append(f"{cell['r1'] / cn:.4f}" if cn else "")
        cat_n.append(str(cn))
    row = base + cat_r1 + cat_n
    with path.open("a") as f:
        f.write("\t".join(row) + "\n")


# Categories present in the v2 query set, sorted for stable column order.
CATEGORIES = [
    "behavioral_search",
    "conceptual_search",
    "cross_language",
    "identifier_lookup",
    "multi_step",
    "negation",
    "structural_search",
    "type_filtered",
]


def main() -> None:
    args = parse_args()
    queries = load_queries(args.split)
    print(
        f"Loaded {len(queries)} {args.split} queries from {QUERY_SET}",
        file=sys.stderr,
    )

    values = [v.strip() for v in args.values.split(",") if v.strip()]
    print(f"Sweeping {len(values)} values: {values}", file=sys.stderr)

    write_tsv_header_if_needed(args.out, CATEGORIES)

    param_name = args.env if args.env else args.cli_flag
    config_label = args.label or "default"
    started = time.time()

    for i, value in enumerate(values, start=1):
        cell_started = time.time()

        env = os.environ.copy()
        extra_args = list(args.extra_flags)

        if args.env:
            env[args.env] = value
            print(
                f"\n[{i}/{len(values)}] {args.env}={value}  extras={extra_args}",
                file=sys.stderr,
            )
        else:
            extra_args.extend([args.cli_flag, value])
            print(
                f"\n[{i}/{len(values)}] {args.cli_flag} {value}  extras={extra_args}",
                file=sys.stderr,
            )

        metrics = evaluate_one_cell(
            queries,
            extra_args=extra_args,
            env=env,
            label=f"{param_name}={value}",
        )

        elapsed = time.time() - cell_started
        n = metrics["n"]
        if n == 0:
            print(f"  → no queries evaluated (skipping row)", file=sys.stderr)
            continue
        print(
            f"  → R@1={metrics['r1'] / n * 100:.1f}%  "
            f"R@5={metrics['r5'] / n * 100:.1f}%  "
            f"R@20={metrics['r20'] / n * 100:.1f}%  "
            f"({elapsed:.0f}s)",
            file=sys.stderr,
        )

        timestamp = dt.datetime.now().isoformat(timespec="seconds")
        append_tsv_row(
            args.out,
            timestamp,
            param_name,
            value,
            config_label,
            metrics,
            CATEGORIES,
        )

    total_elapsed = time.time() - started
    print(
        f"\nSweep complete: {len(values)} cells in {total_elapsed:.0f}s "
        f"({total_elapsed / max(len(values), 1):.0f}s/cell). Results in {args.out}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
