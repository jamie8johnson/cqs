---
name: cqs-callers
description: Find functions that call a given function. Impact analysis.
disable-model-invocation: false
argument-hint: "<function_name>"
---

# Callers

Parse arguments:
- First positional arg = function name (required)

Run via Bash: `cqs callers "<name>" --json -q`

Present the results to the user. Shows all call sites for the named function. Useful for impact analysis, dead code checks, and understanding usage patterns.
