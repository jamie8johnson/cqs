---
name: cqs-related
description: Find functions related by shared callers, callees, or types.
disable-model-invocation: false
argument-hint: "<function_name>"
---

# Related

Parse arguments:
- First positional arg = function name or file:function (required)
- `-n` / `--limit` = max results per category (default 5)

Run via Bash: `cqs related "<name>" --json -q`

Present the results to the user. Returns three co-occurrence dimensions:
1. **Shared callers** — functions called by the same callers as the target
2. **Shared callees** — functions that call the same things as the target
3. **Shared types** — functions using the same custom types in their signatures

Use this to find what else needs review when touching a function.
