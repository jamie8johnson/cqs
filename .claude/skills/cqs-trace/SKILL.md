---
name: cqs-trace
description: Follow a call chain between two functions. BFS shortest path through the call graph.
disable-model-invocation: false
argument-hint: "<source> <target> [--max-depth 10]"
---

# Trace

Call `cqs_trace` MCP tool. Parse arguments:

- First positional arg = `source` — starting function name (required)
- Second positional arg = `target` — destination function name (required)
- `--max-depth <n>` → maximum BFS depth (default 10)

Returns the shortest call path from source to target with file/line/signature for each step. Saves 5-10 sequential file reads per code-flow question.
