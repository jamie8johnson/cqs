---
name: cqs-test-map
description: Map functions to tests that exercise them via reverse call graph traversal.
disable-model-invocation: false
argument-hint: "<function_name> [--depth 5]"
---

# Test Map

Call `cqs_test_map` MCP tool. Parse arguments:

- First positional arg = `name` — function to find tests for (required)
- `--depth <n>` → max reverse BFS depth (default 5)

Returns tests reachable via reverse call graph with full call chains. Useful before refactoring to know which tests to run.
