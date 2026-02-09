---
name: cqs-test-map
description: Map functions to tests that exercise them via reverse call graph traversal.
disable-model-invocation: false
argument-hint: "<function_name> [--depth 5]"
---

# Test Map

Parse arguments:
- First positional arg = function name to find tests for (required)
- `--depth <n>` — max reverse BFS depth (default 5)

Run via Bash: `cqs test-map "<name>" [--depth N] --json -q`

Present the results to the user. Returns tests reachable via reverse call graph with full call chains. Useful before refactoring to know which tests to run.
