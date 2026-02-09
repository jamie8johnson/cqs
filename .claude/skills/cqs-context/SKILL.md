---
name: cqs-context
description: Module-level understanding — chunks, callers, callees, notes for a file.
disable-model-invocation: false
argument-hint: "<file_path>"
---

# Context

Parse arguments:
- First positional arg = file path (required)
- `--summary` — return counts only instead of full details

Run via Bash: `cqs context "<path>" [--summary] --json -q`

Present the results to the user. Returns a module overview: all chunks (signatures), external callers, external callees, dependent files, and related notes. Useful for understanding a file's role before making changes.
