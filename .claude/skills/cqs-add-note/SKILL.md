---
name: cqs-add-note
description: Add a note to project memory. Indexed immediately for future search.
disable-model-invocation: false
argument-hint: "<text> [--sentiment -1|-0.5|0|0.5|1] [--mentions file1,file2]"
---

# Add Note

Call `cqs_add_note` MCP tool. Parse arguments:

- First positional arg (or quoted string) = `text` (required)
- `--sentiment <value>` → only discrete values: -1, -0.5, 0, 0.5, 1
- `--mentions <file1,file2,...>` → array of file paths or concepts

## Sentiment guide

| Value | Meaning |
|-------|---------|
| `-1` | Serious pain (broke something, lost time) |
| `-0.5` | Notable pain (friction, annoyance) |
| `0` | Neutral observation |
| `0.5` | Notable gain (useful pattern) |
| `1` | Major win (saved significant time/effort) |

Notes are indexed immediately and surface in future `cqs_search` results.
