---
name: cqs-explain
description: Function card — signature, docs, callers, callees, similar functions in one call.
disable-model-invocation: false
argument-hint: "<function_name>"
---

# Explain

Parse arguments:
- First positional arg = function name or `file:function` (required)

Run via Bash: `cqs explain "<name>" --json -q`

Present the results to the user. Returns a comprehensive function card: signature, docstring, callers, callees, and similar functions. Collapses 4+ separate lookups into 1.
