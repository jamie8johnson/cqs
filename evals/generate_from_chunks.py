#!/usr/bin/env python3
"""Generate category-targeted eval queries from indexed chunks.

For each target category, sample diverse seed chunks from the cqs index,
ask the local LLM to produce queries phrased to fit the target category,
then validate each generated query by classifying it back — only queries
whose classification matches the target category are kept. This filters
prompt drift.

Writes to evals/queries/v3_generated_round1.json with a schema matching the
labeled-telemetry output so downstream can merge them.

Usage:
    python3 evals/generate_from_chunks.py \\
        --target cross_language=20,multi_step=20,structural_search=20 \\
        --concurrency 16

Defaults to a small smoke run (N=20 per category) so we can eyeball output
before scaling to N=100.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import random
import sqlite3
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from llm_client import CATEGORIES, LLMClient  # noqa: E402

DB_PATH = Path(".cqs/index.db")
OUT_PATH = Path(__file__).parent / "queries" / "v3_generated_round1.json"

# Languages worth sampling from. Markdown dominates the index but generates
# useless code queries. TOML/JSON/YAML are config — similarly useless.
CODE_LANGUAGES = {
    "rust", "python", "typescript", "javascript", "go", "java", "csharp",
    "cpp", "c", "ruby", "php", "swift", "kotlin", "scala", "haskell",
    "ocaml", "elixir", "erlang", "lua", "zig", "sql", "bash", "powershell",
    "css", "dart",
}

# Kinds that make sense as query targets (exclude markdown sections, config
# keys, bare constants).
CODE_KINDS = {"function", "method", "class", "struct", "impl", "interface",
              "trait", "enum", "typealias", "macro", "constructor"}


def load_candidate_chunks(db: Path, exclude_ids: set | None = None) -> list[dict]:
    conn = sqlite3.connect(str(db))
    conn.row_factory = sqlite3.Row
    placeholders_lang = ",".join("?" * len(CODE_LANGUAGES))
    placeholders_kind = ",".join("?" * len(CODE_KINDS))
    rows = conn.execute(
        f"""
        SELECT DISTINCT id, name, origin, chunk_type, language, line_start, line_end,
               signature, substr(content, 1, 400) AS preview
        FROM chunks
        WHERE name != ''
          AND length(content) > 60
          AND language IN ({placeholders_lang})
          AND chunk_type IN ({placeholders_kind})
          AND origin NOT LIKE 'tests/%'
          AND origin NOT LIKE '%/tests/%'
          AND origin NOT LIKE 'docs/%'
          AND origin NOT LIKE 'target/%'
        ORDER BY language, origin, line_start
        """,
        (*sorted(CODE_LANGUAGES), *sorted(CODE_KINDS)),
    ).fetchall()
    conn.close()
    out = [dict(r) for r in rows]
    if exclude_ids:
        before = len(out)
        out = [c for c in out if c["id"] not in exclude_ids]
        print(f"[load] excluded {before - len(out)} previously-used chunks; "
              f"{len(out)} held-out candidates remain", file=sys.stderr)
    return out


def stratify(chunks: list[dict], n: int, seed: int = 0) -> list[dict]:
    """Pick n chunks with roughly balanced language coverage."""
    rng = random.Random(seed)
    by_lang: dict[str, list[dict]] = defaultdict(list)
    for c in chunks:
        by_lang[c["language"]].append(c)
    langs = list(by_lang.keys())
    rng.shuffle(langs)
    picked: list[dict] = []
    # Round-robin across languages.
    cursor = {lang: 0 for lang in langs}
    idx_lang = {lang: 0 for lang in langs}
    for lang in langs:
        rng.shuffle(by_lang[lang])
    while len(picked) < n and langs:
        progress = False
        for lang in langs:
            if idx_lang[lang] < len(by_lang[lang]):
                picked.append(by_lang[lang][idx_lang[lang]])
                idx_lang[lang] += 1
                progress = True
                if len(picked) >= n:
                    break
        if not progress:
            break
    return picked


async def _gen_one(
    client: LLMClient,
    sem: asyncio.Semaphore,
    chunk: dict,
    target_category: str,
    n_per_chunk: int,
) -> list[dict]:
    """Generate n queries for one seed chunk, validate each via classify."""
    async with sem:
        try:
            raw = await client.generate(
                chunk["signature"] or chunk["name"],
                chunk["preview"],
                n=n_per_chunk,
                category=target_category,
                language=chunk["language"],
            )
        except Exception as e:  # noqa: BLE001
            return [{"error": f"generate: {type(e).__name__}: {e}", "chunk_id": chunk["id"]}]
    kept: list[dict] = []
    for q in raw:
        q = q.strip()
        if not q or len(q) < 6:
            continue
        async with sem:
            try:
                got = await client.classify(q)
            except Exception as e:  # noqa: BLE001
                kept.append({"error": f"classify: {type(e).__name__}: {e}", "query": q, "chunk_id": chunk["id"]})
                continue
        kept.append(
            {
                "query": q,
                "category": got,
                "target_category": target_category,
                "matched": got == target_category,
                "gold_chunk": {
                    "id": chunk["id"],
                    "name": chunk["name"],
                    "origin": chunk["origin"],
                    "language": chunk["language"],
                    "chunk_type": chunk["chunk_type"],
                    "line_start": chunk["line_start"],
                    "line_end": chunk["line_end"],
                },
            }
        )
    return kept


async def generate_for_category(
    client: LLMClient,
    chunks: list[dict],
    category: str,
    target_n: int,
    concurrency: int,
    n_per_chunk: int = 2,
) -> tuple[list[dict], list[dict]]:
    """Generate target_n validated queries for a category.

    Returns (kept, rejected). Kept entries have category == target_category
    and a valid gold_chunk. Rejected entries are off-target or had errors.
    """
    # Over-sample seeds to account for rejection.
    oversample = max(target_n * 3, target_n + 10)
    seeds = stratify(chunks, oversample, seed=hash(category) & 0xFFFF)
    sem = asyncio.Semaphore(concurrency)
    results = await asyncio.gather(
        *(_gen_one(client, sem, s, category, n_per_chunk) for s in seeds)
    )
    flat: list[dict] = [r for sublist in results for r in sublist]
    kept = [r for r in flat if r.get("matched")]
    rejected = [r for r in flat if not r.get("matched")]
    # Deduplicate by query text, keep first.
    seen: set[str] = set()
    unique_kept: list[dict] = []
    for r in kept:
        q = r["query"]
        if q in seen:
            continue
        seen.add(q)
        unique_kept.append(r)
        if len(unique_kept) >= target_n:
            break
    return unique_kept, rejected


def parse_targets(s: str) -> list[tuple[str, int]]:
    out: list[tuple[str, int]] = []
    for part in s.split(","):
        part = part.strip()
        if not part:
            continue
        k, v = part.split("=")
        k = k.strip()
        if k not in CATEGORIES:
            raise SystemExit(f"unknown category: {k}")
        out.append((k, int(v)))
    return out


async def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument(
        "--target",
        type=parse_targets,
        default="cross_language=20,multi_step=20,structural_search=20",
        help="comma-separated category=N pairs",
    )
    p.add_argument("--concurrency", type=int, default=16)
    p.add_argument("--n-per-chunk", type=int, default=2)
    p.add_argument("--db", type=Path, default=DB_PATH)
    p.add_argument("--out", type=Path, default=OUT_PATH)
    p.add_argument("--exclude-chunks", type=Path, default=None,
                   help="Newline-separated list of chunk IDs to exclude (held-out eval split)")
    args = p.parse_args()

    if not args.db.exists():
        print(f"index missing: {args.db}", file=sys.stderr)
        return 1

    targets = args.target if isinstance(args.target, list) else parse_targets(args.target)

    exclude_ids = set()
    if args.exclude_chunks and args.exclude_chunks.exists():
        exclude_ids = {line.strip() for line in args.exclude_chunks.read_text().splitlines() if line.strip()}
        print(f"[load] excluding {len(exclude_ids)} chunks from {args.exclude_chunks}", file=sys.stderr)
    chunks = load_candidate_chunks(args.db, exclude_ids=exclude_ids)
    lang_counts = Counter(c["language"] for c in chunks)
    print(f"candidate chunks: {len(chunks)}")
    print("language distribution:")
    for lang, n in lang_counts.most_common(10):
        print(f"  {lang:<14} {n:>5}")

    client = LLMClient()
    all_kept: list[dict] = []
    all_rejected: list[dict] = []

    for category, target_n in targets:
        print(f"\n=== {category} (target {target_n}) ===")
        t0 = time.monotonic()
        kept, rejected = await generate_for_category(
            client, chunks, category, target_n, args.concurrency, n_per_chunk=args.n_per_chunk
        )
        dt = time.monotonic() - t0
        pass_rate = len(kept) / (len(kept) + len(rejected)) if (kept or rejected) else 0.0
        print(f"  kept: {len(kept):>3}/{target_n} | rejected: {len(rejected):>3} | pass: {pass_rate:5.1%} | {dt:5.1f}s")
        if len(kept) < target_n:
            print(f"  WARN: under target by {target_n - len(kept)} — need more seeds or looser generation")
        all_kept.extend(kept)
        all_rejected.extend(rejected)

    await client.aclose()

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(
            {
                "generated_at": int(time.time()),
                "model": client.model,
                "n_kept": len(all_kept),
                "n_rejected": len(all_rejected),
                "targets": [{"category": c, "target_n": n} for c, n in targets],
                "queries": all_kept,
                "rejected": all_rejected[:200],  # truncate rejected log
            },
            indent=2,
        )
    )
    print(f"\nwrote {args.out}")
    print(f"total kept: {len(all_kept)} | total rejected: {len(all_rejected)}")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
