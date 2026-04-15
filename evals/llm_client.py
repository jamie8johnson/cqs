#!/usr/bin/env python3
"""LLM client for eval expansion, labeling, and validation.

Connects to a local OpenAI-compatible server (typically vLLM serving
Gemma 4 31B AWQ at http://localhost:8000). Caches prompts by hash in
SQLite (~/.cache/cqs/llm-cache.db) so repeat runs don't re-spend compute.

Three prompt modes:
    classify(query)            -> category  (one of CATEGORIES)
    generate(signature, body)  -> list[str] (queries)
    validate(query, sig, body) -> bool      (does chunk answer query)

Env vars:
    VLLM_URL   (default http://localhost:8000/v1)
    VLLM_MODEL (default cyankiwi/gemma-4-31B-it-AWQ-4bit)
    LLM_CACHE  (default ~/.cache/cqs/llm-cache.db)
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import sqlite3
import time
from pathlib import Path

from openai import AsyncOpenAI

CATEGORIES = [
    "identifier_lookup",
    "behavioral_search",
    "conceptual_search",
    "type_filtered",
    "cross_language",
    "structural_search",
    "negation",
    "multi_step",
    "unknown",
]

VLLM_URL = os.environ.get("VLLM_URL", "http://localhost:8000/v1")
VLLM_MODEL = os.environ.get("VLLM_MODEL", "cyankiwi/gemma-4-31B-it-AWQ-4bit")
CACHE_PATH = Path(os.environ.get("LLM_CACHE", os.path.expanduser("~/.cache/cqs/llm-cache.db")))


def _hash(*parts: str) -> str:
    h = hashlib.blake2b(digest_size=16)
    for p in parts:
        h.update(p.encode("utf-8"))
        h.update(b"\x00")
    return h.hexdigest()


class LLMClient:
    def __init__(self, url: str = VLLM_URL, model: str = VLLM_MODEL, cache_path: Path = CACHE_PATH):
        self.url = url
        self.model = model
        self.client = AsyncOpenAI(base_url=url, api_key="local-vllm", timeout=600.0)
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

    async def _chat(self, system: str, user: str, role: str, *, max_tokens: int = 512, temperature: float = 0.0) -> str:
        key = _hash(self.model, role, system, user, str(temperature), str(max_tokens))
        if (c := self._cached(key)) is not None:
            return c
        r = await self.client.chat.completions.create(
            model=self.model,
            messages=[
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            temperature=temperature,
            max_tokens=max_tokens,
        )
        out = (r.choices[0].message.content or "").strip()
        self._store(key, role, out)
        return out

    async def classify(self, query: str) -> str:
        system = (
            "You classify code-search queries into exactly one category:\n"
            + "\n".join(f"- {c}" for c in CATEGORIES)
            + "\nReturn ONLY the category name (snake_case, no quotes, no prose)."
        )
        resp = await self._chat(system, f"Query: {query}", role="classify", max_tokens=16)
        norm = resp.lower().strip().strip("`\"'")
        for c in CATEGORIES:
            if norm == c or norm.startswith(c) or c in norm.split():
                return c
        return "unknown"

    async def generate(self, signature: str, preview: str, n: int = 3) -> list[str]:
        system = (
            "You generate realistic code-search queries that a developer would type "
            "and expect the given chunk to be the top result.\n"
            "Return a JSON array of strings. No markdown fences, no prose."
        )
        user = (
            f"Signature: {signature}\n\n"
            f"Preview:\n{preview}\n\n"
            f"Produce exactly {n} diverse queries: mix natural-language intent, "
            "keyword-style, and code-aware phrasing."
        )
        resp = await self._chat(system, user, role="generate", max_tokens=320)
        text = resp.strip()
        if text.startswith("```"):
            text = text.split("\n", 1)[1].rsplit("```", 1)[0]
        try:
            arr = json.loads(text)
            return [str(q).strip() for q in arr if str(q).strip()][:n]
        except json.JSONDecodeError:
            lines = [ln.strip("-* ").strip() for ln in text.splitlines() if ln.strip()]
            return lines[:n]

    async def validate(self, query: str, signature: str, preview: str) -> bool:
        system = 'You judge whether a code chunk answers a search query. Reply ONLY "yes" or "no".'
        user = f"Query: {query}\n\nSignature: {signature}\n\nPreview:\n{preview}\n\nDoes this chunk answer the query?"
        resp = await self._chat(system, user, role="validate", max_tokens=8)
        return resp.strip().lower().startswith("y")

    async def aclose(self) -> None:
        await self.client.close()
        self.conn.close()


async def _smoke() -> None:
    c = LLMClient()
    sig = "pub fn parse_config(path: &Path) -> Result<Config>"
    body = "fn parse_config(path: &Path) -> Result<Config> {\n    let text = fs::read_to_string(path)?;\n    Ok(toml::from_str(&text)?)\n}"
    q = "What function parses the config file?"
    print(f"server : {VLLM_URL}")
    print(f"model  : {VLLM_MODEL}")
    print(f"cache  : {CACHE_PATH}")
    print(f"classify : {await c.classify(q)}")
    print(f"generate : {await c.generate(sig, body, n=3)}")
    print(f"validate : {await c.validate(q, sig, body)}")
    await c.aclose()


if __name__ == "__main__":
    asyncio.run(_smoke())
