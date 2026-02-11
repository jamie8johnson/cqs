---
name: cqs-stale
description: Check index freshness — list files modified since last index.
disable-model-invocation: false
argument-hint: "[--count-only]"
---

# Stale

Check if the search index is up to date. Reports files that have changed on disk since they were last indexed, and files in the index that no longer exist.

Parse arguments:
- `--count-only` → show counts only, skip file list
- No other arguments needed

Run via Bash: `cqs stale --json -q`

Present the results to the user. If stale or missing files are found, suggest running `cqs index` to update.

## Examples

- `/cqs-stale` — check if index is fresh
- `/cqs-stale --count-only` — just get counts, no file list
