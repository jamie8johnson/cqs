#!/usr/bin/env python3
"""Verify or attach a gold_chunk for every query, using pre-built pools.

Reads evals/queries/v3_pools.json (built by build_pools.py — one persistent
cqs batch process). This script only does Claude API calls now; cqs is not
spawned at all here. Decoupling pool building from validation keeps each
phase memory-light and survives WSL constraints.

Source-aware handling:
  - **Generated queries**: gold_chunk is the seed (set at generation time).
    Pool must contain the seed (any retriever surfaces it). If not, query is
    dropped — gold is NOT swapped (that would make v3 tautological).
  - **Telemetry queries**: gold_chunk is null. Walk the pool in min-rank
    order (best-surfaced first), ask Claude validate(query, chunk) on each
    independently, take the first yes. If nothing validates, dropped.

Writes evals/queries/v3_all_validated.json with:
  - `gold_verified` (bool)
  - `gold_rank` (int or null) — min rank across retrievers
  - `gold_appearances` (dict) — {retriever_name: rank_in_that_retriever}
  - `gold_validation_note` (str)
  - `pool_size` (int) — passed through from pools.json

Usage: python3 evals/validate_gold.py [--concurrency 8] [--llm-backend claude]
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import sys
import time
import traceback
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

QUERIES_DIR = Path(__file__).parent / "queries"
POOLS_PATH = QUERIES_DIR / "v3_pools.json"
OUT_PATH = QUERIES_DIR / "v3_all_validated.json"

PROGRESS_INTERVAL_S = 5.0

log = logging.getLogger("validate_gold")


def _make_client(backend: str):
    """Return a client with async validate(query, sig, preview) -> bool."""
    if backend == "vllm":
        from llm_client import LLMClient  # noqa: E402 — needs openai SDK
        return LLMClient()
    if backend == "claude":
        from claude_client import ClaudeClient  # noqa: E402 — needs anthropic SDK
        return ClaudeClient()
    raise ValueError(f"unknown backend: {backend}")


def chunk_matches(result: dict, gold: dict) -> bool:
    """Origin + name + line_start identity. Chunk ids drift across reindexes
    but origin/name/line_start stay stable for the same logical chunk."""
    return (
        result.get("file") == gold.get("origin")
        and result.get("name") == gold.get("name")
        and result.get("line_start") == gold.get("line_start")
    )


def _res_to_gold(res: dict) -> dict:
    return {
        "id": res.get("id"),
        "name": res.get("name"),
        "origin": res.get("file"),
        "language": res.get("language"),
        "chunk_type": res.get("chunk_type"),
        "line_start": res.get("line_start"),
        "line_end": res.get("line_end"),
    }


async def validate_one(client, sem: asyncio.Semaphore, entry: dict) -> dict:
    """Verify or attach the gold_chunk for one entry. `entry` comes from
    v3_pools.json and already has its retrieval pool attached."""
    query = entry["query"]
    gold = entry.get("gold_chunk")
    source = entry.get("source", "telemetry")
    pool = entry.get("pool", [])

    out = dict(entry)
    out.pop("pool", None)  # don't carry the pool into the validated output
    out["pool_size"] = len(pool)

    if not pool:
        out["gold_verified"] = False
        out["gold_rank"] = None
        out["gold_appearances"] = {}
        out["gold_validation_note"] = "empty pool — all retrievers returned 0 results"
        return out

    if source == "generated":
        if not gold:
            out["gold_verified"] = False
            out["gold_validation_note"] = "generated but no gold_chunk — schema bug upstream"
            return out
        for pe in pool:
            if chunk_matches(pe["result"], gold):
                out["gold_verified"] = True
                out["gold_rank"] = pe["min_rank"]
                out["gold_appearances"] = pe["appearances"]
                out["gold_validation_note"] = (
                    f"seed in pool from {list(pe['appearances'])} at min rank {pe['min_rank']}"
                )
                return out
        out["gold_verified"] = False
        out["gold_rank"] = None
        out["gold_appearances"] = {}
        out["gold_validation_note"] = (
            f"seed not in pooled top-K ({len(pool)} candidates) — dropping. "
            "Do NOT swap gold; that would be circular."
        )
        return out

    # Telemetry: walk pool by min_rank, take first LLM-validated.
    llm_errors = 0
    for pe in pool:
        res = pe["result"]
        try:
            async with sem:
                ok = await client.validate(
                    query,
                    res.get("signature") or res.get("name") or "",
                    res.get("preview") or res.get("content") or res.get("snippet") or "",
                )
        except Exception as e:  # noqa: BLE001 — one bad call shouldn't stall the entry
            llm_errors += 1
            log.warning("validate failed (%s) query=%r chunk=%s", type(e).__name__, query[:60], res.get("name"))
            continue
        if ok:
            out["gold_chunk"] = _res_to_gold(res)
            out["gold_verified"] = True
            out["gold_rank"] = pe["min_rank"]
            out["gold_appearances"] = pe["appearances"]
            out["llm_errors"] = llm_errors
            out["gold_validation_note"] = (
                f"telemetry: LLM-validated pool member from {list(pe['appearances'])} "
                f"at min rank {pe['min_rank']}"
            )
            return out
    out["gold_verified"] = False
    out["gold_rank"] = None
    out["gold_appearances"] = {}
    out["llm_errors"] = llm_errors
    out["gold_validation_note"] = (
        f"telemetry: no LLM-match in {len(pool)} pool candidates"
        + (f" ({llm_errors} LLM errors skipped)" if llm_errors else "")
    )
    return out


async def _safe_validate(client, sem, entry, errors):
    try:
        return await validate_one(client, sem, entry)
    except Exception as e:  # noqa: BLE001
        errors.append({"query": entry.get("query"), "err": f"{type(e).__name__}: {e}"})
        log.error("validate_one crashed: %s\n%s", e, traceback.format_exc())
        out = {k: v for k, v in entry.items() if k != "pool"}
        out["pool_size"] = len(entry.get("pool", []))
        out["gold_verified"] = False
        out["gold_validation_note"] = f"exception: {type(e).__name__}: {e}"
        return out


def _atomic_write(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2))
    os.replace(tmp, path)


async def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--in", dest="inp", type=Path, default=POOLS_PATH)
    p.add_argument("--out", type=Path, default=OUT_PATH)
    p.add_argument("--concurrency", type=int, default=8)
    p.add_argument("--limit", type=int, default=0)
    p.add_argument("--verbose", "-v", action="count", default=0)
    p.add_argument("--checkpoint-every", type=int, default=100)
    p.add_argument("--llm-backend", choices=["vllm", "claude"], default="claude")
    args = p.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose >= 2 else logging.INFO if args.verbose else logging.WARNING,
        format="%(asctime)s %(levelname)-7s %(name)s  %(message)s",
        datefmt="%H:%M:%S",
    )

    if not args.inp.exists():
        log.error("missing input: %s — run build_pools.py first", args.inp)
        return 1
    try:
        data = json.loads(args.inp.read_text())
    except json.JSONDecodeError as e:
        log.error("input is not valid JSON: %s", e)
        return 1
    rows = data.get("pools") or []
    if not rows:
        log.error("no pools in input")
        return 1
    if args.limit:
        rows = rows[: args.limit]

    log.info("validating %d entries (concurrency=%d backend=%s)", len(rows), args.concurrency, args.llm_backend)

    client = _make_client(args.llm_backend)
    sem = asyncio.Semaphore(args.concurrency)
    errors: list = []
    validated: list = []
    t0 = time.monotonic()
    t_last_progress = t0

    tasks = [asyncio.create_task(_safe_validate(client, sem, r, errors)) for r in rows]

    for fut in asyncio.as_completed(tasks):
        result = await fut
        validated.append(result)

        now = time.monotonic()
        if now - t_last_progress >= PROGRESS_INTERVAL_S or len(validated) == len(rows):
            verified = sum(1 for r in validated if r.get("gold_verified"))
            qps = len(validated) / (now - t0)
            print(
                f"[{len(validated):>4}/{len(rows)}] verified={verified:>4} fail={len(validated)-verified:>3} err={len(errors):>2} qps={qps:5.2f}",
                file=sys.stderr, flush=True,
            )
            t_last_progress = now

        if args.checkpoint_every and len(validated) % args.checkpoint_every == 0:
            _atomic_write(
                args.out.with_suffix(".partial.json"),
                {
                    "schema_version": "v3-validated-partial",
                    "progress": f"{len(validated)}/{len(rows)}",
                    "created_at": int(time.time()),
                    "queries": validated,
                },
            )

    dt = time.monotonic() - t0
    if hasattr(client, "aclose"):
        await client.aclose()

    verified = sum(1 for r in validated if r.get("gold_verified"))
    from_seed = sum(
        1 for r in validated if r.get("gold_verified") and "seed in pool" in (r.get("gold_validation_note") or "")
    )
    from_telemetry = sum(
        1 for r in validated if r.get("gold_verified") and "telemetry" in (r.get("gold_validation_note") or "")
    )
    failed = [r for r in validated if not r.get("gold_verified")]
    gen_failed = sum(1 for r in failed if r.get("source") == "generated")
    tel_failed = len(failed) - gen_failed

    from_retriever: Counter = Counter()
    router_only = 0
    for r in validated:
        if r.get("gold_verified"):
            apps = r.get("gold_appearances") or {}
            for name in apps:
                from_retriever[name] += 1
            if set(apps.keys()) == {"router"}:
                router_only += 1

    print(f"\nverified         : {verified}/{len(validated)} ({100*verified/len(validated):4.1f}%)")
    print(f"  seed-in-pool   : {from_seed}  (generated, seed found by ≥1 retriever)")
    print(f"  telemetry gold : {from_telemetry}  (LLM-picked from pool)")
    print(f"failed           : {len(failed)}")
    print(f"  generated      : {gen_failed}  (seed not in pool — dropped)")
    print(f"  telemetry      : {tel_failed}  (no LLM-match)")
    print(f"wall time        : {dt:.1f}s ({len(validated)/dt:.2f} q/s)")

    pool_sizes = [r.get("pool_size", 0) for r in validated]
    if pool_sizes:
        ps_sorted = sorted(pool_sizes)
        median = ps_sorted[len(ps_sorted) // 2]
        mean = sum(ps_sorted) / len(ps_sorted)
        empty = sum(1 for s in ps_sorted if s == 0)
        print(f"\npool sizes       : min={ps_sorted[0]} median={median} mean={mean:.1f} max={ps_sorted[-1]} empty={empty}")

    if verified:
        print(f"\ngold-by-retriever (overlap counted per variant):")
        for name in ["router", "dense", "sparse"]:
            n = from_retriever.get(name, 0)
            print(f"  {name:<7} {n:>4}  ({100*n/verified:4.1f}% of verified)")
        print(f"  router-only (nothing else found it): {router_only}")

    if errors:
        print(f"\nuncaught errors  : {len(errors)} (first 3):")
        for e in errors[:3]:
            print(f"  {e['err']}  query={e['query']!r:.80}")

    _atomic_write(
        args.out,
        {
            "schema_version": "v3-validated",
            "created_at": int(time.time()),
            "n": len(validated),
            "concurrency": args.concurrency,
            "llm_backend": args.llm_backend,
            "verified": verified,
            "uncaught_error_count": len(errors),
            "queries": validated,
        },
    )
    partial = args.out.with_suffix(".partial.json")
    if partial.exists():
        try:
            partial.unlink()
        except OSError:
            pass
    print(f"\nwrote {args.out}")

    if failed:
        print("\nfirst 8 failures:")
        for r in failed[:8]:
            print(f"  [{r.get('category','?'):<20}] {r.get('query','?')}")
            print(f"    note: {r.get('gold_validation_note','?')}")

    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
