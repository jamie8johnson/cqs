---
name: cqs-impact
description: What breaks if you change X? Callers with snippets + affected tests.
disable-model-invocation: false
argument-hint: "<function_name> [--depth 1]"
---

# Impact

Call `cqs_impact` MCP tool. Parse arguments:

- First positional arg = `name` — function to analyze (required)
- `--depth <n>` → caller depth (default 1 = direct callers only)

Returns callers with call-site snippets and affected tests via reverse BFS. Collapses ~5 tool calls (callers + reading each caller file + grepping tests) into 1.
