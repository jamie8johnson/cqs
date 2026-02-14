---
name: cqs-search
description: Semantic code search. Finds functions by concept, not text. Use instead of grep/glob.
disable-model-invocation: false
argument-hint: "<query> [--lang rust] [--limit 10] [--name-only] [--semantic-only] [--rerank]"
---

# Search

Parse arguments from the invocation:

- First positional arg = query (required)
- `--lang <language>` → `--lang` filter (rust, python, typescript, javascript, go, c, java, sql, markdown)
- `--limit <n>` → `-n` (default 5, max 20)
- `--threshold <n>` → `-t` (default 0.3)
- `--name-only` → definition search, skips embedding. Use for "where is X defined?"
- `--semantic-only` → pure vector similarity, no hybrid RRF
- `--rerank` → re-rank results with cross-encoder (slower, more accurate)
- `--path <glob>` → `-p` path pattern filter (e.g., `src/cli/**`)
- `--chunk-type <T>` → `--chunk-type` (function, method, class, struct, enum, trait, interface, constant, section)
- `--pattern <P>` → `--pattern` (builder, error_swallow, async, mutex, unsafe, recursion)
- `--note-only` → return only notes, skip code search
- `--note-weight <F>` → weight for note scores 0.0-1.0 (default 1.0)
- `--ref <name>` → search only this reference index (skip project index)
- `--tokens <N>` → token budget (packs highest-scoring results into budget)
- `--expand` → expand results with parent context (small-to-big retrieval)
- `-C/--context <N>` → show N lines of context before/after the chunk
- `--no-content` → show only file:line, no code
- `--no-stale-check` → skip per-file staleness checks

Run via Bash: `cqs "<query>" [flags] --json -q`

Present the results to the user.

## Examples

- `/cqs-search retry with exponential backoff` — find retry logic by concept
- `/cqs-search Store::open --name-only` — find where Store::open is defined
- `/cqs-search error handling --lang rust --path src/cli/**` — scoped search
- `/cqs-search "query routing" --rerank` — cross-encoder re-ranking for precision
- `/cqs-search "config parsing" --ref aveva` — search only a named reference
