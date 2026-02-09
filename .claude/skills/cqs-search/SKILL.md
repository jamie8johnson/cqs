---
name: cqs-search
description: Semantic code search. Finds functions by concept, not text. Use instead of grep/glob.
disable-model-invocation: false
argument-hint: "<query> [--lang rust] [--limit 10] [--name-only] [--semantic-only]"
---

# Search

Parse arguments from the invocation:

- First positional arg = query (required)
- `--lang <language>` → `--lang` filter (rust, python, typescript, javascript, go, c, java, sql, markdown)
- `--limit <n>` → `-n` (default 5, max 20)
- `--threshold <n>` → `-t` (default 0.3)
- `--name-only` → definition search, skips embedding. Use for "where is X defined?"
- `--semantic-only` → pure vector similarity, no hybrid RRF
- `--path <glob>` → `-p` path pattern filter (e.g., `src/mcp/**`)
- `--chunk-type <T>` → `--chunk-type` (function, method, class, struct, enum, trait, interface, constant, section)
- `--pattern <P>` → `--pattern` (builder, error_swallow, async, mutex, unsafe, recursion)
- `--note-only` → return only notes, skip code search
- `--note-weight <F>` → weight for note scores 0.0-1.0 (default 1.0)

Run via Bash: `cqs "<query>" [flags] --json -q`

Present the results to the user.

## Examples

- `/cqs-search retry with exponential backoff` — find retry logic by concept
- `/cqs-search Store::open --name-only` — find where Store::open is defined
- `/cqs-search error handling --lang rust --path src/mcp/**` — scoped search
