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
# Default matches the `--served-model-name` flag used when launching vLLM.
# If you relaunch without that flag, set VLLM_MODEL to the full HF path.
VLLM_MODEL = os.environ.get("VLLM_MODEL", "gemma-4-31b")
CACHE_PATH = Path(os.environ.get("LLM_CACHE", os.path.expanduser("~/.cache/cqs/llm-cache.db")))

_GENERATE_CATEGORY_HINTS: dict[str, str] = {
    "cross_language": (
        "The chunk is written in one language (e.g. Rust). Generate queries "
        "that explicitly compare to other languages or ask about the same "
        "pattern across languages. Use phrasings like 'Python equivalent of', "
        "'across different languages', 'in Go vs Rust', 'translate to X', "
        "'how does $other_lang do this'. Pick languages appropriate to the "
        "chunk's concept."
    ),
    "multi_step": (
        "Generate queries that conjoin two or more conditions with 'and'/'or'. "
        "Examples: 'callers AND affected tests', 'functions that return X "
        "AND take Y', 'structs that implement Foo OR derive Bar'. Each query "
        "must contain at least two distinct retrieval criteria joined by "
        "AND/OR."
    ),
    "structural_search": (
        "Generate queries about the STRUCTURAL pattern of the chunk — return "
        "types, generic bounds, visibility, trait impls, enum variants, "
        "lifetime params, async-ness — NOT the name or behavior. Phrase as "
        "'functions that return Result<T>', 'async methods on Store', "
        "'structs with lifetime parameters', 'enums with tagged variants'."
    ),
    "negation": (
        "Generate queries with an explicit exclusion using 'not' or 'without'. "
        "Examples: 'sort without allocating', 'parser that is not recursive', "
        "'store that is read-only not writable'. The exclusion must be the "
        "key constraint."
    ),
    "type_filtered": (
        "Generate queries that filter by code kind or role: 'all test "
        "functions', 'impl blocks on Store', 'methods on the parser', "
        "'enum variants for errors'. The query must name the kind (function, "
        "method, struct, impl, enum, test, module) as the filter."
    ),
    "identifier_lookup": (
        "Generate queries that are the identifier name itself, possibly with "
        "a few descriptor words. The query should be dominated by the "
        "specific identifier — not a description of behavior."
    ),
    "behavioral_search": (
        "Generate queries that describe what the chunk DOES using verbs or "
        "'how does X' phrasing. Examples: 'how does the watcher handle "
        "errors', 'function that validates user input', 'code that caches "
        "embeddings'."
    ),
    "conceptual_search": (
        "Generate queries about an ABSTRACT concept the chunk exemplifies, "
        "no specific identifier. Examples: 'dependency injection pattern', "
        "'rate limiting with backoff', 'lazy initialization'."
    ),
}


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
            "Mission: you are labeling queries in a code-search eval dataset "
            "that will train a query classifier and measure retrieval quality. "
            "Each query has exactly one best category — pick the one that "
            "captures the query's retrieval intent most specifically.\n"
            "\n"
            "You classify a code-search query into EXACTLY ONE of these categories. "
            "Return ONLY the snake_case category name, no quotes, no prose.\n"
            "\n"
            "Categories:\n"
            "- identifier_lookup: the query IS a name (function, type, variable, "
            "method) — including a bare snake_case or CamelCase identifier alone, "
            "an identifier with a few descriptor words, or pipe-separated identifiers. "
            "A single word like `search_filtered` or `BlameEntry` is identifier_lookup.\n"
            "- behavioral_search: describes what code DOES using verbs or 'how does X' "
            "phrasing (e.g. 'how the watcher handles errors', 'validates user input').\n"
            "- conceptual_search: an abstract concept with no specific identifier "
            "('dependency injection pattern', 'lazy loading').\n"
            "- type_filtered: filters by code kind or role ('all test functions', "
            "'impl blocks on Store', 'methods on the parser').\n"
            "- cross_language: explicitly spans multiple languages ('Python equivalent of "
            "map in Rust', 'extract doc comments across languages').\n"
            "- structural_search: looks for a code structural pattern "
            "('functions that return Result', 'structs with lifetime parameters').\n"
            "- negation: contains 'not X' or 'without Y' constraint.\n"
            "- multi_step: two or more conjoined conditions joined by AND/OR ('callers "
            "AND affected tests', 'shared callers or shared types').\n"
            "- unknown: the query is not a code-search query, is test junk, or "
            "genuinely matches none of the above.\n"
            "\n"
            "Tie-breakers:\n"
            "1. If the query is (or begins with) a bare identifier name, return "
            "identifier_lookup even when surrounded by descriptors like "
            "'sort determinism' or 'tie-breaker'.\n"
            "2. If the query has 'how does X do Y' phrasing, prefer behavioral_search "
            "over multi_step unless there are clearly two separate conditions.\n"
            "3. negation wins over the verb-based category whenever 'not' or 'without' "
            "is the key constraint.\n"
            "\n"
            "Examples:\n"
            "  search_filtered                              -> identifier_lookup\n"
            "  rrf_fuse sort determinism tie-breaker        -> identifier_lookup\n"
            "  BlameEntry                                   -> identifier_lookup\n"
            "  how does the watcher handle errors           -> behavioral_search\n"
            "  dependency injection pattern                 -> conceptual_search\n"
            "  all test functions                           -> type_filtered\n"
            "  Python equivalent of map in Rust             -> cross_language\n"
            "  functions that return Result                 -> structural_search\n"
            "  sort without allocating                      -> negation\n"
            "  callers and affected tests for a function    -> multi_step"
        )
        resp = await self._chat(system, f"Query: {query}", role="classify", max_tokens=16)
        norm = resp.lower().strip().strip("`\"'").split()[0] if resp.strip() else ""
        for c in CATEGORIES:
            if norm == c:
                return c
        # Second-chance: substring match on the full response (in case the model
        # prepended "Category:" or similar).
        lower = resp.lower()
        for c in CATEGORIES:
            if c in lower:
                return c
        return "unknown"

    async def generate(
        self,
        signature: str,
        preview: str,
        *,
        n: int = 3,
        category: str | None = None,
        language: str | None = None,
    ) -> list[str]:
        """Generate realistic queries that should retrieve this chunk.

        When `category` is set, the prompt shapes the queries toward that
        category's phrasing conventions (e.g. cross_language explicitly
        references other languages). Always call `classify()` on each returned
        query to filter out off-target drift.
        """
        base = (
            "Mission: you are generating queries for a code-search eval "
            "dataset. Each query must be (1) unambiguous for its target "
            "category, (2) realistic — something a human developer would "
            "actually type, (3) specific enough that the given chunk is "
            "plausibly the top-1 result. Avoid borderline or multi-category "
            "phrasings.\n"
            "\n"
            "You generate realistic code-search queries that a developer would "
            "type and expect the given chunk to be the top-1 result. Return a "
            "JSON array of strings. No markdown fences, no prose, no numbering."
        )
        cat_hint = _GENERATE_CATEGORY_HINTS.get(category or "", "") if category else ""
        system = base + ("\n\n" + cat_hint if cat_hint else "")
        lang_hint = f"Language: {language}\n" if language else ""
        user = (
            f"{lang_hint}"
            f"Signature: {signature}\n\n"
            f"Preview:\n{preview}\n\n"
            f"Produce exactly {n} diverse queries."
            + (
                ""
                if not category
                else f" Every query must be phrased so its category is "
                     f"`{category}` — re-read the category hint above."
            )
        )
        # Cache key must include category so category-tagged prompts stay distinct.
        role = f"generate:{category}" if category else "generate"
        resp = await self._chat(system, user, role=role, max_tokens=320)
        text = resp.strip()
        if text.startswith("```"):
            text = text.split("\n", 1)[1].rsplit("```", 1)[0]
        try:
            arr = json.loads(text)
            return [str(q).strip() for q in arr if str(q).strip()][:n]
        except json.JSONDecodeError:
            lines = [ln.strip("-* 0123456789.").strip() for ln in text.splitlines() if ln.strip()]
            return [ln for ln in lines if len(ln) > 2][:n]

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
