---
name: cqs-similar
description: Find code similar to a given function. Search by example.
disable-model-invocation: false
argument-hint: "<function_name> [--limit 5] [--threshold 0.3] [--lang rust]"
---

# Similar

Parse arguments:
- First positional arg = `target` — function name or `file:function` (required)
- `--limit <n>` — max results (default 5, max 20)
- `--threshold <n>` — minimum similarity (default 0.3)
- `--lang <language>` — filter by language

Run via Bash: `cqs similar "<target>" [--limit N] [--threshold F] [--lang L] --json -q`

Present the results to the user. Useful for refactoring discovery, finding duplicates, and understanding patterns.
