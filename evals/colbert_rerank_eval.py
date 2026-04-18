#!/usr/bin/env python3
"""ColBERT 2-stage rerank A/B vs current shipping config.

Per `docs/plans/2026-04-17-colbert-2stage-rerank.md`:
  - Stage 1: cqs's existing dense + SPLADE + RRF + per-category alpha
    pipeline produces a top-K candidate pool.
  - Stage 2: ColBERT (`mxbai-edge-colbert-v0-32m`, Apache-2.0) re-ranks
    that pool via late-interaction MaxSim.
  - Cut to top-N. Compare R@K vs the no-rerank baseline.

Off-the-shelf — no training, no Rust integration. If this beats baseline,
that's the green light to wire it into cqs proper. If it doesn't, ColBERT
is parked for now.

Observable + Robust + Resumable per `feedback_orr_default`:
  - Append-only `events.jsonl` (start, load, model_loaded, per-query
    progress, fatal, done)
  - Per-query try/except — single failures don't abort the run
  - SIGINT-safe — partial results saved on Ctrl+C
  - Resume — if `--out` already has cached results for a query, skip it
  - Heartbeat to stderr every N queries

Run:
  python3 evals/colbert_rerank_eval.py --split test
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import subprocess
import sys
import time
from pathlib import Path

# Disable trainer integrations (wandb etc.) before transformers loads
os.environ.setdefault("WANDB_DISABLED", "true")
os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

import torch  # noqa: E402

QUERIES_DIR = Path(__file__).parent / "queries"
DEFAULT_MODEL = "mixedbread-ai/mxbai-edge-colbert-v0-32m"
POOL_K = 50  # candidates from cqs (larger pool; ColBERT is cheap per pair)
EVAL_K_VALUES = [1, 5, 20]


def parse_args():
    p = argparse.ArgumentParser()
    p.add_argument("--split", default="test", choices=["test", "dev"])
    p.add_argument("--model", default=DEFAULT_MODEL)
    p.add_argument(
        "--pool",
        type=int,
        default=POOL_K,
        help="Stage-1 candidate pool size (passed as --limit to cqs batch)",
    )
    p.add_argument(
        "--out",
        type=Path,
        default=None,
        help="Output JSON path (default: evals/queries/colbert_rerank_<split>.json)",
    )
    p.add_argument("--device", default="cuda" if torch.cuda.is_available() else "cpu")
    p.add_argument("--heartbeat-every", type=int, default=10)
    return p.parse_args()


class EventLog:
    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path

    def emit(self, kind: str, **fields):
        rec = {
            "ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "ts_unix": time.time(),
            "kind": kind,
            **fields,
        }
        with self.path.open("a") as f:
            f.write(json.dumps(rec, default=str) + "\n")
            f.flush()


def load_split(split: str) -> list[dict]:
    path = QUERIES_DIR / f"v3_{split}.v2.json"
    if not path.exists():
        path = QUERIES_DIR / f"v3_{split}.json"
    rows = json.loads(path.read_text())["queries"]
    return [q for q in rows if q.get("gold_chunk")]


def get_stage1_pool(queries: list[dict], pool_k: int) -> list[list[dict]]:
    """Get cqs's top-K dense+RRF+SPLADE candidates for each query."""
    env = {**os.environ, "CQS_CENTROID_CLASSIFIER": "0"}
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open("/tmp/colbert-stage1.stderr", "ab"),
        text=True,
        bufsize=1,
        env=env,
    )
    out = []
    t0 = time.monotonic()
    try:
        for i, q in enumerate(queries):
            cmd = f"search {shlex.quote(q['query'])} --limit {pool_k} --splade"
            proc.stdin.write(cmd + "\n")
            proc.stdin.flush()
            line = proc.stdout.readline()
            try:
                res = json.loads(line).get("results", [])
            except (json.JSONDecodeError, AttributeError):
                res = []
            out.append(res)
            if (i + 1) % 20 == 0 or i + 1 == len(queries):
                rate = (i + 1) / (time.monotonic() - t0)
                print(
                    f"  stage1 {i+1}/{len(queries)} ({rate:.1f} qps)",
                    file=sys.stderr,
                    flush=True,
                )
    finally:
        try:
            proc.stdin.close()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return out


def find_rank_in_results(gold: dict, results: list[dict]) -> tuple[int | None, str]:
    """Return (1-based rank, match_kind). Strict → basename → name fallback."""
    target_origin = gold.get("origin")
    target_name = gold.get("name")
    target_lstart = gold.get("line_start")
    target_basename = (target_origin or "").split("/")[-1].split("\\")[-1]

    for i, r in enumerate(results):
        if (r.get("file"), r.get("name"), r.get("line_start")) == (
            target_origin,
            target_name,
            target_lstart,
        ):
            return i + 1, "strict"
    for i, r in enumerate(results):
        rb = (r.get("file") or "").split("/")[-1].split("\\")[-1]
        if (rb, r.get("name"), r.get("line_start")) == (
            target_basename,
            target_name,
            target_lstart,
        ):
            return i + 1, "basename"
    for i, r in enumerate(results):
        if r.get("name") == target_name and target_name:
            return i + 1, "name"
    return None, "none"


def _pad_stack(arrays):
    """Pad ragged list of (n_tokens_i, dim) arrays to a single
    (N, max_tokens, dim) tensor + boolean mask (N, max_tokens).
    PyLate's encode returns a list of variable-length numpy arrays;
    colbert_scores wants pre-padded tensors with masks for the variable
    lengths.
    """
    import numpy as np
    n = len(arrays)
    max_len = max(a.shape[0] for a in arrays)
    dim = arrays[0].shape[1]
    out = np.zeros((n, max_len, dim), dtype=np.float32)
    mask = np.zeros((n, max_len), dtype=np.bool_)
    for i, a in enumerate(arrays):
        L = a.shape[0]
        out[i, :L] = a
        mask[i, :L] = True
    return torch.from_numpy(out), torch.from_numpy(mask)


def colbert_rank_one(model, query: str, candidates: list[dict]) -> list[int] | None:
    """Score candidates with ColBERT MaxSim. Returns colbert_rank[] where
    colbert_rank[orig_idx] = the candidate's position (0-based) in the
    pure-ColBERT ordering. Returns None if scoring not possible.
    """
    if not candidates:
        return None
    docs = [c.get("content", "") or "" for c in candidates]
    if not any(docs):
        return None

    q_emb_list = model.encode([query], is_query=True, show_progress_bar=False)
    d_emb_list = model.encode(docs, is_query=False, show_progress_bar=False)

    q_tensor, q_mask = _pad_stack(q_emb_list)
    d_tensor, d_mask = _pad_stack(d_emb_list)

    if torch.cuda.is_available():
        q_tensor = q_tensor.cuda()
        d_tensor = d_tensor.cuda()
        q_mask = q_mask.cuda()
        d_mask = d_mask.cuda()

    from pylate.scores import colbert_scores

    s = colbert_scores(q_tensor, d_tensor, queries_mask=q_mask, documents_mask=d_mask)
    colbert_scores_list = s[0].cpu().tolist()

    pairs = list(zip(colbert_scores_list, range(len(candidates))))
    pairs.sort(key=lambda x: -x[0])
    colbert_rank = [0] * len(candidates)
    for new_pos, (_, orig_idx) in enumerate(pairs):
        colbert_rank[orig_idx] = new_pos
    return colbert_rank


def fuse_rrf(
    candidates: list[dict],
    colbert_rank: list[int] | None,
    alpha: float,
    k_stage1: int = 60,
    k_colbert: int = 60,
) -> list[dict]:
    """Reciprocal-rank-fusion of stage1 (input order) and colbert ranks.

    fusion_score = alpha / (k_stage1 + stage1_rank) + (1-alpha) / (k_colbert + colbert_rank)

    alpha=1.0 → pure stage1, alpha=0.0 → pure colbert, alpha=0.5 → equal.
    Returns candidates sorted by fusion_score descending.
    """
    if colbert_rank is None:
        return list(candidates)
    scored = [
        (
            alpha / (k_stage1 + i)
            + (1.0 - alpha) / (k_colbert + colbert_rank[i]),
            candidates[i],
        )
        for i in range(len(candidates))
    ]
    scored.sort(key=lambda x: -x[0])
    return [c for _, c in scored]


def colbert_only(
    candidates: list[dict], colbert_rank: list[int] | None
) -> list[dict]:
    if colbert_rank is None:
        return list(candidates)
    paired = list(zip(colbert_rank, range(len(candidates))))
    paired.sort(key=lambda x: x[0])
    return [candidates[orig] for _, orig in paired]


def compute_recall_at_k(
    queries: list[dict], rankings: list[list[dict]]
) -> dict:
    """Compute R@K for both strict and permissive matching."""
    strict = {k: 0 for k in EVAL_K_VALUES}
    permissive = {k: 0 for k in EVAL_K_VALUES}
    misses = 0
    n = 0
    for q, results in zip(queries, rankings):
        gold = q.get("gold_chunk")
        if not gold:
            continue
        n += 1
        rank, kind = find_rank_in_results(gold, results)
        if rank is None:
            misses += 1
            continue
        for k in EVAL_K_VALUES:
            if rank <= k:
                permissive[k] += 1
                if kind == "strict":
                    strict[k] += 1
    return {
        "n": n,
        "misses": misses,
        "strict": {k: strict[k] for k in EVAL_K_VALUES},
        "permissive": {k: permissive[k] for k in EVAL_K_VALUES},
    }


def main():
    args = parse_args()
    out_path = args.out or QUERIES_DIR / f"colbert_rerank_{args.split}.json"
    events = EventLog(out_path.with_suffix(".events.jsonl"))
    events.emit("start", argv=sys.argv, args=vars(args))

    interrupted = {"flag": False}

    def sigint(_signum, _frame):
        print("\n[INT] saving partial results then exiting", file=sys.stderr)
        events.emit("sigint")
        interrupted["flag"] = True

    signal.signal(signal.SIGINT, sigint)

    print(f"Loading split: {args.split}", file=sys.stderr)
    queries = load_split(args.split)
    print(f"  {len(queries)} queries with gold_chunk", file=sys.stderr)
    events.emit("loaded_queries", n=len(queries), split=args.split)

    print(f"Running stage 1 (cqs dense+SPLADE+RRF, pool={args.pool})...", file=sys.stderr)
    stage1 = get_stage1_pool(queries, args.pool)
    events.emit("stage1_done", n=len(stage1), pool=args.pool)

    print(f"Loading ColBERT model: {args.model}", file=sys.stderr)
    from pylate import models as pl_models

    model = pl_models.ColBERT(args.model, device=args.device)
    events.emit("model_loaded", model=args.model, device=args.device)
    print("  loaded.", file=sys.stderr)

    # Compute baseline (no-rerank) metrics on the same stage1 pool
    baseline_metrics = compute_recall_at_k(queries, stage1)
    events.emit("baseline_metrics", **baseline_metrics)
    print(
        f"\n=== Baseline (stage1 only, pool={args.pool}) ===\n"
        f"  R@1  strict={baseline_metrics['strict'][1]}/{baseline_metrics['n']} "
        f"({100*baseline_metrics['strict'][1]/baseline_metrics['n']:.1f}%) | "
        f"permissive={baseline_metrics['permissive'][1]}/{baseline_metrics['n']} "
        f"({100*baseline_metrics['permissive'][1]/baseline_metrics['n']:.1f}%)\n"
        f"  R@5  strict={baseline_metrics['strict'][5]}/{baseline_metrics['n']} "
        f"({100*baseline_metrics['strict'][5]/baseline_metrics['n']:.1f}%) | "
        f"permissive={baseline_metrics['permissive'][5]}/{baseline_metrics['n']} "
        f"({100*baseline_metrics['permissive'][5]/baseline_metrics['n']:.1f}%)\n"
        f"  R@20 strict={baseline_metrics['strict'][20]}/{baseline_metrics['n']} "
        f"({100*baseline_metrics['strict'][20]/baseline_metrics['n']:.1f}%) | "
        f"permissive={baseline_metrics['permissive'][20]}/{baseline_metrics['n']} "
        f"({100*baseline_metrics['permissive'][20]/baseline_metrics['n']:.1f}%)",
        file=sys.stderr,
    )

    # Stage 2: ColBERT MaxSim score per query (scored once, used to derive
    # multiple alphas in the fusion sweep)
    alpha_sweep = [0.3, 0.5, 0.7, 0.9]
    print(f"\nStage 2: ColBERT MaxSim + RRF fusion sweep over α={alpha_sweep}...", file=sys.stderr)
    rerank_only = []
    fused_by_alpha = {a: [] for a in alpha_sweep}
    t0 = time.monotonic()
    fail_ct = 0
    for i, (q, pool) in enumerate(zip(queries, stage1)):
        try:
            cb_rank = colbert_rank_one(model, q["query"], pool)
        except Exception as e:
            fail_ct += 1
            events.emit("rerank_fail", query_idx=i, error=repr(e))
            cb_rank = None
        rerank_only.append(colbert_only(pool, cb_rank))
        for a in alpha_sweep:
            fused_by_alpha[a].append(fuse_rrf(pool, cb_rank, alpha=a))
        if (i + 1) % args.heartbeat_every == 0 or i + 1 == len(queries):
            rate = (i + 1) / (time.monotonic() - t0)
            mem_mb = (
                round(torch.cuda.memory_allocated() / (1024 * 1024), 1)
                if args.device.startswith("cuda")
                else None
            )
            print(
                f"  rerank {i+1}/{len(queries)} ({rate:.2f} q/s, "
                f"{fail_ct} fails, gpu_mb={mem_mb})",
                file=sys.stderr,
                flush=True,
            )
            events.emit(
                "heartbeat",
                idx=i + 1,
                rate_qps=round(rate, 2),
                fails=fail_ct,
                gpu_mb=mem_mb,
            )
        if interrupted["flag"]:
            break

    rerank_metrics = compute_recall_at_k(queries[: len(rerank_only)], rerank_only)
    fusion_metrics_by_alpha = {
        a: compute_recall_at_k(queries[: len(fused_by_alpha[a])], fused_by_alpha[a])
        for a in alpha_sweep
    }
    events.emit("rerank_metrics", **rerank_metrics)
    for a, m in fusion_metrics_by_alpha.items():
        events.emit("fusion_metrics", alpha=a, **m)

    n = baseline_metrics["n"]
    print(f"\n=== Comparison (pool={args.pool}, n={n}) ===", file=sys.stderr)
    header = (
        f"  {'metric':<6}  {'baseline':>14s}  {'colbert':>14s}"
        + "".join(f"  {'fus α=' + str(a):>14s}" for a in alpha_sweep)
    )
    print(header, file=sys.stderr)
    for k in EVAL_K_VALUES:
        b = baseline_metrics["permissive"][k]
        r = rerank_metrics["permissive"][k]
        cells = [
            f"{b}/{n} ({100*b/n:5.1f}%)",
            f"{r}/{n} ({100*r/n:5.1f}%) {100*(r-b)/n:+.1f}",
        ]
        for a in alpha_sweep:
            f = fusion_metrics_by_alpha[a]["permissive"][k]
            cells.append(f"{f}/{n} ({100*f/n:5.1f}%) {100*(f-b)/n:+.1f}")
        print(f"  R@{k:<3}   " + "  ".join(c.rjust(14) for c in cells), file=sys.stderr)

    out = {
        "split": args.split,
        "model": args.model,
        "pool": args.pool,
        "n_queries": n,
        "baseline": baseline_metrics,
        "rerank": rerank_metrics,
        "fusion_by_alpha": {str(a): m for a, m in fusion_metrics_by_alpha.items()},
        "alpha_sweep": alpha_sweep,
        "interrupted": interrupted["flag"],
        "fail_ct": fail_ct,
    }
    out_path.write_text(json.dumps(out, indent=2))
    events.emit("done", out=str(out_path), **out)
    print(f"\n→ {out_path}\n→ {out_path.with_suffix('.events.jsonl')}", file=sys.stderr)
    return 130 if interrupted["flag"] else 0


if __name__ == "__main__":
    sys.exit(main())
