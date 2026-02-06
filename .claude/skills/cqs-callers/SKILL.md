---
name: cqs-callers
description: Find functions that call a given function. Impact analysis.
disable-model-invocation: false
argument-hint: "<function_name>"
---

# Callers

Call `cqs_callers` MCP tool with `name` set to the user's argument.

Shows all call sites for the named function. Useful for:
- Impact analysis before refactoring
- Verifying a function is actually called (dead code check)
- Understanding how a function is used
