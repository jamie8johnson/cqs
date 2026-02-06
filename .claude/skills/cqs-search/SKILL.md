---
name: cqs-search
description: Semantic code search. Finds functions by concept, not text. Use instead of grep/glob.
disable-model-invocation: false
argument-hint: "<query> [--lang rust] [--limit 10] [--name-only] [--semantic-only]"
---

# Search

Semantic code search using `cqs_search` MCP tool.

## Usage

Call `cqs_search` with the user's query. Parse arguments from the invocation:

- First positional arg = `query` (required)
- `--lang <language>` → `language` filter (rust, python, typescript, javascript, go, c, java)
- `--limit <n>` → `limit` (default 5, max 20)
- `--threshold <n>` → `threshold` (default 0.3)
- `--name-only` → `name_only: true` — definition search, skips embedding. Use for "where is X defined?"
- `--semantic-only` → `semantic_only: true` — pure vector similarity, no hybrid RRF
- `--path <glob>` → `path_pattern` filter (e.g., `src/mcp/**`)
- `--sources <name,...>` → `sources` array — filter which indexes (e.g., `project`, reference names)

## Examples

- `/cqs-search retry with exponential backoff` — find retry logic by concept
- `/cqs-search Store::open --name-only` — find where Store::open is defined
- `/cqs-search error handling --lang rust --path src/mcp/**` — scoped search
- `/cqs-search authentication --sources project,stdlib` — multi-index search
