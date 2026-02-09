---
name: cqs-read
description: Read a file with contextual notes injected as comments. Richer than raw Read.
disable-model-invocation: false
argument-hint: "<path> [--focus function]"
---

# CQS Read

Parse arguments:

- First positional arg = path (relative to project root, required)
- `--focus <function>` → return only the target function + its type dependencies instead of the whole file

Run via Bash: `cqs read <path> [--focus function] --json -q`

Returns the file contents with relevant notes and observations injected as comments at the appropriate locations. Use instead of raw `Read` tool when you want contextual awareness from prior sessions.

Present the results to the user.
