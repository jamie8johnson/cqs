---
name: cqs-update-note
description: Update an existing note's text, sentiment, or mentions.
disable-model-invocation: false
argument-hint: "<exact_text> [--new-text ...] [--new-sentiment N] [--new-mentions ...]"
---

# Update Note

Parse arguments:

- First positional arg = text (exact match to find the note)
- `--new-text <text>` → replacement text
- `--new-sentiment <value>` → replacement sentiment (-1, -0.5, 0, 0.5, 1)
- `--new-mentions <file1,file2,...>` → replacement mentions

Run via Bash: `cqs notes update "<text>" [--new-text "..."] [--new-sentiment N] [--new-mentions a,b] -q`

All update fields are optional — omit to keep current value.
