#!/usr/bin/env python3
"""Build retrieval pools for v3_all.json using a single cqs batch subprocess.

Why a separate phase: each `cqs` cold-start eats ~5 GB RAM (BGE-large ONNX +
HNSW + SPLADE + 52 tree-sitter grammars). 24 simultaneous calls = 113 GB
memory pressure → WSL OOM-kill → distro restart. `cqs batch` keeps one
process alive that handles all queries sequentially, holding ~5 GB total.

Output: evals/queries/v3_pools.json — one entry per input query containing
its retrieval pool (deduped union of 3 retriever variants), used downstream
by validate_gold.py without ever spawning cqs again.

Usage: python3 evals/build_pools.py [--in v3_all.json] [--out v3_pools.json]
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

QUERIES_DIR = Path(__file__).parent / "queries"
IN_PATH = QUERIES_DIR / "v3_all.json"
OUT_PATH = QUERIES_DIR / "v3_pools.json"

# Same retriever variants as validate_gold.py method 2.
RETRIEVER_VARIANTS: list[tuple[str, list[str]]] = [
    ("router", []),
    ("dense", ["--splade-alpha", "0.0"]),
    ("sparse", ["--splade-alpha", "1.0"]),
]

log = logging.getLogger("build_pools")


def _quote_query(q: str) -> str:
    """cqs batch parses each line as `argv` — quote the query so spaces and
    special characters don't break the parser."""
    return shlex.quote(q)


def _format_command(query: str, top_k: int, extra: list[str]) -> str:
    extra_str = " ".join(shlex.quote(x) for x in extra)
    return f"search {_quote_query(query)} --limit {top_k} {extra_str}".rstrip()


def _chunk_key(res: dict) -> tuple:
    return (res.get("file"), res.get("name"), res.get("line_start"))


def _atomic_write(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2))
    os.replace(tmp, path)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--in", dest="inp", type=Path, default=IN_PATH)
    p.add_argument("--out", type=Path, default=OUT_PATH)
    p.add_argument("--top-k", type=int, default=20)
    p.add_argument("--limit", type=int, default=0, help="process only first N rows")
    p.add_argument("--checkpoint-every", type=int, default=100)
    p.add_argument("--verbose", "-v", action="count", default=0)
    args = p.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose >= 2 else logging.INFO if args.verbose else logging.WARNING,
        format="%(asctime)s %(levelname)-7s %(name)s  %(message)s",
        datefmt="%H:%M:%S",
    )

    if not args.inp.exists():
        log.error("missing input: %s", args.inp)
        return 1
    data = json.loads(args.inp.read_text())
    rows = data.get("queries") or []
    if args.limit:
        rows = rows[: args.limit]
    log.info("building pools for %d queries (top_k=%d)", len(rows), args.top_k)

    # Spawn cqs batch — ONE process for all 3*N queries.
    # IMPORTANT: stderr goes to a file, NOT a pipe. Each query emits ~90
    # "content_hash column missing" WARN lines (non-fatal); if we capture
    # stderr in a Python pipe and never drain it, the 64KB pipe buffer fills
    # in ~3-4 queries and cqs batch blocks on stderr write (wchan: pipe_write).
    log.info("spawning cqs batch (cold start ~5-15 s)")
    stderr_log = args.out.with_suffix(".cqs-stderr.log")
    stderr_log.parent.mkdir(parents=True, exist_ok=True)
    stderr_fh = open(stderr_log, "wb")
    log.info("cqs batch stderr -> %s", stderr_log)
    proc = subprocess.Popen(
        ["cqs", "batch"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=stderr_fh,
        text=True,
        bufsize=1,  # line-buffered for stdin/stdout text streams
    )

    pools: list[dict] = []
    t0 = time.monotonic()
    t_last_progress = t0
    PROGRESS_INTERVAL_S = 5.0

    try:
        for i, entry in enumerate(rows):
            query = entry["query"]
            results_per_variant: dict[str, list[dict]] = {}

            for variant_name, extra in RETRIEVER_VARIANTS:
                cmd = _format_command(query, args.top_k, extra)
                try:
                    proc.stdin.write(cmd + "\n")
                    proc.stdin.flush()
                except (BrokenPipeError, OSError) as e:
                    log.error("cqs batch stdin closed: %s", e)
                    proc.kill()
                    return 2

                # Read one JSONL response. cqs batch emits exactly one line
                # per input line (success or {"error":...}).
                line = proc.stdout.readline()
                if not line:
                    log.error("cqs batch stdout EOF after %d queries", i)
                    err = proc.stderr.read()
                    log.error("stderr tail: %s", err[-500:] if err else "<empty>")
                    return 3
                try:
                    parsed = json.loads(line)
                except json.JSONDecodeError as e:
                    log.warning("could not parse cqs response (q=%d v=%s): %s", i, variant_name, e)
                    parsed = {"error": f"parse: {e}"}
                if "error" in parsed:
                    log.warning("cqs error (q=%d v=%s): %s", i, variant_name, parsed["error"][:120])
                    results_per_variant[variant_name] = []
                else:
                    results_per_variant[variant_name] = parsed.get("results") or []

            # Build pool: dedupe by (file, name, line_start), record min rank.
            pool: dict[tuple, dict] = {}
            for variant_name, _ in RETRIEVER_VARIANTS:
                for rank, res in enumerate(results_per_variant.get(variant_name, [])):
                    key = _chunk_key(res)
                    if key not in pool:
                        pool[key] = {"result": res, "appearances": {}, "min_rank": rank}
                    pool[key]["appearances"][variant_name] = rank
                    if rank < pool[key]["min_rank"]:
                        pool[key]["min_rank"] = rank
            pool_sorted = sorted(pool.values(), key=lambda x: x["min_rank"])

            pools.append({
                "query": query,
                "category": entry.get("category"),
                "source": entry.get("source"),
                "gold_chunk": entry.get("gold_chunk"),
                "metadata": entry.get("metadata", {}),
                "pool": pool_sorted,
                "pool_size": len(pool_sorted),
                "appearances_count": {
                    name: sum(1 for pe in pool_sorted if name in pe["appearances"])
                    for name, _ in RETRIEVER_VARIANTS
                },
            })

            now = time.monotonic()
            if now - t_last_progress >= PROGRESS_INTERVAL_S or i + 1 == len(rows):
                qps = (i + 1) / (now - t0)
                empty = sum(1 for p in pools if p["pool_size"] == 0)
                avg_pool = sum(p["pool_size"] for p in pools) / len(pools)
                print(
                    f"[{i+1:>4}/{len(rows)}] qps={qps:5.2f} avg_pool={avg_pool:4.1f} empty={empty}",
                    file=sys.stderr, flush=True,
                )
                t_last_progress = now

            if args.checkpoint_every and (i + 1) % args.checkpoint_every == 0:
                _atomic_write(
                    args.out.with_suffix(".partial.json"),
                    {
                        "schema_version": "v3-pools-partial",
                        "progress": f"{i+1}/{len(rows)}",
                        "created_at": int(time.time()),
                        "pools": pools,
                    },
                )
    finally:
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        try:
            stderr_fh.close()
        except OSError:
            pass

    dt = time.monotonic() - t0
    pool_sizes = [p["pool_size"] for p in pools]
    empty = sum(1 for s in pool_sizes if s == 0)
    print(
        f"\nbuilt {len(pools)} pools in {dt:.1f} s ({len(pools)/dt:.2f} q/s)\n"
        f"  pool sizes: min={min(pool_sizes,default=0)} median={sorted(pool_sizes)[len(pool_sizes)//2] if pool_sizes else 0} "
        f"mean={sum(pool_sizes)/len(pool_sizes):.1f} max={max(pool_sizes,default=0)} empty={empty}",
        file=sys.stderr, flush=True,
    )

    _atomic_write(
        args.out,
        {
            "schema_version": "v3-pools",
            "created_at": int(time.time()),
            "n": len(pools),
            "top_k": args.top_k,
            "retriever_variants": [name for name, _ in RETRIEVER_VARIANTS],
            "pools": pools,
        },
    )
    partial = args.out.with_suffix(".partial.json")
    if partial.exists():
        try:
            partial.unlink()
        except OSError:
            pass
    print(f"wrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
