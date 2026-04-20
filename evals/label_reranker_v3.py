#!/usr/bin/env python3
"""Label cqs-domain reranker candidates via Gemma 4 31B for graded pointwise training.

Reads v3_{train,dev,test}.json and v3_pools.json, pulls raw chunk content
straight from source files (bypassing the tokenized-content bug in some
cqs stored rows), and asks Gemma to score each (query, candidate) pair
on a 3-way scale:

    relevant   -> label 1.0
    partial    -> label 0.5
    not_relevant -> label 0.0

Output (JSONL, one per line):
    {"query": str, "content": str, "label": float,
     "query_idx": int, "gold": bool, "chunk_name": str, "chunk_file": str,
     "split": "train"|"dev"|"test"}

Three contracts (per feedback_orr_default.md):

  Observable
    - Heartbeat to stderr every ~20 items (rows/sec, eta, pct done)
    - Events append to evals/label_reranker_v3.events.jsonl: start/row/resume/done

  Robust
    - Skips candidates whose content file is missing or line range is invalid
    - Skips raw LLM replies that don't match one of the three verdicts
    - Falls back to 0.0 (not_relevant) if the LLM reply is non-parseable but
      only AFTER logging + counting; never silently invents a label

  Resumable
    - Uses LLMClient's blake3→SQLite cache: re-running is a no-op on already-labeled pairs
    - Output JSONL is append-only; on resume we re-read existing rows and skip them
    - A fresh --output directory starts from scratch; an existing one continues

Run:
    python3 evals/label_reranker_v3.py \\
        --output evals/reranker_v3 \\
        --concurrency 16

Smoke (first 20 rows per split):
    python3 evals/label_reranker_v3.py --limit-per-split 20 --output /tmp/rr3-smoke
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
from pathlib import Path

from llm_client import LLMClient

QUERIES_DIR = Path(__file__).parent / "queries"
POOLS_PATH = QUERIES_DIR / "v3_pools.json"
SPLITS = [
    ("train", QUERIES_DIR / "v3_train.json"),
    ("dev", QUERIES_DIR / "v3_dev.v2.json"),
    ("test", QUERIES_DIR / "v3_test.v2.json"),
]

VERDICT_TO_LABEL = {"relevant": 1.0, "partial": 0.5, "not_relevant": 0.0}

# Gemma has an 8192-token context. System prompt + query + instructions eat
# ~400 tokens; leave a safe margin and cap content at ~28k chars (~7k tokens
# at a conservative 4 chars/token). Longer chunks are truncated with a
# marker so the LLM sees the signal-rich head, not a hard 400 error.
MAX_CONTENT_CHARS = 28_000


SYSTEM_PROMPT = (
    "You judge whether a code chunk is relevant to a search query in a "
    "code-retrieval system. You reply with EXACTLY one of three tokens:\n"
    "\n"
    "  relevant       — the chunk is a direct match for what the query asks; "
    "a developer seeing this chunk first would say 'yes, this is what I wanted'.\n"
    "  partial        — the chunk is related (same module, mentions the concept, "
    "similar signature) but is not the primary answer. A developer might click "
    "through but would prefer something more specific.\n"
    "  not_relevant   — the chunk does not answer the query. Wrong function, "
    "wrong concept, or only surface-level word overlap.\n"
    "\n"
    "Be strict. Only return 'relevant' for a clear primary answer. When in "
    "doubt between partial and not_relevant, pick not_relevant.\n"
    "\n"
    "Reply with ONLY the verdict word — no quotes, no markdown, no prose."
)


def load_source_content(repo_root: Path, origin: str, line_start: int, line_end: int) -> str | None:
    """Read raw source content from the repo for the given chunk span.

    Returns None if the file is missing or the line range is out of bounds.
    We use 1-indexed inclusive line ranges, matching cqs conventions.
    """
    if not origin or line_start is None or line_end is None:
        return None
    p = repo_root / origin
    if not p.exists() or not p.is_file():
        return None
    try:
        lines = p.read_text(encoding="utf-8", errors="replace").splitlines(keepends=True)
    except OSError:
        return None
    if line_start < 1 or line_end > len(lines) or line_start > line_end:
        return None
    return "".join(lines[line_start - 1 : line_end])


def build_work_items(repo_root: Path, limit_per_split: int | None) -> list[dict]:
    """Flatten train+dev+test × pool into a list of labeling work items."""
    pools_data = json.loads(POOLS_PATH.read_text())
    pools_by_query: dict[str, dict] = {p["query"]: p for p in pools_data["pools"]}

    items: list[dict] = []
    missing_file = 0
    missing_pool = 0

    for split_name, split_path in SPLITS:
        if not split_path.exists():
            print(f"[skip] {split_name}: {split_path} missing", file=sys.stderr)
            continue
        rows = json.loads(split_path.read_text())["queries"]
        if limit_per_split:
            rows = rows[:limit_per_split]

        for q_idx, entry in enumerate(rows):
            query = entry["query"]
            gold = entry.get("gold_chunk") or {}
            gold_key = (gold.get("origin"), gold.get("name"), gold.get("line_start"))
            pool_entry = pools_by_query.get(query)
            if not pool_entry:
                missing_pool += 1
                continue
            for cand in pool_entry.get("pool", []):
                r = cand["result"]
                origin = r.get("file") or r.get("origin")
                line_start = r.get("line_start")
                line_end = r.get("line_end")
                name = r.get("name")
                content = load_source_content(repo_root, origin, line_start, line_end)
                if content is None:
                    missing_file += 1
                    continue
                is_gold = (origin, name, line_start) == gold_key
                items.append({
                    "split": split_name,
                    "query": query,
                    "query_idx": q_idx,
                    "content": content,
                    "chunk_name": name,
                    "chunk_file": origin,
                    "line_start": line_start,
                    "line_end": line_end,
                    "gold": is_gold,
                })
    print(
        f"[load] {len(items)} work items (missing_pool={missing_pool}, "
        f"missing_file={missing_file})",
        file=sys.stderr,
    )
    return items


def load_completed(out_dir: Path) -> set[tuple]:
    """Keys of rows already written, so we can resume without re-calling the LLM.

    Key = (split, query, chunk_file, chunk_name, line_start).
    """
    completed: set[tuple] = set()
    for split, _ in SPLITS:
        path = out_dir / f"reranker_v3_{split}.graded.jsonl"
        if not path.exists():
            continue
        with path.open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    continue
                completed.add((
                    r.get("split"),
                    r.get("query"),
                    r.get("chunk_file"),
                    r.get("chunk_name"),
                    r.get("line_start"),
                ))
    return completed


async def label_one(
    client: LLMClient,
    query: str,
    content: str,
) -> tuple[str, str]:
    """Return (verdict, raw_reply). Verdict is one of relevant/partial/not_relevant, or 'invalid'."""
    if len(content) > MAX_CONTENT_CHARS:
        content = content[:MAX_CONTENT_CHARS] + "\n… [truncated]"
    user = (
        f"Query: {query}\n"
        f"\n"
        f"Code chunk:\n"
        f"```\n{content}\n```\n"
        f"\n"
        f"Verdict (one word):"
    )
    raw = await client._chat(
        SYSTEM_PROMPT, user, role="rerank_v3_pointwise", max_tokens=8, temperature=0.0
    )
    tok = raw.strip().lower().split()[0] if raw.strip() else ""
    if tok in VERDICT_TO_LABEL:
        return tok, raw
    # Second-chance: exact-word match in the reply
    low = raw.lower()
    for key in VERDICT_TO_LABEL:
        if key in low:
            return key, raw
    return "invalid", raw


class EventLog:
    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path

    def emit(self, kind: str, **fields) -> None:
        rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "kind": kind, **fields}
        with self.path.open("a") as f:
            f.write(json.dumps(rec, default=str) + "\n")
            f.flush()
            os.fsync(f.fileno())


async def main_async():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True,
                    help="Output directory. Creates {train,dev,test}.graded.jsonl here.")
    ap.add_argument("--repo-root", type=Path, default=Path("/mnt/c/Projects/cqs"))
    ap.add_argument("--concurrency", type=int, default=16)
    ap.add_argument("--limit-per-split", type=int, default=None,
                    help="Truncate each split to first N queries (smoke test).")
    args = ap.parse_args()

    args.output.mkdir(parents=True, exist_ok=True)
    events = EventLog(args.output / "events.jsonl")
    events.emit("start", argv=sys.argv, args=vars(args))

    items = build_work_items(args.repo_root, args.limit_per_split)
    completed = load_completed(args.output)
    if completed:
        print(f"[resume] {len(completed)} rows already labeled — skipping them",
              file=sys.stderr)
        events.emit("resume", completed=len(completed))

    remaining = [
        it for it in items
        if (it["split"], it["query"], it["chunk_file"], it["chunk_name"], it["line_start"])
        not in completed
    ]
    n_total = len(remaining)
    print(f"[plan] {n_total} items to label (already done: {len(items) - n_total})",
          file=sys.stderr)
    if n_total == 0:
        print("[done] nothing to do", file=sys.stderr)
        return 0

    client = LLMClient()
    sem = asyncio.Semaphore(args.concurrency)

    writers: dict[str, object] = {}
    for split, _ in SPLITS:
        writers[split] = (args.output / f"reranker_v3_{split}.graded.jsonl").open("a")

    counts = {"relevant": 0, "partial": 0, "not_relevant": 0, "invalid": 0}
    t0 = time.monotonic()
    done = 0
    write_lock = asyncio.Lock()

    async def worker(item):
        nonlocal done
        async with sem:
            verdict, raw = await label_one(client, item["query"], item["content"])
        counts[verdict] += 1
        label = VERDICT_TO_LABEL.get(verdict, 0.0)  # invalid → 0.0 per contract
        row = {
            "query": item["query"],
            "content": item["content"],
            "label": label,
            "query_idx": item["query_idx"],
            "gold": item["gold"],
            "chunk_name": item["chunk_name"],
            "chunk_file": item["chunk_file"],
            "line_start": item["line_start"],
            "split": item["split"],
            "verdict": verdict,
        }
        async with write_lock:
            f = writers[item["split"]]
            f.write(json.dumps(row) + "\n")
            f.flush()
            done += 1
            if done % 20 == 0 or done == n_total:
                elapsed = time.monotonic() - t0
                rate = done / max(elapsed, 0.001)
                eta_s = (n_total - done) / max(rate, 0.001)
                print(
                    f"  {done}/{n_total} ({100 * done / n_total:.1f}%) "
                    f"{rate:.2f} rows/s eta {eta_s / 60:.1f}m "
                    f"rel={counts['relevant']} par={counts['partial']} "
                    f"not={counts['not_relevant']} inv={counts['invalid']}",
                    file=sys.stderr, flush=True,
                )
                events.emit(
                    "progress", done=done, total=n_total, rate=round(rate, 2),
                    counts=dict(counts),
                )

    try:
        tasks = [asyncio.create_task(worker(it)) for it in remaining]
        await asyncio.gather(*tasks)
    finally:
        for f in writers.values():
            f.close()
        await client.aclose()

    wall = time.monotonic() - t0
    events.emit("done", wall_secs=round(wall, 1), counts=dict(counts), done=done)
    print(
        f"\n[done] {done} rows in {wall / 60:.1f}m "
        f"rel={counts['relevant']} par={counts['partial']} "
        f"not={counts['not_relevant']} inv={counts['invalid']}",
        file=sys.stderr,
    )
    return 0


def main():
    sys.exit(asyncio.run(main_async()))


if __name__ == "__main__":
    main()
