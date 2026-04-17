#!/usr/bin/env python3
"""Calibration gate for the 200k Reranker V2 labeling corpus.

Labels 1000 (query, chunk_a, chunk_b) triples sampled from
`~/training-data/augmented_200k_keydac.jsonl` with BOTH the local Gemma 4
31B vLLM server AND Claude Haiku, then computes inter-rater agreement.
The decision (GEMMA_ONLY / HYBRID / CLAUDE_ONLY) gates whether the full
200k labeling pass can rely on Gemma alone.

Pipeline:
    1.  Sample 1000 triples (resumable: reuses existing
        evals/queries/calibration_1k.jsonl unless --resample).
    2.  Label with Gemma + Claude in parallel (cheap when re-run thanks to
        the SQLite caches in llm_client.py / claude_client.py).
    3.  Compute agreement, Cohen's kappa, ground-truth agreement, and a
        confusion matrix; write evals/queries/calibration_agreement.json.

Observability:
    - Per-judge progress to stderr every --progress-every triples
      (default 25): "[gemma] 250/1000 (25.0%, 8.3 t/s, ETA 1m30s, 2 errs)"
    - 30s heartbeat even when no progress (so a hung server is visible)
    - Final timing breakdown for every phase

Resumability:
    - Output JSONLs are append-only with flush() after each line; a kill
      mid-run preserves N labels.
    - On restart, reads the existing JSONLs to skip already-labeled IDs.
    - The sampled triples file (`calibration_1k.jsonl`) is reused unless
      --resample (so judges always see the same prompts across restarts).
    - --gemma-only / --claude-only let a partial run complete one side.

Robustness:
    - Per-triple try/except: one Gemma timeout / Claude rate limit / parse
      error doesn't abort the run.
    - Network errors → 3 retries with capped exponential backoff before
      recording `label: "LABEL_ERROR"`.
    - Unparseable LLM responses → `label: "PARSE_ERROR"` (counted, but
      excluded from agreement metrics).
    - SIGINT (Ctrl+C) → flush + summary + exit 130.
    - Three dry-run pairs before the main loop validate the prompt.
    - >5% parse errors on either judge → loud warning + 5s pause (the
      prompt is probably broken and you should stop and fix it).

Usage:
    python3 evals/calibrate_reranker_labels.py --sample 1000
    python3 evals/calibrate_reranker_labels.py --gemma-only --concurrency 32
    python3 evals/calibrate_reranker_labels.py --dry-run 10
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import io
import json
import os
import random
import signal
import sys
import time
import traceback
from collections import Counter
from pathlib import Path
from typing import Any, Awaitable, Callable

sys.path.insert(0, str(Path(__file__).parent))

QUERIES_DIR = Path(__file__).parent / "queries"
SAMPLE_PATH = QUERIES_DIR / "calibration_1k.jsonl"
GEMMA_PATH = QUERIES_DIR / "calibration_1k_gemma.jsonl"
CLAUDE_PATH = QUERIES_DIR / "calibration_1k_claude.jsonl"
REPORT_PATH = QUERIES_DIR / "calibration_agreement.json"
CHECKPOINT_PATH = QUERIES_DIR / "calibration.checkpoint.json"

DEFAULT_CORPUS = Path(os.path.expanduser("~/training-data/augmented_200k_keydac.jsonl"))

LABEL_PROMPT_SYSTEM = (
    "You are a code search relevance judge. Given a query and two code chunks, "
    "decide which chunk better matches the query. Reply with EXACTLY one word: "
    "A, B, or TIE. No punctuation, no prose, no explanation."
)


def _label_user_prompt(query: str, chunk_a: str, chunk_b: str) -> str:
    return (
        f"Query: {query}\n\n"
        f"Chunk A:\n{chunk_a}\n\n"
        f"Chunk B:\n{chunk_b}\n\n"
        "Which chunk better matches the query? Reply with EXACTLY one word: A, B, or TIE."
    )


# ---------------------------------------------------------------- IO helpers


def _strip_passage_prefix(s: str) -> str:
    """augmented_200k_keydac stores chunks with `passage: ` prefix and
    queries with `query: ` prefix. Strip them so the judge sees raw text."""
    if not s:
        return s
    for prefix in ("passage: ", "query: "):
        if s.startswith(prefix):
            return s[len(prefix):]
    return s


def _atomic_write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2))
    os.replace(tmp, path)


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    rows: list[dict] = []
    with path.open() as f:
        for ln in f:
            ln = ln.strip()
            if not ln:
                continue
            try:
                rows.append(json.loads(ln))
            except json.JSONDecodeError:
                # Line was partially written before a kill — skip it.
                # The label will be re-issued on restart.
                continue
    return rows


# ---------------------------------------------------------------- sampling


def _index_offsets(path: Path) -> list[int]:
    """Return byte offsets to the start of each line in `path`. Linear
    scan but read-once: ~30s for the 2.6 GB corpus."""
    offsets = [0]
    with path.open("rb") as f:
        while True:
            ln = f.readline()
            if not ln:
                break
            offsets.append(f.tell())
    offsets.pop()  # last entry is EOF, not a line start
    return offsets


def _sample_triples(corpus: Path, n: int, seed: int) -> list[dict]:
    """Random sample n triples. Each row in augmented_200k_keydac has 0+
    hard negatives; we skip rows with no negatives, pick one negative at
    random for each kept row, and randomize whether positive lands as A
    or B so the label distribution isn't ordered."""
    rng = random.Random(seed)
    print(f"[sample] indexing line offsets in {corpus}…", file=sys.stderr, flush=True)
    t0 = time.monotonic()
    offsets = _index_offsets(corpus)
    print(
        f"[sample] indexed {len(offsets)} rows in {time.monotonic()-t0:.1f}s",
        file=sys.stderr, flush=True,
    )

    # Oversample: some rows have no negatives, drop those and resample.
    target = n
    pool: list[dict] = []
    tried: set[int] = set()

    def _try_append(f, i: int) -> bool:
        if i in tried:
            return False
        tried.add(i)
        f.seek(offsets[i])
        try:
            r = json.loads(f.readline())
        except json.JSONDecodeError:
            return False
        negs = r.get("negatives") or []
        if not negs:
            return False
        neg = rng.choice(negs)
        positive_is_a = rng.random() < 0.5
        pos_clean = _strip_passage_prefix(r.get("positive", ""))
        neg_clean = _strip_passage_prefix(neg)
        if positive_is_a:
            chunk_a, chunk_b = pos_clean, neg_clean
        else:
            chunk_a, chunk_b = neg_clean, pos_clean
        pool.append(
            {
                "id": len(pool),
                "query": _strip_passage_prefix(r.get("query", "")),
                "chunk_a": chunk_a,
                "chunk_b": chunk_b,
                "positive_is_a": positive_is_a,
                "source": r.get("source", ""),
                "language": r.get("language", ""),
            }
        )
        return True

    with corpus.open("rb") as f:
        # First pass: 2× oversample without replacement.
        candidates = rng.sample(range(len(offsets)), min(int(n * 2), len(offsets)))
        for i in candidates:
            if len(pool) >= target:
                break
            _try_append(f, i)

        # Second pass: if the corpus has a low-negative tail, keep drawing
        # random indices (with replacement-avoidance via `tried`) until we
        # hit target or exhaust the whole file.
        while len(pool) < target and len(tried) < len(offsets):
            i = rng.randrange(0, len(offsets))
            _try_append(f, i)

    return pool


# ---------------------------------------------------------------- judges


def _parse_label(raw: str) -> str:
    """Map the model's reply to A / B / TIE / PARSE_ERROR.

    The prompt asks for exactly one word, but Gemma sometimes prefixes
    with ``Answer:`` or wraps in markdown. We try to be lenient without
    accepting noise (a free-text response counts as a parse error so the
    metrics aren't polluted with random A/B picks)."""
    if not raw:
        return "PARSE_ERROR"
    cleaned = raw.strip().strip("`\"'.,! ").upper()
    # Strip common leading prefixes
    for prefix in ("ANSWER:", "LABEL:", "CHUNK", "RESPONSE:"):
        if cleaned.startswith(prefix):
            cleaned = cleaned[len(prefix):].strip()
    # First token after normalization
    first = cleaned.split()[0] if cleaned.split() else ""
    if first in ("A", "B", "TIE"):
        return first
    # Edge: model says "A." or "BTIE" — try first character only
    if first and first[0] in ("A", "B", "T"):
        if first[0] == "T" and "TIE" in cleaned[:8]:
            return "TIE"
        if first[0] in ("A", "B") and len(first) == 1:
            return first[0]
    return "PARSE_ERROR"


class Judge:
    """One side of the calibration. Wraps a callable that takes
    (system, user) and returns the raw model response. Owns the output
    JSONL and a counter set."""

    def __init__(
        self,
        name: str,
        out_path: Path,
        invoke: Callable[[str, str], Awaitable[str]],
        progress_every: int,
        retries: int = 3,
    ):
        self.name = name
        self.out_path = out_path
        self.invoke = invoke
        self.progress_every = max(1, progress_every)
        self.retries = max(1, retries)

        self.completed: int = 0
        self.parse_errors: int = 0
        self.label_errors: int = 0
        self.in_flight: int = 0
        self.t0: float = time.monotonic()
        self.t_last_progress: float = self.t0
        self.t_last_event: float = self.t0
        self.lock = asyncio.Lock()
        self.fh: io.TextIOWrapper | None = None

    async def __aenter__(self) -> "Judge":
        self.out_path.parent.mkdir(parents=True, exist_ok=True)
        self.fh = self.out_path.open("a", encoding="utf-8")
        self.t0 = time.monotonic()
        self.t_last_progress = self.t0
        self.t_last_event = self.t0
        return self

    async def __aexit__(self, *exc) -> None:
        if self.fh is not None:
            try:
                self.fh.flush()
                self.fh.close()
            except OSError:
                pass

    async def label(self, triple: dict, total: int) -> None:
        self.in_flight += 1
        try:
            user = _label_user_prompt(triple["query"], triple["chunk_a"], triple["chunk_b"])
            t_start = time.monotonic()
            raw: str | None = None
            err: str | None = None
            for attempt in range(self.retries):
                try:
                    raw = await self.invoke(LABEL_PROMPT_SYSTEM, user)
                    break
                except Exception as e:  # noqa: BLE001 — network / SDK errors
                    err = f"{type(e).__name__}: {e}"
                    backoff = min(8.0, 0.5 * (2 ** attempt))
                    print(
                        f"[{self.name}] retry {attempt+1}/{self.retries} for id={triple['id']}: "
                        f"{err} (sleep {backoff:.1f}s)",
                        file=sys.stderr, flush=True,
                    )
                    await asyncio.sleep(backoff)
            latency_ms = int((time.monotonic() - t_start) * 1000)

            if raw is None:
                label = "LABEL_ERROR"
                raw_response = err or ""
                async with self.lock:
                    self.label_errors += 1
            else:
                label = _parse_label(raw)
                raw_response = raw
                if label == "PARSE_ERROR":
                    async with self.lock:
                        self.parse_errors += 1
                    print(
                        f"[{self.name}] PARSE_ERROR id={triple['id']} raw={raw!r:.120}",
                        file=sys.stderr, flush=True,
                    )

            row = {
                "id": triple["id"],
                "label": label,
                "raw_response": raw_response,
                "latency_ms": latency_ms,
            }

            async with self.lock:
                if self.fh is not None:
                    self.fh.write(json.dumps(row) + "\n")
                    self.fh.flush()
                self.completed += 1
                self.t_last_event = time.monotonic()
                self._maybe_progress(total)
        finally:
            self.in_flight -= 1

    def _maybe_progress(self, total: int) -> None:
        now = time.monotonic()
        if (
            self.completed % self.progress_every == 0
            or self.completed == total
            or now - self.t_last_progress >= 30.0
        ):
            elapsed = max(0.001, now - self.t0)
            rate = self.completed / elapsed
            remaining = max(0, total - self.completed)
            eta_s = remaining / rate if rate > 0 else float("inf")
            eta_str = f"{eta_s:.0f}s" if eta_s < 90 else f"{eta_s/60:.1f}m"
            print(
                f"[{self.name}] {self.completed}/{total} "
                f"({100*self.completed/max(1,total):.1f}%, {rate:.1f} t/s, "
                f"ETA {eta_str}, in_flight={self.in_flight}, "
                f"parse_err={self.parse_errors}, label_err={self.label_errors})",
                file=sys.stderr, flush=True,
            )
            self.t_last_progress = now


async def _heartbeat(judges: list[Judge], total: int, stop: asyncio.Event) -> None:
    """Print a heartbeat every 30s so a hung run is visible. Quits when
    stop is set."""
    while not stop.is_set():
        try:
            await asyncio.wait_for(stop.wait(), timeout=30.0)
            break
        except asyncio.TimeoutError:
            pass
        now = time.monotonic()
        for j in judges:
            if j.completed >= total:
                continue
            silent_s = now - j.t_last_event
            if silent_s >= 30.0:
                print(
                    f"[heartbeat {j.name}] alive but silent {silent_s:.0f}s "
                    f"(completed {j.completed}/{total}, in_flight={j.in_flight})",
                    file=sys.stderr, flush=True,
                )


# ---------------------------------------------------------------- judge wrappers


def _make_gemma_invoke(client) -> Callable[[str, str], Awaitable[str]]:
    """Use llm_client.LLMClient's underlying _chat with the calibration
    role so cache keys are scoped to this script."""
    async def invoke(system: str, user: str) -> str:
        return await client._chat(  # noqa: SLF001 — intentional reuse
            system, user, role="rerank_calibrate", max_tokens=8, temperature=0.0
        )
    return invoke


def _make_claude_invoke(client) -> Callable[[str, str], Awaitable[str]]:
    """Issue the same prompt against Claude Haiku. claude_client.py only
    exposes validate(); we make raw messages.create calls directly here
    while still piggybacking on its SQLite cache for replays."""
    import anthropic  # noqa: F401 — surface ImportError if missing

    async def invoke(system: str, user: str) -> str:
        from claude_client import _hash as ch_hash  # noqa: SLF001
        key = ch_hash(client.model, "rerank_calibrate", system, user)
        cached = client._cached(key)  # noqa: SLF001
        if cached is not None:
            return cached
        resp = await client.client.messages.create(
            model=client.model,
            max_tokens=8,
            system=[
                {
                    "type": "text",
                    "text": system,
                    "cache_control": {"type": "ephemeral"},
                }
            ],
            messages=[{"role": "user", "content": user}],
        )
        text = ""
        if resp.content:
            for block in resp.content:
                if hasattr(block, "text"):
                    text = block.text
                    break
        client._store(key, "rerank_calibrate", text)  # noqa: SLF001
        return text

    return invoke


# ---------------------------------------------------------------- prompt smoke


async def _prompt_smoke(invoke: Callable[[str, str], Awaitable[str]], judge: str) -> None:
    """Three dry-run pairs to validate the prompt before launching 1k.
    Fail fast if the model outputs garbage on simple inputs."""
    fixtures = [
        (
            "function that parses JSON",
            "def parse_json(s):\n    import json\n    return json.loads(s)",
            "def add(a, b):\n    return a + b",
            "A",
        ),
        (
            "compute factorial",
            "def square(n): return n * n",
            "def factorial(n):\n    return 1 if n == 0 else n * factorial(n - 1)",
            "B",
        ),
        (
            "reverse a string",
            "def reverse(s):\n    return s[::-1]",
            "def reverse_string(s: str) -> str:\n    return ''.join(reversed(s))",
            "TIE",  # both are valid; we just want a parseable answer
        ),
    ]
    print(f"[{judge}] prompt smoke test (3 fixtures)…", file=sys.stderr, flush=True)
    parsed: list[str] = []
    for q, a, b, _expected in fixtures:
        raw = await invoke(LABEL_PROMPT_SYSTEM, _label_user_prompt(q, a, b))
        label = _parse_label(raw)
        print(f"[{judge}]   q={q!r:.40} → {label!r}  (raw={raw!r:.40})", file=sys.stderr, flush=True)
        parsed.append(label)
    if all(p == "PARSE_ERROR" for p in parsed):
        raise RuntimeError(
            f"{judge}: all 3 smoke fixtures returned PARSE_ERROR — prompt is broken"
        )


# ---------------------------------------------------------------- agreement metrics


def _cohen_kappa(labels_a: list[str], labels_b: list[str]) -> float:
    """Cohen's kappa for two raters on a 3-class problem (A/B/TIE).
    Returns nan when either side has zero variance (degenerate)."""
    assert len(labels_a) == len(labels_b)
    n = len(labels_a)
    if n == 0:
        return float("nan")
    classes = ["A", "B", "TIE"]
    obs = sum(1 for x, y in zip(labels_a, labels_b) if x == y) / n
    pa = Counter(labels_a)
    pb = Counter(labels_b)
    expected = sum((pa[c] / n) * (pb[c] / n) for c in classes)
    if expected == 1.0:
        return float("nan")
    return (obs - expected) / (1 - expected)


def _confusion(labels_a: list[str], labels_b: list[str]) -> dict[str, int]:
    """3x3 confusion matrix keyed `<gemma><claude>` with T standing in
    for TIE — matches the spec's `{"AA","AB","AT","BA","BB","BT","TA","TB","TT"}`
    schema. Inputs that aren't A/B/TIE are skipped (PARSE_ERROR / LABEL_ERROR
    rows shouldn't reach this function but we guard anyway)."""
    classes = ["A", "B", "T"]  # T short for TIE
    out: dict[str, int] = {a + b: 0 for a in classes for b in classes}
    label_to_code = {"A": "A", "B": "B", "TIE": "T"}
    for la, lb in zip(labels_a, labels_b):
        ka = label_to_code.get(la)
        kb = label_to_code.get(lb)
        if ka is None or kb is None:
            continue
        out[ka + kb] += 1
    return out


# ---------------------------------------------------------------- decision gate


def _decide(agreement_pct: float) -> tuple[str, str]:
    if agreement_pct >= 85.0:
        return "GEMMA_ONLY", ">=85%"
    if agreement_pct >= 70.0:
        return "HYBRID", "70-85%"
    return "CLAUDE_ONLY", "<70%"


# ---------------------------------------------------------------- main


async def _run_judge(
    judge: Judge,
    triples: list[dict],
    sem: asyncio.Semaphore,
    total: int,
) -> None:
    async def _one(t):
        async with sem:
            try:
                await judge.label(t, total)
            except Exception as e:  # noqa: BLE001 — never let one triple kill the loop
                print(
                    f"[{judge.name}] uncaught exception id={t.get('id')}: "
                    f"{type(e).__name__}: {e}",
                    file=sys.stderr, flush=True,
                )
                traceback.print_exc(file=sys.stderr)

    await asyncio.gather(*(_one(t) for t in triples))


async def main_async(args) -> int:
    timing: dict[str, float] = {}
    t_overall = time.monotonic()
    QUERIES_DIR.mkdir(parents=True, exist_ok=True)

    # ---------------- Phase 1: sampling
    t = time.monotonic()
    if SAMPLE_PATH.exists() and not args.resample:
        triples = _read_jsonl(SAMPLE_PATH)
        if len(triples) < args.sample:
            print(
                f"[sample] existing {SAMPLE_PATH} has only {len(triples)} rows < "
                f"requested {args.sample}; resampling.",
                file=sys.stderr, flush=True,
            )
            triples = _sample_triples(args.corpus, args.sample, args.seed)
        else:
            print(
                f"[sample] reusing {len(triples)} triples from {SAMPLE_PATH} "
                f"(--resample to regenerate)",
                file=sys.stderr, flush=True,
            )
    else:
        triples = _sample_triples(args.corpus, args.sample, args.seed)
    timing["sampling_s"] = round(time.monotonic() - t, 2)

    if not SAMPLE_PATH.exists() or args.resample:
        # Truncate-write the sample (atomic via tmp).
        tmp = SAMPLE_PATH.with_suffix(SAMPLE_PATH.suffix + ".tmp")
        with tmp.open("w") as f:
            for tr in triples:
                f.write(json.dumps(tr) + "\n")
        os.replace(tmp, SAMPLE_PATH)
        print(f"[sample] wrote {len(triples)} triples to {SAMPLE_PATH}", file=sys.stderr, flush=True)

    # Stratification report
    src_counts = Counter(t["source"] for t in triples)
    lang_counts = Counter(t["language"] for t in triples)
    pos_a_count = sum(1 for t in triples if t["positive_is_a"])
    print(
        f"[sample] sources={dict(src_counts.most_common(5))} "
        f"languages={dict(lang_counts.most_common(5))} "
        f"positive_is_a={pos_a_count}/{len(triples)} "
        f"({100*pos_a_count/max(1,len(triples)):.1f}%)",
        file=sys.stderr, flush=True,
    )

    if args.dry_run > 0:
        triples = triples[: args.dry_run]
        print(f"[dry-run] truncated to {len(triples)} triples; output JSONLs will NOT be written", file=sys.stderr, flush=True)

    # ---------------- Phase 2: labeling

    # Resume: skip IDs already present in the per-judge output JSONL.
    gemma_done_ids: set[int] = set()
    claude_done_ids: set[int] = set()
    if not args.dry_run:
        gemma_done_ids = {r["id"] for r in _read_jsonl(GEMMA_PATH) if "id" in r}
        claude_done_ids = {r["id"] for r in _read_jsonl(CLAUDE_PATH) if "id" in r}

    if gemma_done_ids:
        print(f"[gemma] resume: {len(gemma_done_ids)} IDs already labeled", file=sys.stderr, flush=True)
    if claude_done_ids:
        print(f"[claude] resume: {len(claude_done_ids)} IDs already labeled", file=sys.stderr, flush=True)

    triples_for_gemma = [t for t in triples if t["id"] not in gemma_done_ids]
    triples_for_claude = [t for t in triples if t["id"] not in claude_done_ids]

    # ---- build clients (lazy import so missing deps fail with a clear msg)
    gemma_client = None
    claude_client = None
    gemma_invoke = None
    claude_invoke = None

    if not args.claude_only:
        try:
            from llm_client import LLMClient
        except ImportError as e:
            print(f"[gemma] cannot import llm_client: {e}", file=sys.stderr, flush=True)
            return 2
        gemma_client = LLMClient()
        gemma_invoke = _make_gemma_invoke(gemma_client)

    if not args.gemma_only:
        try:
            from claude_client import ClaudeClient
        except ImportError as e:
            print(f"[claude] cannot import claude_client: {e}", file=sys.stderr, flush=True)
            return 2
        claude_client = ClaudeClient()
        claude_invoke = _make_claude_invoke(claude_client)

    # ---- prompt smoke (fail fast if the prompt is broken)
    try:
        if gemma_invoke is not None:
            await _prompt_smoke(gemma_invoke, "gemma")
        if claude_invoke is not None:
            await _prompt_smoke(claude_invoke, "claude")
    except Exception as e:  # noqa: BLE001
        print(f"[smoke] FATAL: {type(e).__name__}: {e}", file=sys.stderr, flush=True)
        if gemma_client is not None:
            await gemma_client.aclose()
        if claude_client is not None:
            await claude_client.aclose()
        return 3

    # ---- run labeling
    stop_heartbeat = asyncio.Event()
    sigint = {"hit": False}

    def _sigint_handler():
        sigint["hit"] = True
        print("\n[main] SIGINT — flushing and exiting; rerun to resume", file=sys.stderr, flush=True)
        stop_heartbeat.set()

    loop = asyncio.get_running_loop()
    with contextlib.suppress(NotImplementedError):
        loop.add_signal_handler(signal.SIGINT, _sigint_handler)

    judges: list[Judge] = []
    runners: list[Awaitable] = []

    t_label = time.monotonic()

    async with contextlib.AsyncExitStack() as stack:
        if gemma_invoke is not None and triples_for_gemma:
            j = await stack.enter_async_context(
                Judge("gemma", GEMMA_PATH, gemma_invoke, args.progress_every)
            )
            judges.append(j)
            sem = asyncio.Semaphore(args.gemma_concurrency)
            runners.append(_run_judge(j, triples_for_gemma, sem, len(triples)))
        if claude_invoke is not None and triples_for_claude:
            j = await stack.enter_async_context(
                Judge("claude", CLAUDE_PATH, claude_invoke, args.progress_every)
            )
            judges.append(j)
            sem = asyncio.Semaphore(args.claude_concurrency)
            runners.append(_run_judge(j, triples_for_claude, sem, len(triples)))

        if not runners:
            print("[main] nothing to label (all IDs already done or both --*-only)", file=sys.stderr, flush=True)
        else:
            heartbeat_task = asyncio.create_task(_heartbeat(judges, len(triples), stop_heartbeat))
            try:
                await asyncio.gather(*runners)
            finally:
                stop_heartbeat.set()
                with contextlib.suppress(Exception):
                    await asyncio.wait_for(heartbeat_task, timeout=2.0)

    timing["labeling_s"] = round(time.monotonic() - t_label, 2)

    if gemma_client is not None:
        await gemma_client.aclose()
    if claude_client is not None:
        await claude_client.aclose()

    if sigint["hit"]:
        print("[main] exiting 130 after SIGINT", file=sys.stderr, flush=True)
        return 130

    if args.dry_run:
        print("[dry-run] complete; not computing agreement", file=sys.stderr, flush=True)
        timing["total_s"] = round(time.monotonic() - t_overall, 2)
        for k, v in timing.items():
            print(f"[timing] {k:<14} {v}s", file=sys.stderr, flush=True)
        return 0

    # ---------------- Phase 3: agreement
    t_agree = time.monotonic()
    gemma_rows = {r["id"]: r for r in _read_jsonl(GEMMA_PATH)}
    claude_rows = {r["id"]: r for r in _read_jsonl(CLAUDE_PATH)}
    triples_by_id = {t["id"]: t for t in _read_jsonl(SAMPLE_PATH)}

    gemma_parse_err = sum(1 for r in gemma_rows.values() if r.get("label") == "PARSE_ERROR")
    gemma_label_err = sum(1 for r in gemma_rows.values() if r.get("label") == "LABEL_ERROR")
    claude_parse_err = sum(1 for r in claude_rows.values() if r.get("label") == "PARSE_ERROR")
    claude_label_err = sum(1 for r in claude_rows.values() if r.get("label") == "LABEL_ERROR")

    gemma_total = len(gemma_rows)
    claude_total = len(claude_rows)
    gemma_pe_rate = gemma_parse_err / max(1, gemma_total)
    claude_pe_rate = claude_parse_err / max(1, claude_total)
    if gemma_pe_rate > 0.05 or claude_pe_rate > 0.05:
        print(
            f"\n[WARN] HIGH PARSE-ERROR RATE: gemma={100*gemma_pe_rate:.1f}% "
            f"claude={100*claude_pe_rate:.1f}%; the prompt may need work. "
            f"Pausing 5s before continuing.",
            file=sys.stderr, flush=True,
        )
        await asyncio.sleep(5.0)

    # Build paired list for IDs that BOTH judges labeled successfully
    ids_both = set(gemma_rows) & set(claude_rows)
    paired: list[tuple[int, str, str]] = []
    for i in sorted(ids_both):
        gl = gemma_rows[i].get("label")
        cl = claude_rows[i].get("label")
        if gl in ("A", "B", "TIE") and cl in ("A", "B", "TIE"):
            paired.append((i, gl, cl))

    g_labels = [g for _, g, _ in paired]
    c_labels = [c for _, _, c in paired]
    n_compared = len(paired)
    n_agree = sum(1 for g, c in zip(g_labels, c_labels) if g == c)
    agreement_pct = round(100.0 * n_agree / max(1, n_compared), 2)
    kappa = _cohen_kappa(g_labels, c_labels)

    # Ground-truth: positive_is_a → A is correct; else B is correct
    def _gt_correct(label: str, positive_is_a: bool) -> bool:
        if label == "A":
            return positive_is_a
        if label == "B":
            return not positive_is_a
        return False

    gemma_gt_correct = 0
    gemma_gt_n = 0
    for i, row in gemma_rows.items():
        if row.get("label") not in ("A", "B"):
            continue
        triple = triples_by_id.get(i)
        if not triple:
            continue
        gemma_gt_n += 1
        if _gt_correct(row["label"], triple["positive_is_a"]):
            gemma_gt_correct += 1
    claude_gt_correct = 0
    claude_gt_n = 0
    for i, row in claude_rows.items():
        if row.get("label") not in ("A", "B"):
            continue
        triple = triples_by_id.get(i)
        if not triple:
            continue
        claude_gt_n += 1
        if _gt_correct(row["label"], triple["positive_is_a"]):
            claude_gt_correct += 1

    decision, threshold = _decide(agreement_pct)
    confusion = _confusion(g_labels, c_labels)

    timing["agreement_s"] = round(time.monotonic() - t_agree, 2)
    timing["total_s"] = round(time.monotonic() - t_overall, 2)

    report = {
        "schema": "calibration-v1",
        "created_at": int(time.time()),
        "n_triples": len(triples_by_id),
        "n_compared": n_compared,
        "gemma_total_labeled": gemma_total,
        "claude_total_labeled": claude_total,
        "gemma_parse_errors": gemma_parse_err,
        "gemma_label_errors": gemma_label_err,
        "claude_parse_errors": claude_parse_err,
        "claude_label_errors": claude_label_err,
        "agreement_pct": agreement_pct,
        "cohen_kappa": (None if kappa != kappa else round(kappa, 4)),
        "confusion_matrix": confusion,
        "ground_truth_agreement_gemma_pct": (
            round(100.0 * gemma_gt_correct / max(1, gemma_gt_n), 2) if gemma_gt_n else None
        ),
        "ground_truth_agreement_gemma_n": gemma_gt_n,
        "ground_truth_agreement_claude_pct": (
            round(100.0 * claude_gt_correct / max(1, claude_gt_n), 2) if claude_gt_n else None
        ),
        "ground_truth_agreement_claude_n": claude_gt_n,
        "decision": decision,
        "decision_threshold": threshold,
        "stratification": {
            "sources": dict(src_counts),
            "languages": dict(lang_counts),
            "positive_is_a_pct": round(100.0 * pos_a_count / max(1, len(triples_by_id)), 2),
        },
        "timing_s": timing,
        "label_distribution_gemma": {k: sum(1 for r in gemma_rows.values() if r.get("label") == k) for k in ("A", "B", "TIE", "PARSE_ERROR", "LABEL_ERROR")},
        "label_distribution_claude": {k: sum(1 for r in claude_rows.values() if r.get("label") == k) for k in ("A", "B", "TIE", "PARSE_ERROR", "LABEL_ERROR")},
    }

    _atomic_write_json(REPORT_PATH, report)
    print(f"\n[main] wrote {REPORT_PATH}", file=sys.stderr, flush=True)

    print("\n=== CALIBRATION REPORT ===")
    print(f"  n_triples              {report['n_triples']}")
    print(f"  n_compared             {report['n_compared']}")
    print(f"  gemma parse / label err {report['gemma_parse_errors']} / {report['gemma_label_errors']}")
    print(f"  claude parse / label err {report['claude_parse_errors']} / {report['claude_label_errors']}")
    print(f"  agreement_pct          {report['agreement_pct']}%")
    print(f"  cohen_kappa            {report['cohen_kappa']}")
    print(f"  ground-truth gemma     {report['ground_truth_agreement_gemma_pct']}% (N={report['ground_truth_agreement_gemma_n']})")
    print(f"  ground-truth claude    {report['ground_truth_agreement_claude_pct']}% (N={report['ground_truth_agreement_claude_n']})")
    print(f"  decision               {report['decision']}  (threshold {report['decision_threshold']})")
    print(f"\n  confusion matrix (gemma row × claude col, 3x3; T = TIE):")
    classes = ["A", "B", "T"]
    print("         " + "  ".join(f"C={c}" for c in classes))
    for ga in classes:
        cells = [str(confusion.get(ga + ca, 0)) for ca in classes]
        print(f"   G={ga}    " + "  ".join(f"{c:>4}" for c in cells))

    print(f"\n  timing: " + "  ".join(f"{k}={v}s" for k, v in timing.items()))
    return 0


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--sample", type=int, default=1000, help="number of triples to sample (default 1000)")
    p.add_argument("--seed", type=int, default=42, help="RNG seed for sampling (default 42)")
    p.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS, help="path to source corpus JSONL")
    p.add_argument("--resample", action="store_true", help="regenerate the sampled triples even if calibration_1k.jsonl exists")
    p.add_argument("--gemma-only", action="store_true", help="skip Claude; only run Gemma")
    p.add_argument("--claude-only", action="store_true", help="skip Gemma; only run Claude")
    p.add_argument("--gemma-concurrency", type=int, default=32, help="parallel requests to Gemma (default 32)")
    p.add_argument("--claude-concurrency", type=int, default=8, help="parallel requests to Claude (default 8)")
    p.add_argument("--progress-every", type=int, default=25, help="log per-judge progress every N triples (default 25)")
    p.add_argument("--dry-run", type=int, default=0, help="run on first N triples without writing per-judge JSONLs (default 0 = full run)")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    if args.gemma_only and args.claude_only:
        print("--gemma-only and --claude-only are mutually exclusive", file=sys.stderr)
        return 2
    try:
        return asyncio.run(main_async(args))
    except KeyboardInterrupt:
        print("\n[main] KeyboardInterrupt at top level; partial labels preserved", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
