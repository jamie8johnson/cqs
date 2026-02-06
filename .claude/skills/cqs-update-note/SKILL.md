---
name: cqs-update-note
description: Update an existing note's text, sentiment, or mentions.
disable-model-invocation: false
argument-hint: "<exact_text> [--new-text ...] [--sentiment N] [--mentions ...]"
---

# Update Note

Call `cqs_update_note` MCP tool. Parse arguments:

- First positional arg = `text` (exact match to find the note)
- `--new-text <text>` → `new_text` replacement
- `--sentiment <value>` → `new_sentiment` (-1, -0.5, 0, 0.5, 1)
- `--mentions <file1,file2,...>` → `new_mentions` replacement array

All update fields are optional — omit to keep current value.
