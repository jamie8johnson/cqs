---
name: cqs-context
description: Module-level understanding — chunks, callers, callees, notes for a file.
disable-model-invocation: false
argument-hint: "<file_path>"
---

# Context

Parse arguments:
- First positional arg = file path (required)
- `--compact` — signatures-only TOC with caller/callee counts per chunk (no code bodies, no individual caller names). Best for quick "what's in this file?" before drilling in.
- `--summary` — return counts only instead of full details

Run via Bash: `cqs context "<path>" [--compact] [--summary] --json -q`

Present the results to the user. Returns a module overview: all chunks (signatures), external callers, external callees, dependent files, and related notes. Useful for understanding a file's role before making changes. Use `--compact` when you just need the TOC.
