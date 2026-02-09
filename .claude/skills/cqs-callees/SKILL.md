---
name: cqs-callees
description: Find functions called by a given function. Dependency analysis.
disable-model-invocation: false
argument-hint: "<function_name>"
---

# Callees

Parse arguments:
- First positional arg = function name (required)

Run via Bash: `cqs callees "<name>" --json -q`

Present the results to the user. Shows all functions called by the named function. Useful for understanding dependencies.
