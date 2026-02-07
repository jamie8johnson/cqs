---
name: cqs-dead
description: Find functions/methods never called by indexed code.
disable-model-invocation: false
argument-hint: "[--include-pub]"
---

# Dead Code

Call `cqs_dead` MCP tool. Parse arguments:

- `--include-pub` → include public API functions (excluded by default)

Finds functions and methods with no callers in the indexed codebase. Excludes main, test functions, and trait implementations by default. Useful for cleanup and maintenance.
