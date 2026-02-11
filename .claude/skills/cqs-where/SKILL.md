---
name: cqs-where
description: Suggest where to add new code based on semantic similarity and local patterns.
disable-model-invocation: false
argument-hint: "<description>"
---

# Where

Parse arguments:
- First positional arg = description of the code to add (required)

Run via Bash: `cqs where "<description>" --json -q`

Returns file suggestions ranked by relevance. Each suggestion includes:
- File path and insertion line
- Nearest similar function
- Local patterns (imports, error handling, naming convention, visibility, inline tests)

Use this to decide where to place new code before starting implementation.
