---
name: cqs-gather
description: Smart context assembly — seed search + call graph BFS expansion.
disable-model-invocation: false
argument-hint: "<query> [--expand 1] [--direction both|callers|callees] [--limit 10]"
---

# Gather

Parse arguments:

- First positional arg = query (required)
- `--expand <n>` → BFS expansion depth (default 1, max 5)
- `--direction <d>` → expansion direction: both, callers, callees (default both)
- `--limit <n>` → `-n` max results (default 10)

Run via Bash: `cqs gather "<query>" [flags] --json -q`

Returns seed search results expanded via call graph traversal. One call for "show me everything related to X". Cap: 200 nodes.

Present the results to the user.
