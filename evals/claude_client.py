#!/usr/bin/env python3
"""Anthropic Claude client for eval validation.

Mirrors LLMClient.validate() so validate_gold.py can swap backends when
local vLLM is impractical (e.g. WSL+CUDA stability issues with 31B AWQ).
Only `validate(query, signature, preview) -> bool` is implemented — the
generation / classification side stays on the local model where cost
and rate limits don't matter.

Optimizations:
  - **Local SQLite cache** (~/.cache/cqs/claude-cache.db) — same blake3
    keying scheme as LLMClient. Repeat runs with the same prompt are free.
  - **Prompt caching** on the system message via `cache_control: ephemeral`.
    After the first call within the 5-min TTL, subsequent system-token
    cost drops to 0.1× base. Validate calls use a fixed system prompt,
    so cache hit rate trends to 99 %+ during a single batch run.

Env vars:
  ANTHROPIC_API_KEY   required. Picked up by the SDK automatically.
  CLAUDE_MODEL        default `claude-haiku-4-5`. Override e.g. for sonnet.
  CLAUDE_CACHE        default `~/.cache/cqs/claude-cache.db`.
"""

from __future__ import annotations

import asyncio
import hashlib
import logging
import os
import sqlite3
import time
from pathlib import Path

import anthropic

DEFAULT_MODEL = os.environ.get("CLAUDE_MODEL", "claude-haiku-4-5")
DEFAULT_CACHE = Path(os.environ.get("CLAUDE_CACHE", os.path.expanduser("~/.cache/cqs/claude-cache.db")))

VALIDATE_SYSTEM = (
    "You judge whether a code chunk answers a code-search query. "
    "Reply ONLY with 'yes' or 'no' — no punctuation, no prose, no explanation."
)

log = logging.getLogger("claude_client")


def _hash(*parts: str) -> str:
    h = hashlib.blake2b(digest_size=16)
    for p in parts:
        h.update(p.encode("utf-8"))
        h.update(b"\x00")
    return h.hexdigest()


class ClaudeClient:
    """Async validate() against the Anthropic API.

    `max_retries` controls how aggressively the SDK weathers 429 / 5xx —
    we set it high (8) because rate limits are expected during a long
    batch run and the SDK's exponential backoff is the right answer.
    """

    def __init__(
        self,
        model: str = DEFAULT_MODEL,
        cache_path: Path = DEFAULT_CACHE,
        max_retries: int = 8,
    ):
        if not os.environ.get("ANTHROPIC_API_KEY"):
            raise RuntimeError("ANTHROPIC_API_KEY not set; ClaudeClient cannot start")
        self.model = model
        self.client = anthropic.AsyncAnthropic(max_retries=max_retries)
        cache_path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(cache_path)
        self.conn.execute(
            """CREATE TABLE IF NOT EXISTS llm_cache (
                key   TEXT PRIMARY KEY,
                role  TEXT NOT NULL,
                resp  TEXT NOT NULL,
                model TEXT NOT NULL,
                ts    INTEGER NOT NULL
            )"""
        )
        self.conn.commit()

    def _cached(self, key: str) -> str | None:
        row = self.conn.execute("SELECT resp FROM llm_cache WHERE key = ?", (key,)).fetchone()
        return row[0] if row else None

    def _store(self, key: str, role: str, resp: str) -> None:
        self.conn.execute(
            "INSERT OR REPLACE INTO llm_cache(key, role, resp, model, ts) VALUES(?,?,?,?,?)",
            (key, role, resp, self.model, int(time.time())),
        )
        self.conn.commit()

    async def validate(self, query: str, signature: str, preview: str) -> bool:
        user = (
            f"Query: {query}\n\n"
            f"Signature: {signature}\n\n"
            f"Preview:\n{preview}\n\n"
            "Does this chunk answer the query?"
        )
        key = _hash(self.model, "validate", VALIDATE_SYSTEM, user)
        if (c := self._cached(key)) is not None:
            return c.lower().startswith("y")
        # Anthropic SDK auto-retries 429/5xx with backoff up to max_retries.
        resp = await self.client.messages.create(
            model=self.model,
            max_tokens=4,
            system=[
                {
                    "type": "text",
                    "text": VALIDATE_SYSTEM,
                    "cache_control": {"type": "ephemeral"},
                }
            ],
            messages=[{"role": "user", "content": user}],
        )
        text = ""
        if resp.content:
            for block in resp.content:
                if hasattr(block, "text"):
                    text = block.text.strip().lower()
                    break
        self._store(key, "validate", text)
        return text.startswith("y")

    async def aclose(self) -> None:
        # AsyncAnthropic close is idempotent.
        await self.client.close()
        self.conn.close()


async def _smoke() -> None:
    c = ClaudeClient()
    print(f"model: {c.model}")
    sig = "pub fn parse_config(path: &Path) -> Result<Config>"
    body = "fn parse_config(path: &Path) -> Result<Config> {\n    let text = fs::read_to_string(path)?;\n    Ok(toml::from_str(&text)?)\n}"
    print("yes case:", await c.validate("What function parses the config?", sig, body))
    print("no case :", await c.validate("Function that sorts integers", sig, body))
    await c.aclose()


if __name__ == "__main__":
    asyncio.run(_smoke())
